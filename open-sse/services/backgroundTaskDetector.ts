/**
 * Background Task Detector — Feature 3
 *
 * Detects when CLI tools send "background" requests (title generation,
 * summarization, short descriptions) and provides model degradation
 * recommendations to save premium model quota.
 *
 * Detection heuristics:
 * - System prompt patterns indicating background/utility tasks
 * - Very short conversations with summary-like system prompts
 * - X-Request-Priority header
 */

// ── Configuration ───────────────────────────────────────────────────────────

interface DegradationConfig {
  enabled: boolean;
  degradationMap: Record<string, string>; // original → cheaper model
  detectionPatterns: string[]; // regex patterns for system prompt matching
  stats: {
    detected: number;
    tokensSaved: number;
  };
}

const DEFAULT_DETECTION_PATTERNS = [
  // English
  "generate a title",
  "generate title",
  "create a title",
  "create a short",
  "summarize this",
  "summarize the",
  "write a brief",
  "write a summary",
  "one-line summary",
  "one line summary",
  "short description",
  "brief description",
  "conversation title",
  "chat title",
  "name this conversation",
  "name this chat",
  "title for this",
  "suggest a title",
  "label this",
  // Russian
  "придумай название",
  "придумай заголовок",
  "предложи название",
  "предложи заголовок",
  "создай название",
  "создай заголовок",
  "составь название",
  "составь заголовок",
  "сгенерируй название",
  "сгенерируй заголовок",
  "название для",
  "заголовок для",
  "название беседы",
  "название разговора",
  "назови разговор",
  "назови беседу",
  "краткое описание",
  "короткое описание",
  "кратко опиши",
  "коротко опиши",
  "опиши в двух словах",
  "в двух словах",
  "одной строкой",
  "в одну строку",
  "одним предложением",
  "краткое резюме",
  "сделай краткое",
  "краткий пересказ",
  "краткое содержание",
  "краткое изложение",
  "подведи итог",
  "подведи итоги",
  "подведи краткий итог",
  "краткий итог",
  "итог беседы",
  "суммаризируй",
  "суммаризация",
  "сделай саммари",
  "перефразируй кратко",
  "кратко",
  "покороче",
  // German
  "titel generieren",
  "titel für",
  "überschrift",
  "kurze beschreibung",
  "kurz zusammenfassen",
  "fasse zusammen",
  "konversation zusammen",
  "gespräch zusammen",
  "zusammenfassung",
  "in einem satz",
  "eine zeile",
  "betreff",
  // French
  "génère un titre",
  "titre pour",
  "résumé",
  "résume",
  "résumer",
  "courte description",
  "description courte",
  "en une phrase",
  "en une ligne",
  "objet",
];

const DEFAULT_DEGRADATION_MAP: Record<string, string> = {
  // Premium → Cheap alternatives
  "claude-opus-4-6": "gemini-3-flash",
  "claude-opus-4-6-thinking": "gemini-3-flash",
  "claude-opus-4-5-20251101": "gemini-3-flash",
  "claude-sonnet-4-5-20250929": "gemini-3-flash",
  "claude-sonnet-4-20250514": "gemini-3-flash",
  "claude-sonnet-4": "gemini-3-flash",
  "gemini-3.1-pro": "gemini-3-flash",
  "gemini-3.1-pro-high": "gemini-3-flash",
  "gemini-3-pro-preview": "gemini-3-flash-preview",
  "gemini-2.5-pro": "gemini-3-flash",
  "gpt-4o": "gpt-4o-mini",
  "gpt-5": "gpt-5-mini",
  "gpt-5.1": "gpt-5-mini",
  "gpt-5.1-codex": "gpt-5.1-codex-mini",
  // NVIDIA NIM (full combo ids)
  "nvidia/nvidia/nemotron-3-ultra-550b-a55b": "nvidia/deepseek-ai/deepseek-v4-flash",
  "nvidia/nvidia/nemotron-3-super-120b-a12b": "nvidia/deepseek-ai/deepseek-v4-flash",
  "nvidia/deepseek-ai/deepseek-v4-pro": "nvidia/deepseek-ai/deepseek-v4-flash",
  // Xiaomi MiMo (full combo id + alias form)
  "xiaomi-mimo-token-plan/mimo-v2.5-pro": "xiaomi-mimo-token-plan/mimo-v2.5",
  "mimotp/mimo-v2.5-pro": "mimotp/mimo-v2.5",
  // DeepSeek direct (alias forms)
  "if/deepseek-v4-pro": "if/deepseek-v4-flash",
  "ds/deepseek-v4-pro": "ds/deepseek-v4-flash",
};

// Tier fallback: when a model is not explicitly mapped but carries a known
// premium-model marker, degrade to a cheap sibling. Checked only after the exact
// map lookup misses, and only for requests already classified as background tasks.
const DEFAULT_TIER_FALLBACK: Array<{ pattern: string; degradedTo: string }> = [
  { pattern: "nemotron-3-ultra", degradedTo: "nvidia/deepseek-ai/deepseek-v4-flash" },
  { pattern: "nemotron-3-super", degradedTo: "nvidia/deepseek-ai/deepseek-v4-flash" },
  { pattern: "deepseek-v4-pro", degradedTo: "nvidia/deepseek-ai/deepseek-v4-flash" },
  { pattern: "mimo-v2.5-pro", degradedTo: "mimotp/mimo-v2.5" },
];

// Requests whose total context exceeds this estimated token budget are treated as
// real work, not utility tasks — never degrade them to a cheap model.
const CONTEXT_GUARD_TOKENS = 200_000;

// max_tokens below this threshold marks the request as a short/utility task
// (title generation, summaries) even without a matching system prompt pattern.
const LOW_MAX_TOKENS_THRESHOLD = 80;

// ── State ───────────────────────────────────────────────────────────────────

// Backed by globalThis so the singleton is shared across the SEPARATE webpack
// module graphs Next.js builds for `instrumentation.ts` (boot-time hydration via
// applyRuntimeSettings → setBackgroundDegradationConfig) and the app-route /
// open-sse executors (per-request reads in the chat handler). A module-local `let`
// is duplicated per graph, so the operator's opt-in (`enabled:true`) applied at boot
// never reaches the request path — the degradation silently never fires (the
// #5312-class module-graph bug). Mirrors systemPrompt.ts (#2470) and thinkingBudget.ts.
const GLOBAL_KEY = "__omniroute_backgroundDegradation_config__";
const _store = globalThis as unknown as Record<string, DegradationConfig | undefined>;

function getConfig(): DegradationConfig {
  if (!_store[GLOBAL_KEY]) {
    _store[GLOBAL_KEY] = {
      enabled: false, // Disabled by default — user must opt in
      degradationMap: { ...DEFAULT_DEGRADATION_MAP },
      detectionPatterns: [...DEFAULT_DETECTION_PATTERNS],
      stats: { detected: 0, tokensSaved: 0 },
    };
  }
  return _store[GLOBAL_KEY]!;
}

// ── Config Management ───────────────────────────────────────────────────────

/**
 * Set the background degradation config (called from settings API or startup).
 */
export function setBackgroundDegradationConfig(config: Partial<DegradationConfig>): void {
  _store[GLOBAL_KEY] = {
    ...getConfig(),
    ...config,
    stats: getConfig().stats, // preserve stats across config changes
  };
}

/**
 * Get current background degradation config.
 */
export function getBackgroundDegradationConfig(): DegradationConfig {
  return {
    ...getConfig(),
    degradationMap: { ...getConfig().degradationMap },
    detectionPatterns: [...getConfig().detectionPatterns],
    stats: { ...getConfig().stats },
  };
}

/**
 * Reset stats counters.
 */
export function resetStats(): void {
  getConfig().stats = { detected: 0, tokensSaved: 0 };
}

// ── Detection ───────────────────────────────────────────────────────────────

interface BackgroundMessage {
  role?: string;
  content?: unknown;
}

interface BackgroundTaskBody {
  messages?: BackgroundMessage[];
  input?: BackgroundMessage[];
  max_tokens?: unknown;
  max_completion_tokens?: unknown;
  max_output_tokens?: unknown;
}

function toMessageArray(value: unknown): BackgroundMessage[] {
  return Array.isArray(value) ? (value as BackgroundMessage[]) : [];
}

function toFiniteNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim().length > 0) {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function headerValue(headers: Record<string, string> | null, key: string): string {
  if (!headers) return "";
  const value = headers[key] ?? headers[key.toLowerCase()] ?? headers[key.toUpperCase()];
  return typeof value === "string" ? value.trim() : "";
}

/** Conservative token estimate over all message content (ASCII chars ≈ 0.25 token, else 1). */
function estimateContextTokens(messages: BackgroundMessage[]): number {
  let tokens = 0;
  for (const message of messages) {
    const content = message.content;
    const text =
      typeof content === "string"
        ? content
        : content === null || content === undefined
          ? ""
          : JSON.stringify(content);
    if (!text) continue;
    for (const character of text) {
      tokens += character.codePointAt(0)! < 0x80 ? 0.25 : 1;
      if (tokens >= CONTEXT_GUARD_TOKENS) return CONTEXT_GUARD_TOKENS;
    }
  }
  return tokens;
}

/**
 * Get reason label when request is a background/utility task.
 *
 * @param {object} body - Request body
 * @param {object} [headers] - Request headers (optional)
 * @returns {string | null} Reason label or null when not detected
 */
export function getBackgroundTaskReason(
  body: BackgroundTaskBody | unknown,
  headers: Record<string, string> | null = null
): string | null {
  if (!body || typeof body !== "object") return null;
  const typedBody = body as BackgroundTaskBody;

  // 1. Check explicit header
  if (headers) {
    const taskType = headerValue(headers, "x-task-type");
    const priority = headerValue(headers, "x-request-priority");
    const initiator = headerValue(headers, "x-initiator");
    const explicitValue = [taskType, priority, initiator].find(Boolean);
    if (explicitValue && explicitValue.toLowerCase() === "background") {
      return "header_background";
    }
  }

  // 2. Very large contexts are real work, never a utility task — do not degrade.
  const messages = toMessageArray(typedBody.messages ?? typedBody.input ?? []);
  if (Array.isArray(messages) && messages.length > 0) {
    if (estimateContextTokens(messages) >= CONTEXT_GUARD_TOKENS) return null;
  }

  // 3. Very low max tokens usually indicates utility/background tasks
  const maxTokens = toFiniteNumber(
    typedBody.max_tokens ?? typedBody.max_completion_tokens ?? typedBody.max_output_tokens
  );
  if (maxTokens !== null && maxTokens > 0 && maxTokens < LOW_MAX_TOKENS_THRESHOLD) {
    return "low_max_tokens";
  }

  // 4. Check system prompt for background task patterns
  if (!Array.isArray(messages) || messages.length === 0) return null;
  const systemMsg = messages.find(
    (message: BackgroundMessage) => message.role === "system" || message.role === "developer"
  );
  if (!systemMsg) return null;

  const systemContent =
    typeof systemMsg.content === "string" ? systemMsg.content.toLowerCase() : "";

  if (!systemContent) return null;

  // Check against detection patterns
  const matched = getConfig().detectionPatterns.some((pattern) =>
    systemContent.includes(pattern.toLowerCase())
  );

  if (!matched) return null;

  // 5. Additional heuristic: background tasks typically have very few messages
  // (system + 1-2 user messages)
  const userMessages = messages.filter((message: BackgroundMessage) => message.role === "user");
  if (userMessages.length > 3) return null; // Too many turns for a background task

  return "system_prompt_pattern";
}

/**
 * Check if a request is a background/utility task.
 *
 * @param {object} body - Request body
 * @param {object} [headers] - Request headers (optional)
 * @returns {boolean} True if the request looks like a background task
 */
export function isBackgroundTask(
  body: BackgroundTaskBody | unknown,
  headers: Record<string, string> | null = null
): boolean {
  return getBackgroundTaskReason(body, headers) !== null;
}

/**
 * Get the degraded (cheaper) model for a given model.
 *
 * @param {string} originalModel - The original model ID
 * @returns {string} The cheaper model or original if no mapping exists
 */
export function getDegradedModel(originalModel: string): string {
  if (!originalModel) return originalModel;

  const degraded = getConfig().degradationMap[originalModel];
  if (degraded) {
    getConfig().stats.detected++;
    return degraded;
  }

  // Tier fallback: known premium markers not explicitly mapped → cheap sibling.
  for (const rule of DEFAULT_TIER_FALLBACK) {
    if (originalModel.includes(rule.pattern)) {
      getConfig().stats.detected++;
      return rule.degradedTo;
    }
  }

  return originalModel;
}

/**
 * Get default degradation map (for UI reset).
 */
export function getDefaultDegradationMap(): Record<string, string> {
  return { ...DEFAULT_DEGRADATION_MAP };
}

/**
 * Get default detection patterns (for UI reset).
 */
export function getDefaultDetectionPatterns(): string[] {
  return [...DEFAULT_DETECTION_PATTERNS];
}
