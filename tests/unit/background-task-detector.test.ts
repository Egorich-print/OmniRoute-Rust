import test from "node:test";
import assert from "node:assert/strict";

const {
  isBackgroundTask,
  getBackgroundTaskReason,
  getDegradedModel,
  setBackgroundDegradationConfig,
  getBackgroundDegradationConfig,
  getDefaultDegradationMap,
  getDefaultDetectionPatterns,
  resetStats,
} = await import("../../open-sse/services/backgroundTaskDetector.ts");

// ─── isBackgroundTask ───────────────────────────────────────────────────────

test("isBackgroundTask: returns true for title generation pattern", () => {
  setBackgroundDegradationConfig({ enabled: true });
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "Generate a title for this conversation" },
      { role: "user", content: "How to deploy a Next.js app" },
    ],
  };
  assert.equal(isBackgroundTask(body), true);
});

test("isBackgroundTask: returns true for summarize pattern", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "Summarize this conversation briefly" },
      { role: "user", content: "We discussed deployment techniques" },
    ],
  };
  assert.equal(isBackgroundTask(body), true);
});

test("isBackgroundTask: returns false for normal chat", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "You are a helpful coding assistant" },
      { role: "user", content: "Help me write a function" },
    ],
  };
  assert.equal(isBackgroundTask(body), false);
});

test("isBackgroundTask: returns false for many-turn conversations", () => {
  const messages = [
    { role: "system", content: "Generate a title" },
    ...Array.from({ length: 10 }, (_, i) => ({
      role: i % 2 === 0 ? "user" : "assistant",
      content: `Message ${i}`,
    })),
  ];
  const body = { model: "claude-sonnet-4", messages };
  assert.equal(isBackgroundTask(body), false); // Too many turns
});

test("isBackgroundTask: detects X-Request-Priority header", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [{ role: "user", content: "hello" }],
  };
  const headers = { "x-request-priority": "background" };
  assert.equal(isBackgroundTask(body, headers), true);
});

test("isBackgroundTask: detects X-Task-Type header", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [{ role: "user", content: "hello" }],
  };
  const headers = { "x-task-type": "background" };
  assert.equal(isBackgroundTask(body, headers), true);
  assert.equal(getBackgroundTaskReason(body, headers), "header_background");
});

test("isBackgroundTask: detects low max_tokens requests", () => {
  const body = {
    model: "claude-sonnet-4",
    max_tokens: 32,
    messages: [{ role: "user", content: "hello" }],
  };
  assert.equal(isBackgroundTask(body), true);
  assert.equal(getBackgroundTaskReason(body), "low_max_tokens");
});

test("isBackgroundTask: returns false for null/undefined body", () => {
  assert.equal(isBackgroundTask(null), false);
  assert.equal(isBackgroundTask(undefined), false);
});

test("isBackgroundTask: returns false for empty messages", () => {
  assert.equal(isBackgroundTask({ messages: [] }), false);
});

// ─── getDegradedModel ───────────────────────────────────────────────────────

test("getDegradedModel: returns cheaper model from map", () => {
  resetStats();
  assert.equal(getDegradedModel("claude-opus-4-6"), "gemini-3-flash");
  assert.equal(getDegradedModel("gemini-2.5-pro"), "gemini-3-flash");
  assert.equal(getDegradedModel("gpt-4o"), "gpt-4o-mini");
});

test("getDegradedModel: returns original if no mapping exists", () => {
  assert.equal(getDegradedModel("some-unknown-model"), "some-unknown-model");
});

test("getDegradedModel: handles null/empty", () => {
  assert.equal(getDegradedModel(""), "");
  assert.equal(getDegradedModel(null), null);
});

test("getDegradedModel: increments stats counter", () => {
  resetStats();
  getDegradedModel("claude-opus-4-6"); // known mapping
  const config = getBackgroundDegradationConfig();
  assert.equal(config.stats.detected, 1);
});

test("isBackgroundTask: returns true for low max_tokens up to 80", () => {
  const body = {
    model: "claude-sonnet-4",
    max_tokens: 79,
    messages: [{ role: "user", content: "hello" }],
  };
  assert.equal(getBackgroundTaskReason(body), "low_max_tokens");
});

test("isBackgroundTask: returns false for max_tokens above threshold", () => {
  const body = {
    model: "claude-sonnet-4",
    max_tokens: 128,
    messages: [{ role: "user", content: "hello" }],
  };
  assert.equal(getBackgroundTaskReason(body), null);
});

test("isBackgroundTask: detects Russian title pattern", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "Придумай название для этого разговора" },
      { role: "user", content: "Обсуждали деплой" },
    ],
  };
  assert.equal(isBackgroundTask(body), true);
  assert.equal(getBackgroundTaskReason(body), "system_prompt_pattern");
});

test("isBackgroundTask: detects Russian summary pattern", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "Подведи краткий итог беседы" },
      { role: "user", content: "Мы говорили о коде" },
    ],
  };
  assert.equal(isBackgroundTask(body), true);
});

test("isBackgroundTask: detects German summary pattern", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "Fasse die Konversation zusammen" },
      { role: "user", content: "Hallo" },
    ],
  };
  assert.equal(isBackgroundTask(body), true);
});

test("isBackgroundTask: detects French title pattern", () => {
  const body = {
    model: "claude-sonnet-4",
    messages: [
      { role: "system", content: "Génère un titre pour cette conversation" },
      { role: "user", content: "Bonjour" },
    ],
  };
  assert.equal(isBackgroundTask(body), true);
});

test("isBackgroundTask: context guard blocks degradation for huge contexts", () => {
  const bigText = "a".repeat(200_000 * 4 + 100); // > 200k estimated tokens
  const body = {
    model: "claude-sonnet-4",
    max_tokens: 64,
    messages: [
      { role: "system", content: "Summarize this" },
      { role: "user", content: bigText },
    ],
  };
  assert.equal(getBackgroundTaskReason(body), null);
});

test("isBackgroundTask: explicit header still honored under context guard", () => {
  const bigText = "a".repeat(200_000 * 4 + 100);
  const body = {
    model: "claude-sonnet-4",
    messages: [{ role: "user", content: bigText }],
  };
  const headers = { "x-task-type": "background" };
  assert.equal(isBackgroundTask(body, headers), true);
});

test("getDegradedModel: tier fallback maps unlisted premium models", () => {
  resetStats();
  assert.equal(
    getDegradedModel("nvidia/nvidia/nemotron-3-ultra-550b-a55b"),
    "nvidia/deepseek-ai/deepseek-v4-flash"
  );
  assert.equal(
    getDegradedModel("nvidia/deepseek-ai/deepseek-v4-pro"),
    "nvidia/deepseek-ai/deepseek-v4-flash"
  );
  assert.equal(
    getDegradedModel("nvidia/nvidia/nemotron-3-super-120b-a12b"),
    "nvidia/deepseek-ai/deepseek-v4-flash"
  );
});

test("getDegradedModel: maps user's full combo model ids", () => {
  assert.equal(
    getDegradedModel("xiaomi-mimo-token-plan/mimo-v2.5-pro"),
    "xiaomi-mimo-token-plan/mimo-v2.5"
  );
  assert.equal(getDegradedModel("mimotp/mimo-v2.5-pro"), "mimotp/mimo-v2.5");
  assert.equal(getDegradedModel("if/deepseek-v4-pro"), "if/deepseek-v4-flash");
});

test("getDefaultDegradationMap: includes NVIDIA/MiMo entries", () => {
  const map = getDefaultDegradationMap();
  assert.ok(map["nvidia/deepseek-ai/deepseek-v4-pro"]);
  assert.ok(map["xiaomi-mimo-token-plan/mimo-v2.5-pro"]);
});

test("getDefaultDetectionPatterns: includes Russian and German patterns", () => {
  const patterns = getDefaultDetectionPatterns();
  assert.ok(patterns.includes("придумай название"));
  assert.ok(patterns.includes("fasse zusammen"));
  assert.ok(patterns.includes("génère un titre"));
});

// ─── Config Management ──────────────────────────────────────────────────────

test("getBackgroundDegradationConfig: returns config copy", () => {
  const config = getBackgroundDegradationConfig();
  assert.ok(typeof config.enabled === "boolean");
  assert.ok(typeof config.degradationMap === "object");
  assert.ok(Array.isArray(config.detectionPatterns));
});

test("setBackgroundDegradationConfig: updates config", () => {
  setBackgroundDegradationConfig({ enabled: true });
  assert.equal(getBackgroundDegradationConfig().enabled, true);
  setBackgroundDegradationConfig({ enabled: false }); // reset
});

test("getDefaultDegradationMap: returns non-empty map", () => {
  const map = getDefaultDegradationMap();
  assert.ok(Object.keys(map).length > 0);
  assert.ok(map["claude-opus-4-6"]);
});

test("getDefaultDetectionPatterns: returns non-empty array", () => {
  const patterns = getDefaultDetectionPatterns();
  assert.ok(patterns.length > 0);
  assert.ok(patterns.includes("generate a title"));
});
