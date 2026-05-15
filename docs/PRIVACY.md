# SPUR Privacy and Telemetry

SPUR uses anonymous-identifier-based telemetry to improve reliability and performance.
This telemetry is **pseudonymous** (not fully anonymous): events are linked by a random
UUID, with no intended collection of direct identifiers like name or email.

## What We Collect

SPUR telemetry is split into tiers.
Canonical event schemas are defined in `crates/spur-telemetry/src/tier1_events.rs`,
`crates/spur-telemetry/src/tier2_events.rs`, and `crates/spur-telemetry/src/crash.rs`.

### Tier 1: Crash Diagnostics (default ON)

Purpose: detect crashes and stabilize SPUR.

Collected fields (examples):

- `anonymous_id`: `"2f5bb2f1-8ef0-4a4f-a4ad-9c6c3e5f5360"`
- `event`: `"$exception"`
- `panic_type`: `"assertion"`
- `payload_hash`: `"7f3a9c2d"`
- `sanitized_stack`: `"crates/spur-core/src/orchestrator.rs:120:9"`
- `crate`: `"spur-core"`, `module`: `"orchestrator"`, `line`: `120`
- `os`: `"macos"`, `arch`: `"aarch64"`

### Tier 1: Performance Telemetry (default ON)

Purpose: measure runtime latency and operational health.

Collected fields (examples):

- `anonymous_id`: `"2f5bb2f1-8ef0-4a4f-a4ad-9c6c3e5f5360"`
- `event`: `"llm_request_duration"`
- `model_name`: `"gpt-5-codex"`
- `duration_ms`: `842`
- `token_count_bucket`: `300`
- `outcome`: `"ok"`

### Tier 2: Usage Patterns (default OFF; opt-in)

Purpose: understand feature adoption and workflow patterns.

Collected fields (examples):

- `anonymous_id`: `"2f5bb2f1-8ef0-4a4f-a4ad-9c6c3e5f5360"`
- `event`: `"plan_created"`
- `task_count`: `5`
- `brain_model`: `"claude-sonnet-4-7"`
- `duration_ms`: `1540`

`spur_version` is attached by SPUR's telemetry emitter to all events.

## Disable Telemetry

Environment disable (session/runtime):

```sh
SPUR_TELEMETRY=0 spur
```

CLI disable (persisted):

```sh
spur telemetry disable all
```

Per-tier CLI disable:

```sh
spur telemetry disable crash
spur telemetry disable perf
spur telemetry disable usage
```

## Retention

- Default telemetry retention is **90 days**.
- Retention is configured on the PostHog backend; the SPUR client does not auto-purge.
- Crash files written locally are uploaded on next launch and then deleted locally.
- If telemetry is disabled, no new telemetry events are sent.

On first run in non-TTY environments (for example CI/piped output), SPUR prints one
stderr notice that Tier 1 telemetry is enabled and includes one-line disable commands.

## Deletion Request via Anonymous ID

1. Run `spur telemetry status` and copy your `anonymous_id`.
2. Open a GitHub issue in the SPUR repository with title
   `privacy/deletion-request: <anonymous_id>` and include that `anonymous_id`.
3. SPUR operators locate and remove telemetry records for that ID from retained data.
4. Run `spur telemetry reset-id` to rotate your local ID for future sessions.

## Pseudonymity Disclosure

The telemetry identifier is random and not meant to directly identify you, but because it
persists across sessions it is treated as **pseudonymous personal data** in privacy terms.
If you want no telemetry linkage at all, disable telemetry with `SPUR_TELEMETRY=0`.
