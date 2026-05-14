# Anonymous Telemetry Implementation Plan

> **For agentic workers:** Tasks below correspond 1:1 with `submit_plan` task IDs. Spec: `docs/superpowers/specs/2026-05-14-anonymous-telemetry-design.md`. Follow TDD: write failing test → minimal impl → green → commit. Each task ends with a green `cargo test -p spur-telemetry` (or scoped equivalent) and a single commit.

**Goal:** Ship the `spur-telemetry` crate per spec v2, wire it into `spur-cli`/`spur-tui`/`spur-mcp`/`spur-acp` integration points, with end-to-end tests against a loopback PostHog mock.

**Architecture:** New `spur-telemetry` crate exposing typed events via a sealed `IntoProp` trait + `emit!` macro, two-tier consent stored in `~/.spur/telemetry.toml`, bounded-mpsc batch flush, panic hook with sync crash-file write + next-launch upload, `build.rs` compile-time disable when `SPUR_POSTHOG_KEY` is unset, runtime `SPUR_TELEMETRY=0` disable propagated via workspace `.cargo/config.toml` for tests.

**Tech stack:** Rust 2021, tokio, reqwest+rustls, serde/serde_json, toml, uuid, sha2, dirs, tracing, wiremock (dev-deps), tempfile (dev-deps).

---

## File map

```
crates/spur-telemetry/
├── Cargo.toml             # T1
├── build.rs               # T1
└── src/
    ├── lib.rs             # T11 (public API: init/shutdown/emit!/TelemetryGuard)
    ├── error.rs           # T1
    ├── events.rs          # T3 (IntoProp sealed trait + Tier enum + Event trait)
    ├── tier1_events.rs    # T12
    ├── tier2_events.rs    # T13
    ├── redact.rs          # T4
    ├── config.rs          # T5
    ├── consent.rs         # T6
    ├── ratelimit.rs       # T7
    ├── client.rs          # T8
    ├── batch.rs           # T9
    └── crash.rs           # T10
crates/spur-telemetry/tests/integration.rs    # T19
.cargo/config.toml          # T2
crates/spur-cli/src/cmd/telemetry.rs          # T14
crates/spur-cli/src/main.rs (mods)            # T15
crates/spur-mcp/src/... (call sites)          # T16
crates/spur-acp/src/... (call sites)          # T17
crates/spur-tui/src/... (call sites)          # T17
crates/spur-cli/src/llm_*.rs (call sites)     # T18
docs/PRIVACY.md                                # T20
README.md (section)                            # T20
```

---

## Task DAG summary

```
T1 (crate scaffold) ─┬─> T3 (events foundation) ─┬─> T7 (ratelimit) ─┐
T2 (.cargo/config) ──┤                            │                   │
                     ├─> T4 (redact) ─────────────┼─> T8 (client) ────┼─> T11 (lib.rs public API) ─┬─> T12 (Tier 1 events) ─┬─> T16/T17/T18 (call sites) ─┬─> T19 (integration tests)
                     ├─> T5 (config) ─────────────┘                   │                            └─> T13 (Tier 2 events) ─┘                              │
                     └─> T6 (consent) ────────────────> T9 (batch) ───┘                                                                                    └─> T20 (docs)
                                                       T10 (crash) ───┘
                                                                       └─> T14 (telemetry subcmd) ──┬─> T15 (main wiring) ─> T19 ─> T20
```

---

## Task 1 — Bootstrap `spur-telemetry` crate

**Files:**
- Create: `crates/spur-telemetry/Cargo.toml`
- Create: `crates/spur-telemetry/build.rs`
- Create: `crates/spur-telemetry/src/lib.rs` (skeleton)
- Create: `crates/spur-telemetry/src/error.rs`
- Modify: workspace `Cargo.toml` to add `crates/spur-telemetry` as a member

**Goal:** Empty crate compiles in workspace. `build.rs` emits `cargo:rustc-cfg=telemetry_disabled` when `SPUR_POSTHOG_KEY` is absent; emits `cargo:rerun-if-env-changed=SPUR_POSTHOG_KEY` and `cargo:rustc-check-cfg=cfg(telemetry_disabled)` unconditionally. `error.rs` defines `pub enum TelemetryError` + `pub type Result<T>`.

**Tests (T1 self-tests):**
- `cargo build -p spur-telemetry` succeeds with no `SPUR_POSTHOG_KEY`.
- `cargo build -p spur-telemetry` succeeds with `SPUR_POSTHOG_KEY=loopback`.
- `cargo check --workspace` still passes.

**Commit:** `feat(spur-telemetry): scaffold crate with build.rs gating`

---

## Task 2 — Workspace test gating via `.cargo/config.toml`

**Files:**
- Create or modify: `.cargo/config.toml`

**Goal:** Add `[env] SPUR_TELEMETRY = "0"` and `CI = "true"` so all `cargo test` runs in the workspace are runtime-disabled regardless of `cfg(test)` propagation. If the file exists, merge in additively.

**Tests:**
- `cargo test --workspace` runs (will still pass — telemetry isn't wired anywhere yet); env vars are visible via `std::env::var` inside any test.

**Commit:** `chore: workspace test env disables telemetry`

---

## Task 3 — `events.rs` foundation: sealed `IntoProp` + `Tier` + `Event`

**Files:**
- Create: `crates/spur-telemetry/src/events.rs`
- Modify: `crates/spur-telemetry/src/lib.rs` to `pub mod events;`

**Goal:** Implement the privacy-critical type system from spec §6.1.

Key items:
- `mod sealed { pub trait Sealed {} }`
- `pub trait IntoProp: sealed::Sealed { fn into_value(self) -> serde_json::Value; }`
- Impls only for: `bool`, `i32`, `i64`, `u32`, `u64`, `f64`, `&'static str`. **No impl for `String`, `&str`, `Path`, `PathBuf`, or `Cow`** — verify by a compile-fail trybuild test (see below).
- `pub enum Tier { One, Two }` (Copy + serializable)
- `pub trait Event { const NAME: &'static str; const TIER: Tier; fn into_props(self) -> Props; }`
- `pub type Props = std::collections::BTreeMap<&'static str, serde_json::Value>;`

**Tests:**
- Unit: `bool::into_value()` → `Value::Bool`; `42_i64.into_value()` → `Value::Number`.
- `trybuild` compile-fail test asserting `let _: &dyn IntoProp = &String::new();` does NOT compile.
- `trybuild` compile-fail asserting `PathBuf` does not impl `IntoProp`.

**Commit:** `feat(spur-telemetry): sealed IntoProp + Event trait`

---

## Task 4 — `redact.rs`: stack scrubbing, model bucketing, panic-type allowlist

**Files:**
- Create: `crates/spur-telemetry/src/redact.rs`
- Modify: `lib.rs` to `pub(crate) mod redact;`

**Goal:**
- `pub fn scrub_stack(raw: &str) -> String` — strips `/Users/<x>/`, `/home/<x>/`, `C:\Users\<x>\` prefixes; replaces anything before a `spur_` crate name with `<external>`; preserves crate::module::function::line tail.
- `pub fn bucket_model(name: &str) -> &'static str` — maps known public models to canonical strings; unknown maps to provider prefix (`anthropic_other`, `openai_other`, `google_other`, `local_other`, `other`).
- `pub fn classify_panic(msg: &str) -> PanicType` — allowlist enum with variants `Bounds`, `Unwrap`, `OptionUnwrap`, `ResultUnwrap`, `Assertion`, `Other`. Match against substrings (`"index out of bounds"`, `"called `Option::unwrap()`"`, `"called `Result::unwrap()`"`, `"assertion failed"`).
- `pub fn payload_hash(msg: &str, anonymous_id: &str) -> String` — SHA-256 of `msg + anonymous_id`, first 8 hex chars.

**Tests:**
- Golden table for `scrub_stack` covering macOS, Linux, Windows path forms (use raw strings; no platform gating — function is pure).
- `bucket_model("claude-opus-4-7")` → `"claude-opus-4-7"`; `bucket_model("claude-foo-bar")` → `"anthropic_other"`; `bucket_model("xyz")` → `"other"`.
- `classify_panic` for each variant + an unknown → `Other`.
- `payload_hash` is deterministic given the same inputs; differs when `anonymous_id` changes.

**Commit:** `feat(spur-telemetry): redact module (stack/model/panic)`

---

## Task 5 — `config.rs`: `telemetry.toml` schema + atomic I/O

**Files:**
- Create: `crates/spur-telemetry/src/config.rs`
- Modify: `lib.rs` to `pub(crate) mod config;`

**Goal:**
- `pub struct TelemetryConfig { pub version: u32, pub anonymous_id: uuid::Uuid, pub tier1_crash: bool, pub tier1_perf: bool, pub tier2_usage: bool, pub last_consent_prompt_at: Option<DateTime<Utc>> }` (use `chrono` or `time`; pick whichever is already in the workspace).
- `pub fn config_path() -> PathBuf` — returns `dirs::home_dir().join(".spur/telemetry.toml")`; if not available, falls back to `dirs::config_dir().join("spur/telemetry.toml")`.
- `pub fn load_or_default() -> TelemetryConfig` — reads the file; on parse error or missing file, logs `tracing::warn!` and returns defaults (tier1_crash=true, tier1_perf=true, tier2_usage=false, fresh UUID).
- `pub fn save_atomic(cfg: &TelemetryConfig) -> Result<()>` — writes to `telemetry.toml.tmp` then `rename`s. Creates parent dir if missing.
- `pub const SCHEMA_VERSION: u32 = 1;` — `load_or_default` rejects unknown versions (returns defaults + warns).

**Tests:**
- Roundtrip: build a config, save to temp dir (use `tempfile::tempdir()`), reload, compare equal.
- Corrupt-file test: write `"not valid toml"` to the path; `load_or_default` returns defaults without panicking; verify a warn was logged (use `tracing-test`).
- Unknown-version test: write `version = 99`; returns defaults + warns.

**Commit:** `feat(spur-telemetry): telemetry.toml config store`

---

## Task 6 — `consent.rs`: env vars + tier state aggregator

**Files:**
- Create: `crates/spur-telemetry/src/consent.rs`
- Modify: `lib.rs` to `pub(crate) mod consent;`

**Goal:**
- `pub struct Consent { pub crash: bool, pub perf: bool, pub usage: bool }`
- `pub fn resolve(cfg: &TelemetryConfig) -> Consent` — short-circuits all to `false` if `SPUR_TELEMETRY` is set to `"0"`/empty/unset-but-defaulted, or if `CI` is set to anything other than `"false"`/empty. Otherwise reads from `cfg`.
- `pub fn is_event_allowed(consent: &Consent, tier: Tier, kind: EventKind) -> bool` where `EventKind` discriminates `Crash | Perf | Usage` so we honor per-subtier disables (spec §3.4).

**Tests:**
- `SPUR_TELEMETRY=0` → all tiers off.
- `CI=true` → all tiers off.
- `CI=false` + cfg with crash=true, perf=true, usage=false → crash+perf on, usage off.
- Per-event: `is_event_allowed` correctly gates a `Tier::One/Crash` event when `crash=false` but `perf=true`.

**Commit:** `feat(spur-telemetry): consent resolver with env overrides`

---

## Task 7 — `ratelimit.rs`: Instant-based token bucket

**Files:**
- Create: `crates/spur-telemetry/src/ratelimit.rs`
- Modify: `lib.rs` to `pub(crate) mod ratelimit;`

**Goal:**
- `pub struct TokenBucket { capacity: u32, refill_per_sec: f64, tokens: AtomicU32, last_refill: Mutex<Instant> }`
- `pub fn new(capacity: u32, refill_per_sec: f64) -> Self` — initialized full.
- `pub fn try_acquire(&self) -> bool` — refills based on elapsed `Instant::now()`, returns true if a token was consumed.
- Constants: 500 events/min capacity; refill rate `500.0 / 60.0`.

**Tests:**
- Start full at 500; can acquire 500 times in a tight loop; 501st returns false.
- After sleeping ~120ms (≈1 token at 500/min refill), one more acquire succeeds.
- Time-mocking via injectable `clock: fn() -> Instant` parameter for deterministic tests.

**Commit:** `feat(spur-telemetry): token-bucket rate limiter`

---

## Task 8 — `client.rs`: PostHog HTTP client

**Files:**
- Create: `crates/spur-telemetry/src/client.rs`
- Modify: `lib.rs` to `pub(crate) mod client;`

**Goal:**
- `pub struct PosthogClient { http: reqwest::Client, endpoint: String, api_key: &'static str }`
- `pub fn new(endpoint: impl Into<String>) -> Self`
- `pub async fn send_batch(&self, events: &[PosthogEvent]) -> Result<()>` — POST to `{endpoint}/batch/` with body `{"api_key": "...", "batch": [...]}`. 2-second client timeout. Network failure returns `TelemetryError::Network` (caller drops the batch).
- `PosthogEvent` is a serializable struct: `{ event: &'static str, distinct_id: String, properties: serde_json::Value, timestamp: DateTime<Utc> }`.
- `pub(crate) const POSTHOG_KEY: &str = env!("SPUR_POSTHOG_KEY");` — guarded by `#[cfg(not(telemetry_disabled))]`. Under `cfg(telemetry_disabled)`, expose `pub(crate) const POSTHOG_KEY: &str = "";` and a no-op client.
- Default endpoint: `"https://us.i.posthog.com"`. Overridable by `SPUR_POSTHOG_ENDPOINT` env (for tests pointing at wiremock).

**Tests:**
- With `wiremock` running on `127.0.0.1:0`: client posts a batch with one event, mock asserts the body shape (`api_key` field present, `batch` is an array).
- Timeout: configure mock to delay 5s; `send_batch` returns `TelemetryError::Network` within ~2s.
- Build-time: under `cfg(telemetry_disabled)`, `POSTHOG_KEY` is `""` and no `reqwest::Client` is constructed.

**Commit:** `feat(spur-telemetry): PostHog HTTP client`

---

## Task 9 — `batch.rs`: bounded mpsc queue + flush task

**Files:**
- Create: `crates/spur-telemetry/src/batch.rs`
- Modify: `lib.rs` to `pub(crate) mod batch;`

**Goal:**
- `pub struct BatchSender { tx: mpsc::Sender<PosthogEvent>, dropped: Arc<AtomicU64> }`
- `pub fn spawn(client: PosthogClient, consent: Arc<Consent>, ratelimit: Arc<TokenBucket>) -> (BatchSender, JoinHandle<()>)`
- Channel: `tokio::sync::mpsc::channel(200)`.
- Background task: collects events; flushes when 50 events queued OR 10s elapsed; calls `client.send_batch(...)`.
- Per-source sampling: count `mcp_request_duration` and `acp_request_duration` per minute; if > 100 in the window, drop 9 of 10 for that source. Implement via a small per-name counter map inside the task.
- `pub fn try_send(&self, ev: PosthogEvent)` — non-blocking; on full channel, increments `dropped` and logs a single `warn!` per drop-window.
- `pub async fn shutdown(self, timeout: Duration)` — drops sender, awaits background task with the timeout. Anything still queued past the timeout is dropped; counter emitted via `tracing::info!`.

**Tests:**
- Push 50 events; mock server receives a single batch.
- Push 200 events fast; channel full → `dropped` counter increments; mock receives the first 50.
- Shutdown with timeout 250ms drains in-flight queue.
- Sampling: push 200 `mcp_request_duration` events; mock receives roughly 20 (1/10 sampling) + the first 100 unsampled = ~120 total. Allow ±10% tolerance.

**Commit:** `feat(spur-telemetry): bounded batch queue with sampling`

---

## Task 10 — `crash.rs`: panic hook + crash files

**Files:**
- Create: `crates/spur-telemetry/src/crash.rs`
- Modify: `lib.rs` to `pub(crate) mod crash;`

**Goal:**
- `pub fn install(anonymous_id: uuid::Uuid)` — guarded by `std::sync::Once`. Captures prior hook, sets a new one that:
  1. `std::panic::catch_unwind(|| write_crash_file(info, anonymous_id))` — if it panics, do nothing.
  2. Calls the previous hook with `info`.
- `fn write_crash_file(info: &PanicInfo, id: Uuid) -> std::io::Result<()>` — uses only `std::fs`, no tokio. Path: `~/.spur/crash-reports/<uuid>-<timestamp>.json`. Creates dir if missing. Writes a JSON object: `{ panic_type, payload_hash, sanitized_stack, crate, module, line }`. Stack from `std::backtrace::Backtrace::force_capture()` then `redact::scrub_stack`.
- `pub async fn upload_pending(client: &PosthogClient, anonymous_id: Uuid) -> usize` — scan `~/.spur/crash-reports/`; for each `*.json`: parse → wrap as `$exception` event → `client.send_batch(&[event])`. On success: `fs::remove_file`. On parse failure: `fs::remove_file` unconditionally (malformed = drop). Returns count uploaded.
- Crash dir must NOT be created when crash is disabled.

**Tests:**
- Subprocess test: spawn a child binary in `tests/` that calls `install()` then `panic!("test")`. Parent verifies a crash file appears with expected JSON shape after the child exits.
- `upload_pending` against wiremock: drop a synthetic crash file; verify it's posted and then deleted.
- Malformed crash file → silently deleted.
- Hook chains: install a sentinel hook first, then `crash::install`, then panic; sentinel still runs.

**Commit:** `feat(spur-telemetry): panic hook + crash files`

---

## Task 11 — `lib.rs` public API + `emit!` macro + `TelemetryGuard`

**Files:**
- Modify: `crates/spur-telemetry/src/lib.rs`

**Goal:**
- `pub struct InitConfig { ... }` — minimal; just version string for now.
- `pub struct TelemetryGuard { /* opaque */ }` — its `Drop` calls `shutdown_blocking()` with 250ms timeout as a safety net.
- `pub fn init(cfg: InitConfig) -> TelemetryGuard`:
  1. Load `TelemetryConfig` via `config::load_or_default()`.
  2. Resolve `Consent` via `consent::resolve(&cfg)`.
  3. If all tiers off: return a no-op guard (no client, no hook, no task).
  4. If `tier1_crash`: `crash::install(cfg.anonymous_id)`. Best-effort `tokio::spawn(crash::upload_pending(...))` after client is up.
  5. Build `PosthogClient`, `TokenBucket`, spawn batch task.
  6. Store sender + handle in a global `OnceLock<TelemetryState>`.
  7. Register SIGINT/SIGTERM handler via `tokio::signal::ctrl_c` that calls `shutdown()`.
- `pub fn shutdown(guard: TelemetryGuard)` — explicit, await batch task with 250ms.
- `pub fn emit<E: Event>(event: E)` — internal function: checks consent for `E::TIER` + event kind, runtime-disabled atomic, then constructs `PosthogEvent` and calls `try_send`.
- `#[macro_export] macro_rules! emit { ... }` — expands to nothing under `cfg(telemetry_disabled)`; expands to a single function call to the internal `emit` otherwise. The macro must wrap argument evaluation in a `if telemetry_active() { let __e = $event_expr; crate::emit(__e); }` pattern so arg construction is skipped when runtime-disabled.

**Tests:**
- `init()` with no telemetry env → returns no-op guard; `emit!(SessionStarted{...})` does nothing.
- `init()` with mock endpoint → `emit!` produces an HTTP POST observable by wiremock.
- `Drop` of `TelemetryGuard` flushes pending events.

**Commit:** `feat(spur-telemetry): public API + emit! macro`

---

## Task 12 — Tier 1 event structs

**Files:**
- Create: `crates/spur-telemetry/src/tier1_events.rs`
- Modify: `lib.rs` to `pub mod tier1_events;`

**Goal:** Define typed structs implementing `Event` with `TIER = One`, per spec §4:
- `SessionStarted { os, arch, spur_version, is_tui }`
- `Exception { panic_type, payload_hash, sanitized_stack, crate_, module, line }`
- `LlmRequestDuration { model_name, duration_ms, token_count_bucket, outcome }`
- `McpRequestDuration { duration_ms, outcome }`
- `AcpRequestDuration { duration_ms, outcome }`
- `TuiFrameSlow { duration_ms }`

All fields use `IntoProp`-eligible types or allowlisted enums (`ModelName`, `Outcome`, `PanicType`). Define those enums in this file with explicit `IntoProp` impls.

**Tests:**
- For each event: construct it, call `into_props()`, assert the resulting JSON has the expected keys and types.
- Compile-fail test: `Exception { sanitized_stack: PathBuf::from("/x") ... }` does NOT compile.

**Commit:** `feat(spur-telemetry): Tier 1 event structs`

---

## Task 13 — Tier 2 event structs

**Files:**
- Create: `crates/spur-telemetry/src/tier2_events.rs`
- Modify: `lib.rs` to `pub mod tier2_events;`

**Goal:** Per spec §5:
- `PlanCreated { task_count, brain_model: ModelName, duration_ms }`
- `WorkerDispatched { worker_model: ModelName, skill_used: SkillName, attempt_num }`
- `McpToolCalled { server_name: McpServerName, tool_name: McpToolName, outcome: Outcome }` — `McpServerName` is an allowlist enum with a `Custom(HashedShort)` variant; same for `McpToolName`.
- `ReviewCompleted { outcome: ReviewOutcome, iteration_count }`
- `TuiViewOpened { view_name: ViewName }`

**Tests:** struct → props → JSON shape for each. Verify `McpServerName::Custom` serializes to the hash, not the original name.

**Commit:** `feat(spur-telemetry): Tier 2 event structs`

---

## Task 14 — `spur-cli telemetry` subcommand

**Files:**
- Create: `crates/spur-cli/src/cmd/telemetry.rs`
- Modify: `crates/spur-cli/src/cmd/mod.rs` (or equivalent) to register the subcommand.

**Goal:** Per spec §3.3, implement `spur telemetry [status|enable|disable|reset-id|config|flush]`:
- `status`: load config, print `anonymous_id`, per-tier states, last flush time.
- `enable <crash|perf|usage|all>`: mutate config, atomic save.
- `disable <...>`: same; `disable crash` additionally prints a notice that existing crash files are not deleted.
- `reset-id`: replace `anonymous_id` with new UUID, atomic save, print new ID.
- `config`: TTY → interactive prompt; non-TTY → print current state and exit 0.
- `flush`: call `spur_telemetry::shutdown()` synchronously.

Uses the existing `clap` parser in `spur-cli` — follow the file conventions of neighboring subcommands.

**Tests:** integration test with `assert_cmd` invoking the binary; verify state transitions visible in `telemetry.toml`.

**Commit:** `feat(spur-cli): telemetry subcommand`

---

## Task 15 — Wire `init()`/`shutdown()`/panic hook in `spur-cli` main

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

**Goal:**
- After config + logging init, before any work: `let _telemetry = spur_telemetry::init(InitConfig { spur_version: env!("CARGO_PKG_VERSION") });`
- `_telemetry` is held to end of `main`; its `Drop` flushes.
- Register a `tokio::signal` handler that calls `spur_telemetry::shutdown(guard)` then re-raises.
- Emit `SessionStarted` immediately after init.
- First-run consent prompt: if `telemetry.toml` doesn't exist, run the interactive/non-TTY logic from spec §3.2 before any other CLI work.

**Tests:**
- `spur --help` runs (no panic) — basic smoke.
- `assert_cmd` integration test: invoke `spur` with `SPUR_TELEMETRY=0` → no `telemetry.toml` created.
- Invoke `spur` with `HOME` pointing to a tempdir and a mock server → `telemetry.toml` is created with defaults, `SessionStarted` event posted.

**Commit:** `feat(spur-cli): wire telemetry in main`

---

## Task 16 — Integration points: `spur-mcp` (perf + tool_called)

**Files:**
- Modify: `crates/spur-mcp/src/...` (tool dispatch site — locate via `grep -rn "fn dispatch\|fn call_tool" crates/spur-mcp/src/`)

**Goal:**
- Wrap tool dispatch with `Instant::now()` … `emit!(McpRequestDuration { duration_ms, outcome })` (Tier 1).
- After dispatch, emit `McpToolCalled { server_name, tool_name, outcome }` (Tier 2).
- `server_name` and `tool_name` go through `redact::classify_server` / `classify_tool` (extend `redact.rs` with these helpers — add a follow-up commit to T4 if needed).

**Tests:** unit test the wrapper logic against a stub dispatcher.

**Commit:** `feat(spur-mcp): telemetry around tool dispatch`

---

## Task 17 — Integration points: `spur-acp` + `spur-tui`

**Files:**
- Modify: `crates/spur-acp/src/...` (request handlers)
- Modify: `crates/spur-tui/src/...` (frame loop + view switcher)

**Goal:**
- `spur-acp`: emit `AcpRequestDuration` around each request handler.
- `spur-tui`: emit `TuiFrameSlow` when a frame exceeds 33ms; emit `TuiViewOpened` on view transition.

**Tests:** lightweight unit tests; the heavy verification is the integration test in T19.

**Commit:** `feat(spur-acp,spur-tui): telemetry call sites`

---

## Task 18 — Integration points: LLM + plan/worker/review

**Files:**
- Modify: LLM client wrapper(s) in `spur-cli` / `spur-core` (locate via `grep -rn "anthropic\|openai" crates/spur-cli/src crates/spur-core/src`)
- Modify: plan submission / worker dispatch / review-result handlers (likely in `spur-mcp` plan-related modules or `spur-cli` brain loop)

**Goal:**
- `LlmRequestDuration` around each model call.
- `PlanCreated` on successful `submit_plan`.
- `WorkerDispatched` on each worker dispatch.
- `ReviewCompleted` on review result.

**Tests:** unit tests against stub callbacks.

**Commit:** `feat: telemetry on LLM + plan/worker/review boundaries`

---

## Task 19 — Integration tests (wiremock + panic roundtrip)

**Files:**
- Create: `crates/spur-telemetry/tests/integration.rs`
- Create: `crates/spur-telemetry/tests/fixtures/` (panic test child binary)

**Goal:** Acceptance criteria from spec §15:
1. **Mock-server contract**: build with `SPUR_POSTHOG_KEY=loopback`, run a workload that emits each event type, assert every captured event matches an allowed schema. Run again with key unset — mock receives zero requests.
2. **Panic roundtrip**: child binary panics; parent next-launch uploads + deletes the file. Verify via filesystem assertions + mock receipts.
3. **Consent gating**: set `tier2_usage=false` in `telemetry.toml`; verify mock sees no Tier 2 events; flip to true; verify they appear.
4. **`disable crash` semantics**: set `tier1_crash=false`; verify panic hook is NOT installed (no crash file written when panic occurs).
5. **Rate limit**: emit 600 events in 1s; verify mock receives ≤500 and `dropped` counter > 0.
6. **Network failure**: configure mock to 500 the first batch; verify telemetry recovers on the next batch (does not abort, does not retry the dropped batch).

**Commit:** `test(spur-telemetry): wiremock + panic roundtrip integration`

---

## Task 20 — Documentation

**Files:**
- Create: `docs/PRIVACY.md`
- Modify: `README.md` (add telemetry section)
- Verify: `spur telemetry --help` output renders correctly (T14)

**Goal:**
- `docs/PRIVACY.md`: spell out what is collected, per tier, with examples; document `SPUR_TELEMETRY=0`, `spur telemetry disable all`, retention policy (90 days), how to request deletion using the anonymous ID, pseudonymity disclosure.
- README section: 4-6 lines linking to PRIVACY.md and showing the one-line disable.

**Tests:** `markdownlint` if the project uses it; otherwise visual review.

**Commit:** `docs: telemetry privacy + README section`

---

## Self-review checklist (run before submit_plan)

- Every spec §4/§5 event has a corresponding Tier 1/Tier 2 struct in T12/T13. ✓
- Every integration point in spec §10 has a wiring task in T15/T16/T17/T18. ✓
- All seven acceptance criteria in spec §15 are tested in T19 + smoke from T14/T15. ✓
- No "TBD" or "fill in" placeholders. ✓
- Function names used in later tasks (e.g., `scrub_stack`, `bucket_model`, `classify_panic`, `payload_hash`) match across all tasks. ✓
- T4 needs follow-up extension for `classify_server`/`classify_tool` consumed by T16 — flagged in T16 description. ✓
