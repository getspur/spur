# Codex ACP 1.9 Guarded Upgrade — Design

**Status:** Approved by the user on 2026-09-05
**Design epic:** `bd-3v50q` (closed)
**Supersedes for active configuration:** the 1.7 pin in the 2026-08-29 design; that historical record remains unchanged.

## Goal

Upgrade every active SPUR Codex ACP launch and probe surface to exact version `1.9.0`, without persisting the adapter's account identity metadata, while making the supported Codex profile-activation path explicit to the brain.

## Evidence

The indexed `codex-acp@1.9.0` package establishes these version-sensitive facts:

- `_CodexAcpServer::publishFirstAuthStatusAfterResponse` schedules an auth read after `initialize`.
- `_CodexAcpServer::setAuthStatus` sends `_auth/status_update` with `{ authStatus: next }`.
- Auth status equality includes account email, organization, and plan fields.
- `_CodexAcpServer::createSessionConfigOptions` exposes mode, collaboration mode, model, optional reasoning effort, and optional fast mode. It has no primary-profile selector.
- `package.json` depends on `@openai/codex ^0.153.2` and ACP SDK `^1.4.0`.

The exact OpenAI Codex 0.153.2 release commit `657a993cbee87acf52d14b758ce49dbd46d1b8eb` establishes the profile behavior:

- `load_agent_roles` discovers `agents` directories from configuration layers.
- `apply_spawn_agent_role` resolves an `agent_type` and applies its role file to a child configuration.
- `apply_role_to_config_inner` can apply developer instructions, model, and reasoning effort from that role.

The current SPUR path materializes a named profile into `.codex/agents/<name>.toml` and selects it on a fresh worker session. A TUI `@worker` mention is transformed into brain context; it does not mutate the already-running brain connection.

## Root causes

### Auth metadata persistence

`NativeAcpConnection` currently forwards every extension notification, including `_auth/status_update`, to the core session. The session wraps the payload in `SpurEventBody::AgentExtNotification`, and the durable sink serializes every `SpurEvent`. This makes a newly introduced 1.9 account payload eligible for persistence even though SPUR has no auth-status consumer.

### Event storage permissions

The sink uses default process umask behavior for `.spur/events` and its NDJSON files. On the inspected workspace this produced a `0755` directory and `0644` files, which are inappropriate for event data that can include prompts or extension payloads.

### Profile activation ambiguity

The enriched mention hint carries worker/profile/model/effort values but does not state their exact `submit_plan`/delegation mapping. This can be misread as an in-place brain profile switch, an operation neither ACP nor Codex ACP 1.9 exposes.

## Design

### 1. Drop auth status at the ACP extension boundary

Treat `_auth/status_update` as connection metadata, not a SPUR event. Filter it before writing to `ext_notification_tx`. Preserve every other extension notification byte-for-byte. This placement protects every downstream session/event consumer and avoids editing the already-modified core session implementation.

### 2. Make the event store private on Unix

Set `.spur/events` to `0700` and every active or existing `.ndjson` event file to `0600`. Apply the policy when the sink starts, when it opens the initial file, and on rotation. Non-Unix builds retain their existing behavior behind `cfg(unix)`.

### 3. Pin exact Codex ACP 1.9.0 everywhere active

Update the seed template, type documentation, CLI install/help strings, onboarding cookbook, JavaScript probe, Python probe, and their tests. Keep the old 1.7 design/spec files as historical records. The probe must report bundled Codex `0.153.2` for exact 1.9 evidence and must not silently execute a different inherited `codex-acp` binary.

### 4. State the supported profile route in the mention hint

For an enriched mention `@codex,<profile>,<model>,<effort>`:

- `codex` maps to the SPUR worker `agent`.
- `<profile>` maps to the delegation `profile`.
- `<model>` and `<effort>` remain explicit request overrides.
- Activation happens while starting a fresh worker session.
- The current brain session remains unchanged; changing its startup profile requires reconnecting/restarting it.

This preserves SPUR's beads, isolation, review, and request-precedence semantics. It deliberately does not bridge mentions to Codex-native `spawn_agent`, whose role application order can override explicitly requested model or effort.

## Solver pre-gates

| Concern | Current result | Guarded result |
|---|---|---|
| Auth notification safety | `sol_1079dd7ab2cc47e5`: fail/UNSAT, sensitive persisted state reached | `sol_f9e5dc3ae2634c0d`: pass/SAT |
| Unix event modes | `sol_1a1da4307bcb4916`: UNSAT for 0755/0644 under private policy | `sol_d433309ef6af4f51`: SAT with directory `0700`, file `0600` |
| Runtime seed version | `sol_d0ed070866f345a6`: 1.7 fails selected interval | `sol_7149b18b4b0748a5`: 1.9 passes |
| CLI version | `sol_3b318c1a9a2e45a1`: 1.7 fails selected interval | `sol_cd39652f4ed14a7f`: 1.9 passes |
| Probe version | `sol_d79f8c08fdd24dea`: 1.7 fails selected interval | `sol_1293250df9b747b9`: 1.9 passes |
| Profile activation | `sol_9869e727050541e5`: in-place brain target unreachable at bound 2 | `sol_1f87a946bdd7462d`: SPUR worker target reachable at bound 5 |

Each implementation task must first reproduce its RED test, then make the minimal change, run its focused suite, and repeat the corresponding solver request as a post-proof using the landed facts.

## Non-goals

- No in-place mutation of an active brain profile.
- No default enablement of Codex `multi_agent_v2`.
- No automatic conversion of TUI mentions into Codex-native child-agent calls.
- No rewriting or deletion of existing user-owned event logs during this change.
- No edits to the historical 1.7 design/spec records.

## Acceptance criteria

- `_auth/status_update` never reaches SPUR's extension notification receiver; unrelated extension notifications remain unchanged.
- On Unix, the event directory is `0700` and active/existing NDJSON files are `0600` after sink initialization.
- No active runtime, CLI, cookbook, or probe pin remains at Codex ACP 1.7.0.
- Exact 1.9 probe evidence expects bundled Codex 0.153.2.
- Enriched worker hints state the worker/profile/model/effort mapping and the fresh-session boundary.
- Focused tests, affected crate suites, format, and post-solves all pass.
