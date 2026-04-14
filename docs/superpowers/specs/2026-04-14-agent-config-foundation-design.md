# Agent Config Foundation — Spec 1

**Status:** design
**Date:** 2026-04-14
**Roadmap:** `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md`
**Area:** `spur-acp` config + `spur-tui` command/dispatch surface

## Problem

`spur-tui` branches on agent handle in three hardcoded sites:

- `crates/spur-tui/src/commands/registry.rs:130` — `if handle == "kiro" { Dispatch::KiroExecute } else { Dispatch::PromptText }`
- `crates/spur-tui/src/views/session_detail.rs:979` — hardcoded match on `KIRO_COMMANDS_AVAILABLE` to feed the command registry from a kiro-specific vendor-ext notification
- `crates/spur-tui/src/views/session_detail.rs:987` — hardcoded match on `SPUR_KIRO_EXECUTE_RESPONSE` to render the vendor-ext response as a system note

Separately, `AgentConfig` has grown 11 fields (including three scattered `skip_permissions_*` permission-bypass levers added in the 2026-04-14 skip_permissions ship). Adding more per-agent policy knobs — which the roadmap calls for — would push `AgentConfig` past 15 flat fields and make readability poor.

Spec 1 introduces the config-first foundation the roadmap describes. Exactly two built-in hooks (dispatch: `prompt_text`, `vendor_exec`) plus the supporting ingest / response / args-template / item-schema hooks required for kiro. Nothing more.

## Goals

1. **Delete the three hardcoded kiro branches** above. They become data in `.spur/config.toml`.
2. **Nest `skip_permissions_*` into `[agents.entries.permissions]`** (closes follow-up F5).
3. **Add a validator** that fails loudly on missing/unknown hooks and flags `[permissions]` with no active mechanism (closes follow-up F2 as an instance of the general pattern).
4. **Add `spur config check`** for pre-start config validation.
5. **No behavior change for existing configs** that match the shipped defaults — claude-code-acp still uses prompt-text, kiro still uses its vendor-ext dispatch. The difference is those behaviors are now declared in `.spur/config.toml.example` rather than baked into `spur-tui`.

## Non-goals

- Adding new mention sources (Spec 2).
- Adding new agents (Specs 3+).
- Runtime config reload.
- Per-session config overrides.
- Hint elicitation for unstructured-input commands (future).
- Handle aliasing for prefix disambiguation beyond a single `display.handle` field (we expose it, but long-handle UX fixes are out of scope).

## Design

### Config schema (additive)

All existing `[[agents.entries]]` fields stay. Four new optional sub-tables are added. Omitting them preserves today's defaults.

```toml
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
capabilities = []
cost_tier = "medium"

[agents.entries.display]
handle = "claude"              # short alias used as /prefix:cmd on collision
display_name = "Claude"        # reserved for future UX; unused in Spec 1

[agents.entries.commands]
dispatch = "prompt_text"       # hook ID; see built-in vocabulary below

[agents.entries.permissions]
skip = false                   # replaces top-level skip_permissions
# args = []                    # replaces top-level skip_permissions_args
# session_mode = "..."         # replaces top-level skip_permissions_session_mode

# --- kiro example, showing the non-default path ---
[[agents.entries]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[agents.entries.display]
handle = "kiro"

[agents.entries.commands]
dispatch = "vendor_exec"
exec_method = "_kiro.dev/commands/execute"
args_template = "raw_rest"

[[agents.entries.commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"

[[agents.entries.commands.response]]
method = "_kiro.dev/commands/execute"
render = "system_note"

[agents.entries.permissions]
skip = false
args = ["--trust-all-tools"]   # used only when skip = true
```

### Field reference (Spec 1 only)

**`[agents.entries.display]`**

| Field | Type | Default | Notes |
|---|---|---|---|
| `handle` | String | lowercase(name) | Short alias for `/prefix:cmd` when names collide |
| `display_name` | String | name | Reserved; consumed by future UX specs |

**`[agents.entries.commands]`**

| Field | Type | Default | Notes |
|---|---|---|---|
| `dispatch` | `DispatchKind` enum | `prompt_text` | Hook ID. Current values: `prompt_text`, `vendor_exec` |
| `exec_method` | String | — | Required when `dispatch = "vendor_exec"`. Full wire method (e.g. `"_kiro.dev/commands/execute"`) |
| `args_template` | `ArgsTemplateKind` enum | `raw_rest` | Required when `dispatch = "vendor_exec"`. How to turn "/cmd rest" into the RPC `args` payload |
| `ingest` | array of `IngestBinding` | empty | One entry per vendor-ext notification method that advertises commands |
| `response` | array of `ResponseBinding` | empty | One entry per vendor-ext method whose response gets rendered in the trace |

**`IngestBinding`** (sub-table):

| Field | Type | Notes |
|---|---|---|
| `method` | String | Wire method, e.g. `"_kiro.dev/commands/available"` |
| `parser` | `IngestParserKind` enum | Current values: `json_path_list` |
| `path` | String | JSON path (dotted, no array indexing) to the list |
| `item_schema` | `ItemSchemaKind` enum | Current values: `acp_available_command` |

**`ResponseBinding`** (sub-table):

| Field | Type | Notes |
|---|---|---|
| `method` | String | Wire method whose response we render |
| `render` | `ResponseRenderKind` enum | Current values: `system_note` |

**`[agents.entries.permissions]`** (F5 nesting of shipped `skip_permissions_*`):

| Field | Type | Default | Replaces |
|---|---|---|---|
| `skip` | bool | false | top-level `skip_permissions` |
| `args` | `Vec<String>` | empty | top-level `skip_permissions_args` |
| `session_mode` | `Option<String>` | None | top-level `skip_permissions_session_mode` |

Top-level fields retained as `#[serde(alias = "skip_permissions")]` (etc.) for a single release cycle with a deprecation warning; removed in v0.3.

### Built-in hook vocabulary (Spec 1)

Seven hooks ship in Spec 1. Defined in `crates/spur-tui/src/agents/hooks.rs` (new file).

| Hook ID | Kind enum | Purpose |
|---|---|---|
| `prompt_text` | `DispatchKind` | Build `ContentBlock::Text("/name args")`, send as prompt |
| `vendor_exec` | `DispatchKind` | Call `AgentConnection::call_ext(exec_method, args)` |
| `raw_rest` | `ArgsTemplateKind` | `"/cmd rest…"` → `{ args: { raw: "rest…" } }` |
| `json_path_list` | `IngestParserKind` | Look up `params[path]`, expect array, decode each element with `item_schema` |
| `acp_available_command` | `ItemSchemaKind` | `serde_json::from_value::<Vec<AvailableCommand>>` |
| `system_note` | `ResponseRenderKind` | `session_detail.push_system_note(format!("⟨{handle}⟩ response: {params}"))` |
| `files` | `MentionSourceKind` | Today's filesystem source (kept as enum variant for future Spec 2 parity; no behavior change) |

### Types (Rust)

New crate module `crates/spur-tui/src/agents/` with:

- `agents/mod.rs` — re-exports
- `agents/config.rs` — the sub-table structs (`DisplayConfig`, `CommandsConfig`, `IngestBinding`, `ResponseBinding`, `PermissionsConfig`)
- `agents/hooks.rs` — hook enums + registered implementations + `register_builtin_hooks`
- `agents/validator.rs` — startup validation

```rust
// agents/hooks.rs — shape, not full bodies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchKind { PromptText, VendorExec }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsTemplateKind { RawRest }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestParserKind { JsonPathList }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSchemaKind { AcpAvailableCommand }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseRenderKind { SystemNote }
```

Enum variants are the only accepted values at deserialize time — misspellings in config fail at parse with a clear error.

### Where the hardcoded branches go

**`registry.rs:130` (agent_entry):** deleted. Replaced by a helper that consumes the agent's `CommandsConfig` and builds the correct `CommandEntry` for each `AvailableCommand`:

```rust
// new: crates/spur-tui/src/agents/entry_builder.rs
pub fn build_entry(
    handle: &str,
    cfg: &CommandsConfig,
    cmd: &AvailableCommand,
) -> CommandEntry {
    let dispatch = match cfg.dispatch {
        DispatchKind::PromptText => Dispatch::PromptText {
            normalized: format!("/{}", cmd.name),
        },
        DispatchKind::VendorExec => Dispatch::VendorExec {
            method: cfg.exec_method.clone().expect("validator guarantees"),
            command: cmd.name.clone(),
            args_template: cfg.args_template,
        },
    };
    CommandEntry { /* … */ dispatch, /* … */ }
}
```

`Dispatch::KiroExecute` is renamed to `Dispatch::VendorExec { method, command, args_template }` — generic. `SubmitDecision::KiroExecute` likewise becomes `SubmitDecision::VendorExec { method, command, args }`. `Action::KiroExecute` becomes `Action::VendorExec { session, method, command, args }`. The kiro-specific identifiers in the TUI are deleted.

**`session_detail.rs:979` (KIRO_COMMANDS_AVAILABLE handler):** deleted. Replaced by a generic ingest loop driven by `CommandsConfig.ingest`:

```rust
SpurEventBody::AgentExtNotification { session, method, params } => {
    if session.0 != self.session_id.0 { return; }
    for binding in &self.agent_cfg.commands.ingest {
        if binding.method == *method {
            if let Some(entries) = run_ingest_hook(binding, params) {
                self.command_registry.set_agent_commands(&self.handle, entries);
            }
        }
    }
    for binding in &self.agent_cfg.commands.response {
        if binding.method == *method {
            run_response_hook(binding, self, params);
        }
    }
}
```

`run_ingest_hook` and `run_response_hook` dispatch on the enum variants and call the registered hook implementation.

**`session_detail.rs:987` (SPUR_KIRO_EXECUTE_RESPONSE):** falls out of the same response-binding loop above. The constant `SPUR_KIRO_EXECUTE_RESPONSE` in `spur-acp/src/ext.rs` stays (it's a wire-format constant), but `session_detail.rs` no longer references it by name.

### Validator

New module `agents/validator.rs`, invoked once at app startup by `App::new` before any agent connections:

```rust
pub fn validate_agent_config(cfg: &AgentConfig) -> Result<(), Vec<ConfigError>> {
    let mut errors = Vec::new();

    // Rule 1: if dispatch = vendor_exec, exec_method must be set.
    if cfg.commands.dispatch == DispatchKind::VendorExec
        && cfg.commands.exec_method.is_none()
    {
        errors.push(ConfigError::VendorExecMissingMethod { agent: cfg.name.clone() });
    }

    // Rule 2: every ingest/response binding's enum values must be known.
    // (Deserialize already rejects unknowns; this is belt-and-suspenders
    // for future hook additions that might be dynamically registered.)

    // Rule 3: if permissions.skip = true, at least one mechanism must be
    // declared (args, session_mode, or defaulted L2 auto_approve).
    // L2 is always active (permission_tx = None short-circuits), so strictly
    // this rule is advisory — but we WARN if neither args nor session_mode
    // is present, per roadmap's "capability-vs-mechanism consistency".
    // This is follow-up F2, generalized.
    if cfg.permissions.skip
        && cfg.permissions.args.is_empty()
        && cfg.permissions.session_mode.is_none()
    {
        errors.push(ConfigError::SkipPermissionsNoExplicitMechanism {
            agent: cfg.name.clone(),
            note: "relying on L2 auto-approve only; consider setting args or session_mode".into(),
        });
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
```

Errors are split into `fatal` (refuse to start the agent) and `warning` (log but proceed). Rule 1 is fatal, Rule 3 is a warning.

### `spur config check` subcommand

New `spur-cli` subcommand (~80 LOC):

```rust
// crates/spur-cli/src/commands/config_check.rs
pub fn run_config_check() -> anyhow::Result<i32> {
    let cfg = load_config()?;
    let mut fatal_count = 0;
    let mut warn_count = 0;

    for entry in &cfg.agents.entries {
        match validator::validate_agent_config(entry) {
            Ok(()) => println!("✓ {}", entry.name),
            Err(errors) => {
                for e in errors {
                    if e.is_fatal() {
                        eprintln!("✗ {}: {}", entry.name, e);
                        fatal_count += 1;
                    } else {
                        eprintln!("⚠ {}: {}", entry.name, e);
                        warn_count += 1;
                    }
                }
            }
        }
    }

    if fatal_count > 0 { Ok(1) } else { Ok(0) }
}
```

Wired via `spur config check` in `spur-cli`'s clap subcommand tree.

## Data flow (after Spec 1)

```
.spur/config.toml → AgentConfig { commands, permissions, display, ... }
                       │
                       ├─► validator::validate_agent_config
                       │     └─ fatal? → refuse to start
                       │
                       ▼
                 App::new stores cfg per-session
                       │
    ┌──────────────────┼────────────────────┐
    ▼                  ▼                    ▼
markdown_stream     command registry    ext notification
(unchanged)              │                    │
                         │                    ▼
         ingress path: AvailableCommandsUpdate      ingest bindings loop
                         │                    │
                         │                    ▼
                         │         build_entry(handle, cfg.commands, cmd)
                         │                    │
                         └────────────────────┤
                                              ▼
                         CommandRegistry stores Vec<CommandEntry>
                                              │
                                              ▼
                              SubmitRouter::route →
                              { Send, Local, VendorExec, Empty }
                                              │
                                              ▼
                              Action::VendorExec → UserInput::VendorExec
                                    (was Action::KiroExecute)
```

## Testing

**Unit tests (~6 new):**

1. `CommandsConfig` deserializes correctly for each supported dispatch shape (prompt_text, vendor_exec with full ingest + response).
2. `PermissionsConfig` deserializes + back-compat alias from top-level `skip_permissions*` fields works.
3. Unknown hook ID in config fails parse with a clear error.
4. `validator::validate_agent_config` catches `VendorExec` missing `exec_method`.
5. `validator::validate_agent_config` warns on `skip = true` with no explicit mechanism.
6. `build_entry` produces `Dispatch::PromptText` for prompt_text config, `Dispatch::VendorExec` for vendor_exec config — no hardcoded `if handle == "kiro"` path exists.

**Integration tests (~3 new):**

1. With kiro config block using `vendor_exec`, a `KIRO_COMMANDS_AVAILABLE`-shaped notification is ingested and commands appear in the registry.
2. With kiro config block, a `KIRO_COMMANDS_EXECUTE` response is rendered as a system note.
3. With claude-code-acp config block using `prompt_text`, `/compact` routes to `SubmitDecision::Send` with a `ContentBlock::Text("/compact")`.

**CLI test:** `spur config check` with a malformed config exits non-zero and names the offending agent + field.

**Retained tests:** all existing tests under `spur-tui/src/commands/` and `spur-acp/tests/` must continue to pass — the config migration is additive and defaults preserve current behavior for configs that don't use the new sub-tables.

## Affected files

**Create:**
- `crates/spur-tui/src/agents/mod.rs`
- `crates/spur-tui/src/agents/config.rs`
- `crates/spur-tui/src/agents/hooks.rs`
- `crates/spur-tui/src/agents/entry_builder.rs`
- `crates/spur-tui/src/agents/validator.rs`
- `crates/spur-cli/src/commands/config_check.rs`
- `docs/spur/agent-config.md` (reference doc for hook IDs and schema)
- `.spur/config.toml.example` (ships with full claude + kiro blocks)

**Modify:**
- `crates/spur-acp/src/config.rs` — nest `skip_permissions*` under `PermissionsConfig`, add `CommandsConfig`, `DisplayConfig`
- `crates/spur-tui/src/commands/entry.rs` — rename `Dispatch::KiroExecute` → `Dispatch::VendorExec`
- `crates/spur-tui/src/commands/registry.rs` — delete `agent_entry()`; delete `if handle == "kiro"` branch; accept `Vec<CommandEntry>` via `set_agent_commands`
- `crates/spur-tui/src/commands/submit_router.rs` — rename `SubmitDecision::KiroExecute` → `VendorExec`
- `crates/spur-tui/src/views/session_detail.rs` — replace hardcoded `KIRO_COMMANDS_AVAILABLE` / `SPUR_KIRO_EXECUTE_RESPONSE` match arms with generic ingest/response loops
- `crates/spur-tui/src/app.rs` — rename `Action::KiroExecute` → `VendorExec`; invoke validator at startup
- `crates/spur-tui/src/action.rs` — rename `KiroExecute` variant
- `crates/spur-cli/src/main.rs` — wire `config check` subcommand

**Delete (once migration grace period ends — out of scope for Spec 1 but flagged):**
- Nothing in Spec 1. Top-level `skip_permissions*` fields remain as `#[serde(alias)]` shims through v0.2.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Existing kiro users break when they don't add the new config block | Ship `.spur/config.toml.example` with a full kiro block; emit a startup warning that points them at it; keep top-level fields as deserialize aliases for one release |
| Enum variant renaming (`DispatchKind`) collides with snake_case config | `#[serde(rename_all = "snake_case")]` on each enum; explicit in the spec |
| Validator becomes a catch-all junk drawer | Spec 1 ships three rules; roadmap's success-criteria cap the validator surface. Spec 2+ each adds at most one new rule |
| Hook explosion (we talked about 7-10, could become 30) | Hooks compose (ingest takes `path` + `item_schema` — two hooks in one binding); roadmap tripwire explicitly limits non-declarative features |
| `spur config check` drift from runtime validation | Both paths call the same `validator::validate_agent_config` function — single source of truth |

## Success criteria (when Spec 1 is done)

- `grep "if handle == \"kiro\"" crates/` → zero matches
- `grep "KIRO_COMMANDS_AVAILABLE" crates/spur-tui/` → zero matches
- `grep "Dispatch::KiroExecute\|SubmitDecision::KiroExecute\|Action::KiroExecute" crates/` → zero matches
- `AgentConfig` has ≤ 9 top-level fields (down from 11; three skip_permissions fields fold into one `permissions` struct)
- `spur config check` exits non-zero on a broken config
- All existing kiro + claude tests pass, plus the 9 new tests listed above
- Roadmap follow-ups F2 and F5 both close

## What Spec 2 picks up

- `[agents.entries.mentions]` schema and the `MentionSource` plugin point
- Wiring an agent-advertised mention source (e.g., kiro specs or claude memories) as proof-of-concept
- Validator extension for mentions

## What Spec 3 picks up

- Codex onboarding as a pure config addition (no new hooks, reuses `prompt_text` + `stream-json` transport)
- Cookbook documentation for future agent onboarding
