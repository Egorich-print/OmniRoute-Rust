# OmniRoute → Rust: full rewrite plan

> Baseline: `release/v3.8.51` @ `6f26cf27b1` (Egorich-print fork).
> Status: planning artifact. No production code is ported yet.
> Push policy: **fork only** — see §13.

---

## 1. Verdict and scope

The rewrite is a **multi-quarter, staged migration**, not a big-bang port. The
current TypeScript tree is ~1.9M LOC across the server, API, UI, CLI and tests;
a single-pass rewrite would freeze the product and lose the behaviour pinned by
5,810 test files.

The only viable shape is a **strangler fig**: run the Rust gateway next to the
Node server, cut traffic over provider-by-provider and endpoint-by-endpoint, and
keep the JS build shipping until the last consumer is migrated.

This plan defines the target workspace, the porting order, the parity harness
that makes each step verifiable, and what to deliberately drop.

---

## 2. Baseline metrics (measured, not estimated)

### Server / data plane

| Area                          | Files |     LOC |
| ----------------------------- | ----: | ------: |
| `open-sse/services/`          |   565 | 131,412 |
| `open-sse/executors/`         |   199 |  70,794 |
| `open-sse/handlers/`          |   174 |  43,739 |
| `open-sse/utils/`             |   134 |  33,044 |
| `open-sse/config/`            |   350 |  25,681 |
| `open-sse/translator/`        |    59 |  18,926 |
| `open-sse/vendor/`            |    46 |  18,156 |
| `open-sse/mcp-server/`        |    45 |  10,550 |
| `src/lib/db/`                 |   145 |  45,976 |
| `src/lib/usage/`              |   ~45 |  15,239 |
| `src/app/api/**` (700 routes) |   700 |  72,745 |
| `bin/cli/`                    |   166 |  30,431 |

### Provider layer

- **277 registered providers**, ~2,100 static model declarations, 69 distinct
  executor ids (149 entries fall back to `default`).
- **199 executor files**, 80 `class *Executor`, 135 aliases, lazy-loaded.
- Auth types: apikey 189 · oauth 22 · optional 10 · none 10 · cookie 1.
- Formats: openai 206 · claude 15 · openai-responses 8 · gemini 4 · clova 2 ·
  antigravity 2 · kiro/cursor/custom/aihorde/segmind/magnific 1 each.

### Translation matrix

- 12 request pairs (7,192 LOC) and 13 response pairs (5,598 LOC), hub-and-spoke
  through OpenAI; helpers add 3,783 LOC.
- Stateful streaming conversion for `openai-responses`, `claude`, `gemini`,
  `antigravity`, `kiro`, `cursor`, `clova`.

### Persistence / config

- 176 migrations, ~127 tables, 4 driver fallbacks
  (`bun:sqlite` → `better-sqlite3` → `node:sqlite` → `sql.js`).
- 819 distinct variables in `.env.example` (3,225 lines); 699 unique
  `process.env.*` names across the repo.
- ~30 background schedulers (quota, catalog, credential health, cleanup,
  VACUUM, WAL checkpoints, spend flush, jobs, websockets).

### Desktop / build / CI

- Electron shell: 17 source files, 2,582 LOC, 24 native responsibilities.
- Build: Next standalone → CLI dist → Electron staging → electron-builder.
- CI: 26 workflows, 82 jobs; 19 Electron-specific test files.

### Tests

- 5,810 test files, ~960k LOC; c8 floor 60/60/60/60; mutation ratchets for 8
  critical modules; 14 golden snapshots; 21+ `*contract*` suites.

### Frontend

- 145 pages (120 dashboard), 800 `.tsx` components, 190.8k LOC `.tsx`.
- No React Query/SWR; Zustand (4 stores), `useEffect` + polling, 2 SSE streams,
  1 WebSocket.
- 66 locales, 67 MB of message catalogs, ~13,077 leaf keys in `en.json`.
- Heavy widgets: `@xyflow/react` (17 files), recharts (8), Monaco, dnd-kit,
  fumadocs, `@lobehub/icons` (React-only).

---

## 3. Target architecture

Single Cargo workspace at the repo root, plus the desktop app and the Svelte UI.

```
Cargo.toml                     # workspace, edition 2024, rust-version 1.98
rust-toolchain.toml            # 1.98.0 + rustfmt + clippy
crates/
  omniroute-core/              # domain types, WireFormat, errors          (exists)
  omniroute-db/                # rusqlite pool, migrations, repositories
  omniroute-config/            # env + settings + hot reload
  omniroute-translate/         # request/response translators (hub-spoke)
  omniroute-providers/         # registry data + executor traits
    registry/                  # providers, models, aliases
    http/                      # plain API-key executors
    oauth/                     # OAuth executors + token refresh
    web/                       # cookie/web-session executors
  omniroute-routing/           # combos, strategies, account selection, fallback, breakers
  omniroute-usage/             # accounting, pricing, budgets, call logs
  omniroute-gateway/           # axum surface, SSE, auth, /v1/*             (seed exists)
  omniroute-services/          # compression, memory, guardrails, search, skills
  omniroute-mcp/               # MCP server (rmcp)
  omniroute-cli/               # CLI
desktop/
  src-tauri/                   # Tauri v2 shell                           (seed exists)
  ui/                          # SvelteKit + Svelte 5                     (seed exists)
rust-rewrite/
  PLAN.md                      # this document
  PARITY.md                    # fixture inventory + acceptance matrix
```

### Key design decisions

1. **Explicit HTTP client, no global patch.** The Node code patches
   `globalThis.fetch` (`open-sse/utils/proxyFetch.ts`) and reads proxy/TLS/
   capture state from `AsyncLocalStorage`. Rust gets an `HttpClient` trait
   threaded through a per-request `RequestContext` — proxy selection, TLS
   profile, and capture sinks are plain struct fields.
2. **Request context instead of ambient state.** `chatCore.ts` (6,142 LOC) and
   its 95 extracted modules carry mutable closures. Rust models this as an
   explicit `ChatRequest`/`ChatOutcome` state machine.
3. **One SQLite driver.** `rusqlite` (bundled) replaces the 4-way adapter
   cascade; migrations become a `refinery`-style runner with the same file
   names/versions so an existing `storage.sqlite` migrates in place.
4. **Registry as data, executors as code.** Provider metadata becomes a static
   Rust table (or embedded TOML/JSON validated at build time); executors are
   trait objects behind `Arc<dyn Executor>`.
5. **SSE via `async-stream` + `axum::response::sse`.** Heartbeat/keepalive and
   the multi-watchdog semantics are re-expressed with `tokio::time` and
   `tokio_util::sync::CancellationToken`.

---

## 4. Migration strategy

**Dual-run cutover.** Both servers share one `storage.sqlite` (Rust uses
`rusqlite` WAL writers; the Node process keeps running for surfaces not yet
ported). A reverse proxy with a routing table decides per path/provider which
runtime serves a request.

Cutover unit = _provider family + endpoint_, never a half-port of a single
provider. Each cutover is gated by the parity harness (§8) and can be reverted
by flipping one proxy rule.

```
client → edge proxy ──┬─→ Node (Next.js)         [default until parity]
                      └─→ omniroute-gateway (Rust) [only for cut-over scope]
```

Rules:

- Never migrate data destructively; migrations are additive until Phase 9.
- Never cut over a provider without its golden translate-path + streaming test.
- Keep the Electron/Next app installable throughout.

---

## 5. Phases

### Phase 0 — Foundation and guardrails (2–4 weeks)

- [x] Cargo workspace, `rust-toolchain.toml` (1.98.0), edition 2024.
- [x] `omniroute-core` types + `omniroute-gateway` axum seed (`/healthz`,
      `/v1/models`, `/v1/chat/completions` with SSE, `ChatBackend` trait).
- [x] Tauri v2 + Svelte shell seed (`desktop/`).
- [ ] Port the parity harness: golden JSON fixtures + `insta` + `wiremock`.
- [ ] Rust CI lane: `cargo fmt`, `clippy -D warnings`, `nextest`,
      `cargo-deny`, `cargo-llvm-cov --fail-under-lines 60`.
- [ ] Embed the provider registry as data and snapshot it against
      `tests/snapshots/provider/translate-path.json` (277 rows).

**Exit:** CI runs Rust + JS independently; registry snapshot matches JS output.

### Phase 1 — Data plane core (6–10 weeks)

- [ ] `omniroute-db`: `rusqlite` pool, migration runner, repositories for
      connections/keys/combos/settings/usage.
- [ ] `omniroute-config`: typed settings over `key_value` namespaces, env
      parsing (consolidate the 819-var surface into documented groups), hot
      reload via a revision counter.
- [ ] `omniroute-usage`: usage/cost writes, 4-layer pricing resolution,
      buffered spend writer.
- [ ] `omniroute-gateway`: auth (API keys), `/v1/models`, non-streaming
      `/v1/chat/completions` for `default`-executor providers.
- [ ] Migrate an existing `storage.sqlite` unchanged; add a round-trip test.

**Exit:** a real API-key request for a plain HTTP provider returns a correct
non-streaming answer from Rust, with usage rows written identically.

### Phase 2 — Translation layer (4–6 weeks)

- [ ] Port `omniroute-translate` pair-by-pair, in this order:
      openai↔openai-responses → openai↔claude → openai↔gemini/antigravity →
      claude↔gemini → gemini/antigravity/kiro/cursor/clova → openai.
- [ ] Reuse `tests/fixtures/translation/*.json` and
      `tests/snapshots/translation/**` verbatim as `insta` snapshots.
- [ ] Port pre-normalisation (`ensureToolCallIds`, orphan tool-result repair,
      role normalisation, reasoning replay) and the stateful stream converters.

**Exit:** translation golden tests byte-match the JS snapshots (normalised IDs).

### Phase 3 — Provider executors in waves (12–20 weeks)

| Wave | Scope                               | Notes                                                      |
| ---- | ----------------------------------- | ---------------------------------------------------------- |
| 3a   | plain API-key HTTP providers (~190) | `HttpClient` trait, URL/header/body builders, retries      |
| 3b   | OAuth providers (22)                | token refresh, rotation maps, CAS guards, circuit breakers |
| 3c   | web-session/cookie providers (~20)  | cookie/session pools, fingerprint identity, health         |
| 3d   | CLI-wrapping executors              | spawned sidecars, stdio JSON-RPC, process-tree kill        |
| 3e   | browser/anti-bot + TLS-fingerprint  | Chromium sidecar; `wreq`/`rquest` for impersonation        |

**Exit per wave:** golden `provider-translate-path` + live smoke per provider;
no regression in the JS suite for that family.

### Phase 4 — Routing, failover, quotas (3–5 weeks)

- [ ] Combos + strategies (priority, weighted, round-robin, p2c, fill-first,
      cost/quota/context-aware, fusion, pipeline).
- [ ] Account selection/rotation/deck, semaphores, lockouts.
- [ ] Circuit breakers, quota preflight, rate limiters, request dedup.
- [ ] Port `combo-matrix` integration tests and the `g13` golden.

**Exit:** strategy matrix produces identical ordered plans to the JS harness.

### Phase 5 — Streaming parity (3–5 weeks)

- [ ] SSE pipeline: parse → translate per event → re-emit, with usage capture.
- [ ] Early keepalive, mid-stream heartbeat, idle watchdog, disconnect grace,
      stream recovery, cancellation propagation.
- [ ] Port `stream-utils`, `sse-auth`, `sse-chunk-sequences.json` fixtures.

**Exit:** chunk-for-chunk parity on the recorded SSE fixtures; heartbeat and
cancellation behaviour matches under fault injection.

### Phase 6 — Ancillary services (6–12 weeks)

Compression (deterministic engines first: lite, session-dedup, headroom, ccr,
RTK, caveman), memory (SQLite FTS5 + sqlite-vec backend), guardrails
(PII/injection/credential masking), search/fetch gateway, MCP server (`rmcp`),
skills registry. Defer the rest (§7).

### Phase 7 — Desktop shell: Electron → Tauri v2 (3–5 weeks)

- [ ] Port the 24 shell responsibilities; use `tauri-plugin-{shell,updater,
    single-instance,autostart,notification,log,os,dialog,opener}`.
- [ ] Replace the spawned Node server with the Rust gateway binary as a sidecar.
- [ ] Rebuild the updater feed (`latest.json` + minisign) and signing pipeline.
- [ ] Process-tree kill, data-dir migration, `--headless/--cli/--hidden` argv.

**Blockers to resolve first:** no `ELECTRON_RUN_AS_NODE` equivalent; the web-
cookie `loginManager` flow (partitioned cookie jars + `webRequest` header
capture) has **no Tauri v2 equivalent** — redesign or drop.

### Phase 8 — Frontend: Next/React → SvelteKit (10–16 weeks)

- [ ] Foundation: SvelteKit shell, Tailwind v4 tokens, theme store, API client,
      CSRF/base-path equivalent, Paraglide i18n (compile-time, tree-shaken —
      the 66-locale/67 MB catalog must not ship per request).
- [ ] Design system from `src/shared/components` (~103 primitives) using
      `bits-ui`.
- [ ] Module order: read-only pages → settings (13 routes) → providers (25.3k)
      → combos + flow studios → usage/analytics → tools → long tail.
- [ ] Toolkit mapping: `@xyflow/svelte`, LayerChart/echarts, Monaco wrapper,
      `svelte-dnd-action`, `marked`+DOMPurify, `lucide-svelte`,
      `@testing-library/svelte`; keep Playwright e2e untouched as the net.

### Phase 9 — CLI, packaging, decommission (4–8 weeks)

- [ ] Port the CLI last (30k LOC, 88+32 commands); until then the Node CLI is a
      thin client of the Rust server.
- [ ] Rewrite packaging (npm pack policy, Docker, desktop artifacts).
- [ ] Delete `open-sse/`, `src/` server code, Electron, and the JS test lanes
      once the proxy routes nothing to Node.

---

## 6. Subsystem porting notes

### 6.1 Transport (highest-risk refactor)

Node patches `globalThis.fetch` at module import and reads `AsyncLocalStorage`
for proxy, TLS fingerprint and capture context (`proxyFetch.ts`, 1,274 LOC;
`tlsClientBase.ts`, 873; `tlsClient.ts`, 1,045). ~200 executor files call the
ambient `fetch`. Rust: one `HttpClient` impl behind a trait, constructed per
request from `RequestContext`; TLS impersonation moves to `wreq`/`rquest`
(the current `wreq-js` binding is already Rust — this is an opportunity, not a
cost).

### 6.2 Provider executors

Bulk of the port (70.8k LOC). Group and port by wave (§5, Phase 3). The
`BaseExecutor` contract (multi-URL failover, header merging, credential refresh
persistence) becomes a Rust `Executor` trait plus a `DefaultExecutor`.

### 6.3 Translators

Pure logic, stateful for streams — port nearly 1:1 and pin with the existing
golden fixtures. Do not "improve" semantics during the port.

### 6.4 Persistence

`rusqlite` + migration runner keyed on the existing `_omniroute_migrations`
ledger so an existing DB upgrades in place. Preserve WAL checkpoints, scheduled
VACUUM, integrity check and backup/retention semantics — these are load-bearing
for corruption recovery.

### 6.5 Account/session state

In JS these are process-wide `Map`s with bespoke TTL sweeps (breakers, rotation,
lockouts, dedup, session pools). In Rust each becomes an explicit shared struct
(`DashMap`/`Arc<Mutex<...>>`) owned by the runtime, with timers as `tokio`
tasks. Decide per state whether single-process or Redis-backed.

### 6.6 Desktop shell

Enumerated in Phase 7. The two genuinely hard items are the login/cookie flow
and the updater feed/signing change; both need a product decision, not just a
port.

---

## 7. Deliberately dropped / deferred

| Item                                                                | Decision                                             |
| ------------------------------------------------------------------- | ---------------------------------------------------- |
| `sql.js` WASM, `node:sqlite`, Bun adapters, Windows driver probe    | Drop — one `rusqlite` driver                         |
| Legacy `db.json` + XOR encryption migration                         | One-time import tool, then drop                      |
| Migration rename/supersede compatibility shims                      | Drop after the import tool                           |
| MITM tproxy native addon (8.5k LOC)                                 | Defer; revisit only if interception is needed        |
| Gamification (2k LOC)                                               | Defer/drop unless the community feature is strategic |
| Multimodal vision/audio/video bridges                               | Defer                                                |
| LLMLingua/ONNX compression, OmniGlyph, eval harness, language packs | Defer                                                |
| Qdrant/Obsidian/HTTP memory backends, ONNX embeddings               | Defer behind a trait                                 |
| `fumadocs` docs site, `lucide-react` (dead dep)                     | Replace / drop                                       |
| i18n gates, ESLint/knip/jscpd/type-coverage ratchets                | Replace with cargo equivalents                       |

---

## 8. Parity and testing strategy

**Reuse the golden JSON fixtures verbatim.** They are the cheapest possible
guarantee that behaviour did not drift:

| Fixture                                                                     | Ports to                                   |
| --------------------------------------------------------------------------- | ------------------------------------------ |
| `tests/snapshots/provider/translate-path.json` (277 rows)                   | registry URL/header/format contract test   |
| `tests/snapshots/executors/executor-map.json` (135 aliases)                 | executor dispatch contract test            |
| `tests/snapshots/translation/**` (13) + `tests/fixtures/translation/*.json` | `insta` translation snapshots              |
| `tests/snapshots/g13/combo-chatcore-public-seams.json`                      | table-driven routing golden test           |
| `tests/integration/combo-matrix/*`                                          | integration tests with `wiremock` upstream |
| `tests/integration/provider-journey.contract.test.ts`                       | end-to-end contract test                   |
| `tests/golden-set/data/prompts.jsonl` + compression budget (2%)             | `insta` + `criterion` baseline             |

**Harness:** `nextest` profiles (unit/integration/serial), `insta`, `wiremock`,
`tempfile` with explicit `Paths`/`Config` injection (never global env),
`proptest`, `cargo-llvm-cov` (60% floor + per-module ratchets), `cargo-mutants`
for the 8 critical modules, `cargo-deny`, `criterion`.

**Keep three process policies** even in Rust: test-masking guard, "source change
requires test change", and test-discovery verification.

---

## 9. CI / build / packaging changes

- Add Rust lanes: fmt/clippy/nextest/coverage/mutants/deny + `cargo-tauri` build.
- Replace `electron-release.yml` with a `tauri-action` pipeline + `latest.json`
  - signing/notarisation (new secrets).
- Keep the Node CI lane green until the last cutover; then retire the
  Electron-specific jobs (19 test files, packaging steps, quality baselines).
- Rebaseline quality config that names `electron/`: complexity baseline,
  eslint suppressions, dependency allowlist, dead-code and deps checks.
- Add `rust` to CodeQL; add Semgrep Rust rules.

---

## 10. Risks

| #   | Risk                              | Impact                 | Mitigation                                                         |
| --- | --------------------------------- | ---------------------- | ------------------------------------------------------------------ |
| 1   | Ambient `fetch` + ALS context     | everything             | explicit `HttpClient` trait + `RequestContext` (Phase 0/1)         |
| 2   | ~150 executors, bespoke protocols | schedule               | wave-based port; registry as data; treat each family independently |
| 3   | Browser/anti-bot + cookie login   | feature loss           | Chromium sidecar; product decision for loginManager                |
| 4   | Streaming state machines          | silent client breakage | fixture parity before any cutover                                  |
| 5   | Account/session pool semantics    | flaky routing          | port decision tables + serial timing tests                         |
| 6   | `chatCore` god-function           | correctness            | redesign around explicit request state, not 1:1 port               |
| 7   | Dual-runtime DB coherence         | data corruption        | single writer per table during migration; additive migrations      |
| 8   | Tauri updater/signing             | no auto-update         | budget signing work early in Phase 7                               |
| 9   | Svelte i18n redesign (66 locales) | bundle size            | compile-time Paraglide, per-namespace loading                      |
| 10  | Scope creep into upstream         | PR rejection           | fork-only policy (§13)                                             |

---

## 11. Effort estimate

Rough order of magnitude, assuming one senior engineer with AI assistance:

| Phase           | Effort            |
| --------------- | ----------------- |
| 0 Foundation    | 2–4 weeks         |
| 1 Data plane    | 6–10 weeks        |
| 2 Translators   | 4–6 weeks         |
| 3 Executors     | 12–20 weeks       |
| 4 Routing       | 3–5 weeks         |
| 5 Streaming     | 3–5 weeks         |
| 6 Services      | 6–12 weeks        |
| 7 Desktop       | 3–5 weeks         |
| 8 Frontend      | 10–16 weeks       |
| 9 CLI/packaging | 4–8 weeks         |
| **Total**       | **~14–24 months** |

A useful checkpoint is **Phase 1 + 2 + one executor wave**: that already yields
a Rust gateway serving real traffic for plain API providers, which is the first
point where the rewrite pays for itself.

---

## 12. Immediate next actions

1. Land Phase 0 parity harness on a fork branch (fixtures + `insta` + `wiremock`).
2. Add the Rust CI lane (fmt/clippy/nextest/deny/coverage).
3. Embed the provider registry as data and match the 277-row snapshot.
4. Rebase `feat/rust-gateway-tauri-shell` onto the current `release/v3.8.51`
   tip and continue from there.

---

## 13. Push policy (fork only)

All Rust-rewrite work is pushed **only** to the user's fork:

- `origin` / `fork` = `github.com/Egorich-print/OmniRoute-Rust` ✅ allowed.
- `upstream` = `github.com/diegosouzapw/OmniRoute` ❌ never pushed by this work.
- Work happens on dedicated branches (e.g. `feat/rust-gateway-tauri-shell`,
  `fix/i18n-ru-spinner`, `plan/rust-rewrite`), never on the shared PR branch
  `release/v3.8.51` unless the change is a fix explicitly intended for a PR.
- A design discussion with the upstream maintainer precedes any upstream PR for
  the desktop/gateway work (per his request on PR #12388).
