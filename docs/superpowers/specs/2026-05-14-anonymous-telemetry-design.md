# Anonymous Telemetry — Design Spec (v2)

**Status:** Approved for planning
**Date:** 2026-05-14 (v2 incorporates dual-gate review from codex + kimi)
**Owner:** Kevin Truong
**Crate:** `spur-telemetry` (new)
**Backend:** PostHog (org "Plango", project 366596)

---

## 1. Goals

Collect enough signal from SPUR installs in the wild to:

1. **A. Crash & error visibility** — know when SPUR panics, with sanitized stacks sufficient to drive fixes.
2. **B. Performance telemetry** — observe LLM, ACP, MCP, and TUI latency in real-world conditions.
3. **C. Feature/usage patterns** — measure the shape of the agent loop (plans, dispatches, MCP tool hit rates, review outcomes) to prioritize work.

Non-goals: replacing structured logs, replacing distributed tracing, collecting any user content (prompts, file contents, repo names, branch names, commit messages, tool arguments, model outputs).

## 2. Privacy posture

- **Anonymous-identifier-based** telemetry. A persistent UUIDv4 lives in a dedicated config file (see §3.1); the user can view it (`spur telemetry status`), rotate it (`spur telemetry reset-id`), and use it for deletion requests. Under GDPR this is **pseudonymous, not anonymous**; the spec uses "anonymous-identifier-based" as the user-facing term and is honest about pseudonymity in legal docs.
- **No user content ever leaves the box.** Redaction is enforced at the type level via a sealed `IntoProp` trait (§6.1).
- **Two-tier consent** with distinct lawful bases:
  - **Tier 1 (crash + perf)** — default ON, lawful basis: **legitimate interest** (product reliability). Explicit, transparent opt-out via CLI command and env var.
  - **Tier 2 (usage patterns)** — default OFF, lawful basis: **consent**. Opt-in only, prompted at `spur init` (interactive) or activated by `spur telemetry enable usage`.
- **Retention:** 90 days on PostHog, configured project-side. Spec requires a recurring quarterly audit that retention is still enforced.
- **Disclosure:** README section + `docs/PRIVACY.md` + `spur telemetry --help` + first-run notice.

## 3. Consent & configuration

### 3.1 Storage

Telemetry state lives in its **own file**, not in the global `~/.spur/config.toml`, to avoid race conditions on tier toggles and prevent schema entanglement with agents/brain/PM/bot config:

- Path: `~/.spur/telemetry.toml` (or platform-equivalent via `dirs::config_dir()`; macOS still uses `~/.spur/` for parity with existing SPUR config).
- Schema versioned with a top-level `version = 1` key.
- Atomic writes via `tempfile` + `rename`. Reads are best-effort: on parse error or missing file, log `warn!` and operate with defaults (Tier 1 on if env-allowed, Tier 2 off, no persisted UUID).
- Schema (TOML):
  ```toml
  version = 1
  anonymous_id = "uuid-v4-here"
  tier1_crash = true       # panic hook + crash uploads
  tier1_perf  = true       # llm/mcp/acp/tui perf events + session_started
  tier2_usage = false      # opt-in usage events
  last_consent_prompt_at = "2026-05-14T03:39:24Z"
  ```

### 3.2 Activation matrix

| Environment | Behavior |
|---|---|
| TTY attached, first run | Interactive prompt: (1) Tier 1 (default Y), (2) Tier 2 (default N). Choice persisted. |
| Non-TTY (CI, pipe, headless), first run | Tier 1 ON, Tier 2 OFF, single-line stderr notice printed once: `Telemetry: anonymous crash/perf enabled (legitimate interest). Disable with SPUR_TELEMETRY=0 or 'spur telemetry disable all'.` Choice persisted. |
| `SPUR_TELEMETRY=0` env | Runtime-disabled: `init()` returns a no-op handle. No file writes, no events, no panic hook installed. |
| `CI=true` env | Same as `SPUR_TELEMETRY=0`. |
| `SPUR_POSTHOG_KEY` unset at build time | Compile-time disabled: `cfg(telemetry_disabled)` is emitted by `build.rs`, all events compile to no-ops, no HTTP client is linked. |
| `cargo test` | The workspace ships a `.cargo/config.toml` setting `[env] SPUR_TELEMETRY = "0"` so all test runs are runtime-disabled. (We do **not** rely on `cfg(test)`, which would only affect the crate being compiled, not dependents.) |
| `cargo run` / `cargo build` from source | Same: source builds typically don't have `SPUR_POSTHOG_KEY` and so compile to no-ops. |

### 3.3 CLI surface

```
spur telemetry status                      # show ID, tier states, last flush
spur telemetry enable [crash|perf|usage|all]
spur telemetry disable [crash|perf|usage|all]
spur telemetry reset-id                    # rotate UUID
spur telemetry config                      # re-prompt (TTY) or print state (non-TTY, exit 0)
spur telemetry flush                       # force synchronous flush
```

### 3.4 Per-tier disable semantics

| Command | Effect |
|---|---|
| `disable crash` | Panic hook is NOT installed for the rest of this and future sessions. Existing files in `~/.spur/crash-reports/` are not uploaded but are also not deleted; user may delete the directory manually. |
| `disable perf` | `llm_request_duration`, `mcp_request_duration`, `acp_request_duration`, `tui_frame_slow`, and `session_started` are skipped. |
| `disable usage` | All Tier 2 events are skipped. |
| `disable all` | All of the above. Equivalent in behavior to `SPUR_TELEMETRY=0` for the session, but persisted to `telemetry.toml`. |

## 4. Tier 1 events (default ON, legitimate interest)

| Event | Properties | Notes |
|---|---|---|
| `session_started` | `os`, `arch`, `spur_version` (= `env!("CARGO_PKG_VERSION")`), `is_tui` | One per process launch. |
| `$exception` | `panic_type` (allowlisted enum: `bounds`, `unwrap`, `option_unwrap`, `result_unwrap`, `assertion`, `other`), `payload_hash` (SHA-256 of `panic_message + anonymous_id`, first 8 hex chars — peppered to prevent cross-user collision into the same bucket), `sanitized_stack` (absolute paths → relative crate paths; user home stripped), `crate`, `module`, `line` | Written by panic hook to `~/.spur/crash-reports/<uuid>.json`. Uploaded best-effort on next launch, then deleted. Only Rust panics in the SPUR workspace; external MCP server crashes are NOT attributed. |
| `llm_request_duration` | `model_name` (allowlist of known public models; unknown models bucket to provider prefix: `anthropic_other`, `openai_other`, `google_other`, `local_other`, `other`), `duration_ms`, `token_count_bucket` (rounded down to nearest 100), `outcome` (`ok`/`timeout`/`error`) | |
| `mcp_request_duration` | `duration_ms`, `outcome` | **No `tool_name`, no `server_name`.** Generic perf signal only. |
| `acp_request_duration` | `duration_ms`, `outcome` | |
| `tui_frame_slow` | `duration_ms` | Emitted only when frame > **33 ms** (~30 FPS budget); not per frame. Threshold can be tuned via `SPUR_TUI_FRAME_THRESHOLD_MS` env (developer escape hatch, not user-facing). |

Sanitization rules are enforced at the type level via the `IntoProp` trait (see §6.1). Stack-frame paths are scrubbed via `redact::scrub_stack`, which strips:

- Home dir prefixes (`/Users/<x>/`, `/home/<x>/`, `C:\Users\<x>\`).
- Anything before a SPUR crate name (`spur-*`).
- All file paths outside the SPUR workspace (replaced with `<external>`).

Debug-info dependence: `module`/`line` require `debug = "line-tables-only"` in release builds. The build profile change is part of implementation.

## 5. Tier 2 events (opt-in, consent)

| Event | Properties |
|---|---|
| `plan_created` | `task_count`, `brain_model` (allowlisted, bucketed like §4), `duration_ms` |
| `worker_dispatched` | `worker_model` (allowlisted, bucketed), `skill_used` (enum from skill registry), `attempt_num` |
| `mcp_tool_called` | `server_name` (plaintext for allowlist: `github`, `posthog`, `spur-mcp`, `stitch`, `playwright`, `context7`, `firebase`, `sequential-thinking`; SHA-256 prefix for others), `tool_name` (plaintext only when `server_name` is in the allowlist; SHA-256 prefix otherwise to maintain symmetry with `server_name`), `outcome` |
| `review_completed` | `outcome` (`accept` / `reject` / `request_changes`), `iteration_count` |
| `tui_view_opened` | `view_name` (enum of named TUI screens) |

Explicit Tier 2 exclusions:

- No tool arguments, no tool return values.
- No agent prompts, no agent outputs.
- No file paths, no branch names, no commit messages, no issue titles, no plan contents.
- No error message text (only the error type/enum).

## 6. API shape

### 6.1 Typed events + sealed property trait

```rust
// crates/spur-telemetry/src/events.rs (sketch)

mod sealed { pub trait Sealed {} }

pub trait IntoProp: sealed::Sealed {
    fn into_value(self) -> serde_json::Value;
}

// Only these impls exist. Path/PathBuf/String/&str/etc. simply don't compile.
impl sealed::Sealed for bool {}
impl IntoProp for bool { /* ... */ }
impl sealed::Sealed for i64 {}
impl IntoProp for i64 { /* ... */ }
impl sealed::Sealed for u64 {}
impl IntoProp for u64 { /* ... */ }
impl sealed::Sealed for &'static str {}
impl IntoProp for &'static str { /* ... */ }
// Plus allowlisted enums: ModelName, PanicType, McpServerName, McpToolName, Outcome, ViewName, etc.
// Each enum has an explicit impl. New enums require a code review.

pub trait Event {
    const NAME: &'static str;
    const TIER: Tier;
    fn into_props(self) -> Props;
}

pub struct SessionStarted { pub os: &'static str, pub arch: &'static str, pub spur_version: &'static str, pub is_tui: bool }
impl Event for SessionStarted { /* ... */ }
```

`Path` and `PathBuf` lack `IntoProp` impls, so any event field of those types fails to compile. The redaction guarantee is enforced by the type system, not by code review.

### 6.2 Call-site macro

```rust
telemetry::emit!(SessionStarted { os: env::consts::OS, arch: env::consts::ARCH, ... });
```

The macro must:

- Expand to **no tokens at all** when `cfg(telemetry_disabled)` is set (preserves §3.2 compile-time gating).
- When enabled, the macro short-circuits before constructing the event struct: it first checks the global runtime-disabled atomic flag, and only if telemetry is active does it evaluate its arguments and push to the queue. This avoids paying argument-construction cost in hot paths when the user has run-time-disabled telemetry.

### 6.3 Lifecycle

```rust
pub fn init(cfg: InitConfig) -> TelemetryGuard;     // installs panic hook, starts batch task, loads config
pub fn shutdown(guard: TelemetryGuard);             // explicit, returns when flush completes or 250ms elapses
```

`TelemetryGuard` is held in `main()` of `spur-cli`. Its `Drop` impl calls `shutdown()` synchronously as a safety net for early returns.

## 7. Reliability rails

### 7.1 Panic-hook contract

- The hook is **synchronous, `std::fs` only**, no tokio handle, no `await`, no `spawn`, no `block_on`.
- Wrapped in `std::panic::catch_unwind` so a panic during the hook itself is swallowed (worst case: crash file lost, never an amplification loop).
- Chained to any previous hook installed by libraries (calls the prior hook after writing the crash file).
- Idempotent install: a `std::sync::Once` ensures only one hook is set even if `init()` is called multiple times across `spur-cli` and `spur-tui`.
- Crash file write is the only telemetry action on the panic path. No HTTP, no consent check (already gated by whether the hook was installed in the first place).

### 7.2 Shutdown contract

Shutdown is **explicit**, called from defined points:

- `spur-cli` `main` holds a `TelemetryGuard`; its `Drop` calls `shutdown()`.
- A SIGINT/SIGTERM handler (registered via `tokio::signal`) calls `shutdown()` before propagating.
- The CLI subcommand `spur telemetry flush` calls `shutdown()` then exits.

Shutdown blocks for at most **250 ms** waiting for the in-flight batch to upload. Anything still queued after 250 ms is dropped. **Abrupt termination (SIGKILL, power loss) drops the queue — documented as accepted behavior.**

### 7.3 Other rails

| Rail | Behavior |
|---|---|
| Rate limit | Token bucket using **`std::time::Instant`** (monotonic, no clock-jump issues), **500 events/min** per process. Drop overflow with one `tracing::warn!` per drop-window. Drop counter is atomic; logging is throttled. |
| Per-source sampling | High-frequency sources (`mcp_request_duration`, `acp_request_duration`) are sampled at 1/10 when burst rate exceeds 100/min, to prevent rate-limit starvation of lower-frequency events. |
| Batch flush | Every 10 s or 50 events, whichever first. Queue is a `tokio::sync::mpsc` bounded channel, capacity 200. Producer side drops on full channel and increments a counter. |
| Offline buffering | In-memory only for normal events. No disk persistence except crash files. |
| Crash files | One JSON per panic in `~/.spur/crash-reports/`. Uploaded on next launch, then deleted. Lazy ID init — if `telemetry.toml` has no UUID yet, generate one before writing. Malformed crash files on next launch are deleted unconditionally. |
| Network failure | Drop the batch silently externally. Increment internal counters (§13). |
| External MCP crashes | Out of scope. Only Rust panics in the SPUR workspace trigger `$exception`. |

## 8. Build & key handling

`crates/spur-telemetry/build.rs`:

```rust
fn main() {
    println!("cargo:rerun-if-env-changed=SPUR_POSTHOG_KEY");
    println!("cargo:rustc-check-cfg=cfg(telemetry_disabled)");
    if std::env::var("SPUR_POSTHOG_KEY").is_err() {
        println!("cargo:rustc-cfg=telemetry_disabled");
    }
}
```

- If `SPUR_POSTHOG_KEY` is unset at build time, `cfg(telemetry_disabled)` is emitted; `lib.rs` exports no-op shims; no HTTP client is linked. Source builds and forks ship with telemetry compiled out.
- If set, the key is baked in as `pub(crate) const POSTHOG_KEY: &str = env!("SPUR_POSTHOG_KEY")`.
- Release builds intended for distribution set `SPUR_POSTHOG_KEY` in CI. The key is a PostHog write-only project key (public by design, not a secret).
- We do **not** rely on `cfg(test)` to disable telemetry, because it only applies to the crate being compiled — dependent crates' tests would still link a live telemetry crate. Runtime disablement via `SPUR_TELEMETRY=0` (set in workspace `.cargo/config.toml`) is the reliable path.

## 9. Crate layout

```
crates/spur-telemetry/
├── Cargo.toml
├── build.rs
└── src/
    ├── lib.rs         # public API: init(), shutdown(), TelemetryGuard, emit! macro
    ├── client.rs      # PostHog HTTP client (reqwest)
    ├── consent.rs     # tier gating, env-var checks, config load/save
    ├── config.rs      # telemetry.toml schema, atomic read/write, migration
    ├── crash.rs       # panic hook, local crash files, next-launch upload
    ├── ratelimit.rs   # 500/min token bucket (Instant-based)
    ├── redact.rs      # path stripping, model bucketing, panic-type allowlist
    ├── batch.rs       # bounded mpsc queue + flush task
    ├── events.rs      # typed event structs + Event trait + sealed IntoProp
    └── error.rs       # TelemetryError, Result alias
```

`spur-telemetry` depends on: `tokio`, `reqwest` (only when `!cfg(telemetry_disabled)`), `serde`, `serde_json`, `toml`, `uuid`, `sha2`, `dirs`, `tracing`. It depends on **no other `spur-*` crate** (no circular-dep risk). Reading `~/.spur/telemetry.toml` is owned exclusively by this crate; no other crate touches that file.

## 10. Integration points

- **`spur-cli`** (binary entry point): `init()` after config load; install panic hook; hold `TelemetryGuard` in `main`; register SIGINT/SIGTERM handler that calls `shutdown()`. **All telemetry control flow originates here.**
- **`spur-tui`**: emit `tui_frame_slow`, `tui_view_opened` (Tier 2). Does **not** call `init()`; reuses the global state installed by `spur-cli`.
- **`spur-acp`**: emit `acp_request_duration` around request handlers.
- **`spur-mcp`**: emit `mcp_request_duration` (Tier 1) and `mcp_tool_called` (Tier 2) around tool dispatch.
- LLM call sites (wherever model HTTP requests happen): emit `llm_request_duration`.
- Plan/worker/review state transitions: emit `plan_created`, `worker_dispatched`, `review_completed`.

All integration points add a single `telemetry::emit!(...)` call with strongly typed properties. Per-call overhead when telemetry is runtime-disabled: one atomic load. When enabled but a tier is off: one atomic load + one enum compare. When emitting: one mpsc try_send.

## 11. Testing strategy

- **Workspace tests:** `.cargo/config.toml` sets `[env] SPUR_TELEMETRY = "0"` and `CI = "true"` for all `cargo test` runs across the workspace. This guarantees no test in any crate makes outbound network calls regardless of `cfg(test)` propagation.
- **`spur-telemetry` unit tests:** test redaction (`scrub_stack`, `IntoProp` impls), token bucket, config TOML round-trip, panic-type allowlist. None of these require the network.
- **`spur-telemetry` integration tests:** in `tests/integration.rs`, spawn a subprocess that builds with `SPUR_POSTHOG_KEY=loopback` and points the client at a local `wiremock` server (binding to `127.0.0.1:0`). Verify batch shape, redaction, rate limit, consent gating, and crash-file roundtrip end-to-end.
- **Golden-file tests** for `scrub_stack` on each platform (macOS, Linux, Windows path forms).
- **Panic-roundtrip test**: spawn a child process that intentionally panics; verify the crash file is written, then run the parent again and verify upload + deletion.

## 12. Open questions deferred to implementation

- Exact path-scrub regex(es) on Windows path separators / mixed `/` and `\`.
- The full `ModelName` enum contents — initial list from current SPUR-supported models; growth requires a PR.
- The full `McpServerName` allowlist — initial list above; growth requires a PR.

## 13. Self-observability

Telemetry must observe itself so silent failures don't go undetected. The crate emits **internal `tracing` events** (NOT PostHog telemetry; these go to the user's local log sink and respect their `RUST_LOG`):

- `tracing::warn!` once per drop-window when the rate limit kicks in, with the dropped count for the window.
- `tracing::warn!` on PostHog HTTP failure (status code, attempt number; no payload).
- `tracing::info!` on graceful shutdown with `{events_uploaded, events_dropped, batches_sent}` counters.
- `tracing::error!` on `telemetry.toml` parse failure, with the path and the parse error.

These are diagnostic for SPUR operators, not phoned home. A user running with `RUST_LOG=spur_telemetry=debug` gets full visibility into telemetry behavior.

## 14. Out of scope

- Distributed tracing (OpenTelemetry).
- Structured logs to a remote sink.
- Real-time alerting.
- A/B experiment infrastructure.
- Self-hosting the telemetry backend.

## 15. Acceptance criteria

The implementation lands when:

1. `cargo test --workspace` passes with zero outbound network calls. Verified by running tests behind `tcp.deny` on Linux CI or by the integration-test mock-server contract (no test should reach any host other than 127.0.0.1).
2. `cargo run -- telemetry status` shows the anonymous ID and per-tier states.
3. The panic-roundtrip integration test passes: a child process panics → crash file is written → parent process on next launch uploads it to the loopback mock server → file is deleted.
4. **Mock-server build verification**: an integration test builds `spur-cli` with `SPUR_POSTHOG_KEY=loopback`, points it at a `wiremock` server, runs a workload, and asserts every captured event matches an allowed schema. With `SPUR_POSTHOG_KEY` unset, the same test asserts the mock server receives zero requests.
5. Tier 2 events do not fire when Tier 2 consent has not been recorded (verified by the mock-server contract).
6. `disable crash` removes the panic hook on next launch and does not delete existing crash files (verified by filesystem assertions).
7. Documentation: README section, `docs/PRIVACY.md`, and updated `--help` output.
