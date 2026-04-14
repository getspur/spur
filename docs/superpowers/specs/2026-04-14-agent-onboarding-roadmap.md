# Agent Onboarding Roadmap — Config-First Framework

**Status:** roadmap (north-star for specs 1-4)
**Date:** 2026-04-14
**Area:** `spur-acp` config + `spur-tui` command / mention / dispatch surface

## Why this exists

Today, adding a new agent (opencode, gemini-cli, codex, kimi-cli) requires touching `spur-tui` in 3-4 places: `commands/registry.rs` (hardcoded `if handle == "kiro"`), `views/session_detail.rs` (hardcoded `KIRO_COMMANDS_AVAILABLE` handler), `commands/spur_local.rs` (static command list), plus a `config.toml` entry. The wire-level plumbing is solved — `AgentConnection` trait with four transport adapters (acp, stream-json, cli-wrap, stdio) covers every protocol we've encountered. The **shell layer** (how the TUI surfaces commands, mentions, dispatch) is where onboarding friction lives.

This roadmap formalizes the shell-layer integration surface into a config-first framework with a fixed-size registry of built-in code hooks. New agents onboard by adding a `[[agents.entries]]` block and reusing existing hooks. New hooks are added only when an agent exhibits a genuinely novel behavior.

## The core rule

**Config declares CHOICES between built-in code hooks. Config does not describe TRANSFORMATIONS.**

The moment we feel tempted to express logic in TOML — templates with conditions, JSONPath with fallbacks, arithmetic, loops — we add a named code hook instead. Configs are data; transformations are code.

This is a cultural rule enforced by convention and code review. Spec 1 documents it in the config parser's module header so every future contributor sees it.

## Tripwire: prohibited in the config schema

Any future PR attempting to add the following to the config schema MUST instead add a code hook:

- Template variable substitution beyond the fixed set: `{session_id}`, `{user_text}`, `{rest}`
- Conditionals (`if.then.else`, `when`, `unless`)
- Loops or iteration
- Arithmetic or boolean composition
- String-splitting / regex / substring extraction
- Multi-step pipelines

## Onboarding contract (target)

Adding a new agent to Spur should be one of:

1. **Best case** — one config block in `.spur/config.toml`, no code. Possible when the new agent's transport + dispatch semantics match an existing hook combination. Example target: `codex` (stream-json transport, prompt-text dispatch — both exist).
2. **Typical case** — one config block + one new code hook (one file + one line in a registration function). Possible when the agent has a novel behavior but reuses existing hook *types*. Example target: `opencode` (possibly needs a new ingest parser).
3. **Worst case** — new transport adapter in `spur-acp/src/connection/` (implements `AgentConnection`) + config block + 1-2 new hooks. Only required when the wire protocol is genuinely new. Example candidate: `kimi-cli` if its protocol is neither ACP nor stream-json.

Case (1) is the default; cases (2) and (3) are exceptional. The goal isn't to prevent code — it's to keep the code additions local, file-scoped, and mechanical.

## Sub-concerns expressible in config

Each `[[agents.entries]]` block may contain the following sub-tables. All are optional; sensible defaults apply when absent.

| Sub-table | Purpose | Example key hooks |
|---|---|---|
| `[agents.entries.commands]` | Slash-command dispatch policy + vendor-ext wiring | `dispatch`, `exec_method`, `args_template`, `ingest`, `response` |
| `[agents.entries.mentions]` | Which `MentionSource` hooks to mount for this agent's sessions | `sources = ["files", "kiro_specs"]` |
| `[agents.entries.permissions]` | Permission-bypass levers (already shipped as `skip_permissions_*`; Spec 1 nests these) | `skip = true`, `args`, `session_mode`, `auto_approve` |
| `[agents.entries.capabilities]` | Declared protocol features (plan-mode, usage, load_session, list_sessions) | `plan_mode = true`, `usage = true` |
| `[agents.entries.display]` | UX metadata | `handle = "claude"` (short alias), `display_name = "Claude"` |

Exact shapes are defined in Spec 1. The roadmap only names the sub-tables so all specs share vocabulary.

## Built-in hook vocabulary (target)

Ship ~7-10 hooks in the first release. Names are provisional; Spec 1 finalizes them.

| Hook ID | Kind | What it does | Used by |
|---|---|---|---|
| `prompt_text` | dispatch | Send `/name args` as `ContentBlock::Text` to the agent's prompt stream | claude, codex, gemini |
| `vendor_exec` | dispatch | Call a vendor-ext RPC (`_foo.dev/bar/baz`) with a typed args payload | kiro |
| `raw_rest` | args_template | Input `"/foo bar baz"` → `{ args: { raw: "bar baz" } }` | kiro |
| `json_path_list` | ingest_parser | Decode a JSON array at a named path, producing items via `item_schema` | kiro (future: codex if it advertises commands) |
| `acp_available_command` | item_schema | Decode ACP's `AvailableCommand` struct | kiro, any ACP agent |
| `system_note` | response_render | Render a vendor-ext response as a system-note trace entry | kiro |
| `files` | mention_source | Filesystem file mentions (today's default) | all agents |

New hooks are added in one file: `crates/spur-tui/src/agents/hooks.rs` (or equivalent), plus a one-line registration in `register_builtin_hooks()`. Hook IDs are typed enums with strongly-typed deserialization — a typo in config fails at parse time with a clear error.

## Validation philosophy

1. **Strongly-typed deserialization** — `dispatch: DispatchKind`, not `dispatch: String`. Unknown values rejected by serde at parse time.
2. **Hook existence check at startup** — every hook ID referenced in config must resolve against the built-in registry. Missing → fail loudly with "hook 'xyz' not found; known: [list]".
3. **No silent defaults** — if a config block is malformed or references a missing hook, refuse to start that agent and tell the user why. Do not fall back.
4. **`spur config check` subcommand** — dry-runs the config, reports every error, does not start any agents. Bolt-on, ~100 LOC.
5. **Capability-vs-mechanism consistency** — if an agent config declares `[capabilities]` features that require specific hook bindings, the validator confirms those bindings are present. Closes follow-up F2 (silent `skip_permissions`-without-mechanism warning) as a general case.

## Migration path

**Pre-existing config.toml files keep working.** All new sub-tables are optional; omitting them applies the same defaults as today's hardcoded behavior (`prompt_text` dispatch, `files`-only mentions, no vendor ext). The one exception: **kiro's vendor-ext wiring stops being automatic**. Users on old configs see a warning at startup:

> `[agents.entries]` 'kiro' has no `[agents.entries.commands]` block; the vendor-ext dispatch that was previously hardcoded is now explicit. Using prompt-text dispatch (no kiro vendor-ext). See `docs/spur/agent-config.md` for the migration snippet.

`.spur/config.toml.example` ships with a full kiro block. Users migrate on their own schedule.

## Spec decomposition

### Spec 1 — Foundation (implementable immediately after roadmap approval)

**Scope:**
- `[agents.entries.commands]` + `[agents.entries.display]` schema
- `AgentConfig` refactor: nest the three `skip_permissions_*` fields under `[agents.entries.permissions]` (closes follow-up F5)
- Hook registry (dispatch, args_template, ingest_parser, item_schema, response_render)
- Validator (closes follow-up F2 as an instance of the general pattern)
- `spur config check` subcommand
- Migrate claude + kiro from hardcoded to config-driven
- Delete `if handle == "kiro"` at `registry.rs:130`
- Delete hardcoded `KIRO_COMMANDS_AVAILABLE` handler at `session_detail.rs:979` (replaced by config-driven ingest)

**Non-goals:** new mention sources, new agents, runtime reload of config.

**Closes:** follow-ups F2 (silent mechanism warning) and F5 (AgentConfig sub-struct nesting).

### Spec 2 — Mentions plugin point

**Scope:**
- `[agents.entries.mentions]` schema
- `MentionSource` trait (already exists) becomes pluggable via config
- Add 1-2 new source hooks (e.g., `kiro_specs` as a proof-of-concept)

**Non-goals:** new agents.

### Spec 3 — First new agent (codex)

**Scope:** pure config addition, no new code. Proves the framework. Produces the onboarding cookbook.

**Non-goals:** anything requiring new hooks or new transports.

### Spec 4+ — Remaining agents (opencode, kimi-cli)

One spec per agent. Each proves the framework against a new wire protocol or dispatch shape. Adds at most one new hook or one new transport adapter.

## Success criteria

After Spec 1 ships, the following must be true:
- Grep for `if handle == "kiro"` returns zero matches.
- Grep for `KIRO_COMMANDS_AVAILABLE` in `session_detail.rs` returns zero matches.
- Adding a new agent with claude-like behavior requires only a `[[agents.entries]]` block — no Rust change.
- Every hook ID used in config is documented in `docs/spur/agent-config.md`.
- `spur config check` returns a non-zero exit code when a referenced hook doesn't exist.

After Specs 2-4 ship:
- At least 4 agents are onboarded (claude, kiro, codex, +1 of opencode/kimi/gemini).
- No new "special-case for agent X" branches have appeared anywhere in `spur-tui`.
- The built-in hook count is ≤12.

## What this roadmap does NOT answer

- Specific field names inside sub-tables (Spec 1's job).
- How to handle an agent that breaks the ACP protocol contract mid-stream (operational, not architectural).
- Authentication UX per agent (orthogonal — `authenticate` is already on `AgentConnection`).
- Cost/billing integration per agent (orthogonal — `cost_tier` already in config).
- Runtime config reload (non-goal for v1).

## Relationship to shipped skip_permissions work

The `skip_permissions` lever (merged 2026-04-14, commit `f47b579`) is the first concrete precedent for a config-driven per-agent policy. Its three levers (L1a args, L1b session mode, L2 auto-approve fast-path) map directly to the hook pattern this roadmap formalizes:

| Lever | Hook pattern equivalent |
|---|---|
| L1a (spawn args) | `args_hook` under `[permissions]` |
| L1b (session mode) | `session_mode_hook` under `[permissions]` |
| L2 (permission_tx) | `auto_approve_hook` under `[permissions]` |

Spec 1's `AgentConfig` refactor formalizes this mapping and renames the three scattered `skip_permissions_*` fields into a `[permissions]` sub-table. This closes F5 without inventing a new taxonomy — it just lifts the shipped design into the framework.
