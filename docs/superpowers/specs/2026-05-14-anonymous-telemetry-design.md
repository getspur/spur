# Anonymous Telemetry — Design Spec

**Status:** Approved for planning
**Date:** 2026-05-14
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

- **Anonymous-identifier-based** telemetry. A persistent UUIDv4 lives in `~/.spur/config.toml` (or the platform-equivalent config dir via `dirs::config_dir()`); the user can view it (`spur telemetry status`), rotate it (`spur telemetry reset-id`), and use it for deletion requests. Under GDPR this is **pseudonymous, not anonymous**; the spec uses "anonymous-identifier-based" as the user-facing term and is honest about pseudonymity in legal docs.
- **No user content ever leaves the box.** Redaction is enforced at the type level (see §6).
- **Two-tier consent** with distinct lawful bases:
  - **Tier 1 (crash + perf)** — default ON, lawful basis: **legitimate interest** (product reliability). Explicit, transparent opt-out via CLI command and env var.
  - **Tier 2 (usage patterns)** — default OFF, lawful basis: **consent**. Opt-in only, prompted at `spur init` (interactive) or activated by `spur telemetry enable usage`.
- **Retention:** 90 days on PostHog; configured in the PostHog project. Spec requires a recurring audit (quarterly) that retention is still enforced.
- **Disclosure:** README section + `spur telemetry --help` + first-run notice.

## 3. Consent UX

| Environment | Behavior |
|---|---|
| TTY attached, first run | Interactive prompt: (1) Tier 1 (default Y), (2) Tier 2 (default N). Choice persisted. |
| Non-TTY (CI, pipe, headless) | Tier 1 ON, Tier 2 OFF, single-line notice printed once: `Telemetry: anonymous crash/perf enabled (legitimate interest). Disable with SPUR_TELEMETRY=0 or 'spur telemetry disable all'.` |
| `SPUR_TELEMETRY=0` env | Fully disabled, no events, no init. |
| `CI=true` env | Fully disabled (any value other than `"false"` or empty). |
| `cfg(test)` or `cfg(debug_assertions)` | Compiled out entirely. |
| `SPUR_POSTHOG_KEY` unset at build time | Compiled to no-op (source builds = zero phone-home). |

CLI surface:

```
spur telemetry status            # show ID, tier states, last flush
spur telemetry enable [crash|perf|usage|all]
spur telemetry disable [crash|perf|usage|all]
spur telemetry reset-id          # rotate UUID
spur telemetry config            # re-prompt interactive
spur telemetry flush             # force synchronous flush
```

## 4. Tier 1 events (default ON, legitimate interest)

| Event | Properties | Notes |
|---|---|---|
| `session_started` | `os`, `arch`, `spur_version`, `is_tui` | One per process launch. |
| `$exception` | `panic_type` (allowlisted: `bounds`, `unwrap`, `assertion`, `option_unwrap`, `result_unwrap`, `other`), `payload_hash` (SHA-256 prefix, 8 hex chars, for grouping only), `sanitized_stack` (absolute paths → relative crate paths; user dirs stripped), `crate`, `module`, `line` | Written by panic hook to `~/.spur/crash-reports/<uuid>.json` synchronously. Uploaded best-effort on next launch, then deleted. Only Rust panics in the SPUR workspace — external MCP server crashes are NOT attributed. |
| `llm_request_duration` | `model_name` (allowlist of known public models; `"other"` for anything else), `duration_ms`, `token_count_bucket` (rounded down to nearest 100), `outcome` (`ok`/`timeout`/`error`) | |
| `mcp_request_duration` | `duration_ms`, `outcome` | **No `tool_name`, no `server_name`.** Generic perf signal only. |
| `acp_request_duration` | `duration_ms`, `outcome` | |
| `tui_frame_slow` | `duration_ms` | Emitted only when frame > 100 ms; not per frame. |

Sanitization rules (enforced in `redact.rs`, type-level):

- All `Path` / `PathBuf` properties are forbidden in event structs at the type level (no `impl` for path-like types).
- `model_name` is a typed enum with a `Custom` variant that serializes as `"other"`.
- `panic_type` is an `enum`; raw panic messages never serialize.
- Stack frames have user-home and absolute-path prefixes stripped via a `scrub_stack` function before serialization.

## 5. Tier 2 events (opt-in, consent)

| Event | Properties |
|---|---|
| `plan_created` | `task_count`, `brain_model` (allowlisted), `duration_ms` |
| `worker_dispatched` | `worker_model` (allowlisted), `skill_used` (enum from skill registry), `attempt_num` |
| `mcp_tool_called` | `server_name` (plaintext for known/public servers: `github`, `posthog`, `spur-mcp`, etc.; SHA-256 prefix for custom), `tool_name`, `outcome` |
| `review_completed` | `outcome` (`accept` / `reject` / `request_changes`), `iteration_count` |
| `tui_view_opened` | `view_name` (enum of named TUI screens) |

Explicit Tier 2 exclusions:

- No tool arguments, no tool return values.
- No agent prompts, no agent outputs.
- No file paths, no branch names, no commit messages, no issue titles, no plan contents.
- No error message text (only the error type/enum).

## 6. API shape

Typed event structs, one per event. The tier is a const on the type. Macro wraps the call site so dev/test builds compile to nothing.

```rust
// In crates/spur-telemetry/src/events.rs (sketch)
pub struct SessionStarted {
    pub os: &'static str,
    pub arch: &'static str,
    pub spur_version: &'static str,
    pub is_tui: bool,
}
impl Event for SessionStarted {
    const NAME: &'static str = "session_started";
    const TIER: Tier = Tier::One;
    fn into_props(self) -> Props { /* ... */ }
}

// Call site
telemetry::emit!(SessionStarted { os: env::consts::OS, arch: env::consts::ARCH, ... });
```

Properties:

- Strongly typed — no string maps at call sites.
- Tier is a `const` on the trait impl, not a runtime argument.
- The `emit!` macro expands to a no-op under `cfg(debug_assertions)` or `cfg(test)` so the call site disappears.
- Adding a new event is a code change with review — drift is impossible without a PR.

## 7. Reliability rails

| Rail | Behavior |
|---|---|
| Rate limit | Token bucket, **500 events/min** per process, drop overflow with one `warn!` per drop-window. |
| Batch flush | Every 10 s or 50 events, whichever first. |
| Graceful shutdown flush | Synchronous best-effort flush with **250 ms timeout** on process exit; remaining queue is dropped. |
| Offline buffering | In-memory only for normal events. No disk persistence except crash files. |
| Crash files | One JSON per panic in `~/.spur/crash-reports/`. Uploaded on next launch, then deleted. Lazy ID init — if config has no UUID yet, generate one before writing. |
| Network failure | Drop the batch silently. Never retry indefinitely. Never block app exit. |
| External MCP crashes | Out of scope. Only Rust panics in the SPUR workspace trigger `$exception`. |
| Panic during panic hook | Caught; crash file write is wrapped in `catch_unwind`. Worst case, the crash is lost — never a panic-amplification loop. |

## 8. Build & key handling

- `crates/spur-telemetry/build.rs` reads `SPUR_POSTHOG_KEY` env var.
- If unset: `cargo:rustc-cfg=telemetry_disabled` → `lib.rs` exports no-op shims. Source builds and forks ship with telemetry compiled out.
- If set: key is baked in as `pub(crate) const POSTHOG_KEY: &str = ...`.
- `cfg(debug_assertions)` and `cfg(test)` always force the no-op path regardless of key presence.
- Release builds intended for distribution set `SPUR_POSTHOG_KEY` in CI. The key is a PostHog write-only project key (public by design, not a secret).

## 9. Crate layout

```
crates/spur-telemetry/
├── Cargo.toml
├── build.rs
└── src/
    ├── lib.rs         # public API: init(), shutdown(), emit! macro
    ├── client.rs      # PostHog HTTP client (reqwest)
    ├── consent.rs     # tier gating, env-var checks, config load/save
    ├── crash.rs       # panic hook, local crash files, next-launch upload
    ├── ratelimit.rs   # 500/min token bucket
    ├── redact.rs      # path stripping, model allowlist, panic-type allowlist
    ├── batch.rs       # in-memory queue + flush task
    └── events.rs      # typed event structs + Event trait
```

## 10. Integration points

- `spur-cli` main: call `spur_telemetry::init()` after config load, install panic hook, register shutdown handler that calls `spur_telemetry::shutdown()`.
- `spur-tui` main: same as CLI.
- `spur-acp`: emit `acp_request_duration` around request handlers.
- `spur-mcp`: emit `mcp_request_duration` (Tier 1) and `mcp_tool_called` (Tier 2) around tool dispatch.
- LLM call sites (wherever model HTTP requests happen): emit `llm_request_duration`.
- Plan/worker/review state transitions: emit `plan_created`, `worker_dispatched`, `review_completed`.

## 11. Testing strategy

- `spur-telemetry` unit tests run with `cfg(test)` → telemetry compiles to no-op, so tests don't make HTTP calls.
- A separate integration test target builds with `--features telemetry-test-server` that points the client at a local mock HTTP server (via `wiremock` or hand-rolled). Verifies: batch shape, redaction, rate limit, consent gating, crash file roundtrip.
- Golden-file tests for `scrub_stack` and panic-type allowlist.

## 12. Open questions deferred to implementation

- Exact path-scrub regex(es) on each platform (macOS `/Users/<x>/…`, Linux `/home/<x>/…`, Windows `C:\Users\<x>\…`).
- Whether `tui_frame_slow` threshold should be configurable.
- Model allowlist contents — initial list will be the current SPUR-supported set; growth requires a PR.

## 13. Out of scope

- Distributed tracing (OpenTelemetry).
- Structured logs to a remote sink.
- Real-time alerting.
- A/B experiment infrastructure.
- Self-hosting the telemetry backend.

## 14. Acceptance criteria

The implementation lands when:

1. `cargo test --workspace` passes with zero outbound network calls.
2. `cargo run -- telemetry status` shows the anonymous ID and per-tier states.
3. A forced panic in a release build produces a crash file; the next launch uploads and deletes it.
4. With `SPUR_POSTHOG_KEY` unset, `cargo build --release` produces a binary that emits no events at runtime (verified by tcpdump).
5. Tier 2 events do not fire when Tier 2 consent has not been recorded.
6. Documentation: README section, `docs/PRIVACY.md`, and updated `--help` output.
