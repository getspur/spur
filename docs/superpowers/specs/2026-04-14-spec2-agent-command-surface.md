# Agent Command Execution Surface — Spec 2

**Status:** design
**Date:** 2026-04-14
**Roadmap:** `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md`
**Depends on:** Spec 1 (`2026-04-14-agent-config-foundation-design.md`)
**Area:** `spur-core` orchestrator · `spur-cli` bridge · `spur-acp` config · `spur-tui` command registry

## Problem

Spec 1 makes the TUI layer config-driven: `Dispatch::VendorExec` replaces `Dispatch::KiroExecute`, the ingest/response notification router is generic, and the command registry builds entries from config. Two problems remain:

1. **The runtime pipeline is still kiro-specific.** `InteractiveInput::KiroExecute` in the orchestrator calls a hardcoded wire method (`KIRO_COMMANDS_EXECUTE`) and emits a hardcoded synthetic response method (`SPUR_KIRO_EXECUTE_RESPONSE`). A second vendor-exec agent would require adding another variant.

2. **Commands require runtime discovery.** If an agent doesn't send a vendor-ext notification advertising commands, the popup is empty for that agent. Non-ACP agents (codex, gemini-cli) have no mechanism to surface commands at all.

These two gaps block the roadmap's onboarding contract: "best case — one config block, no code."

### Hardcoded touchpoints (9 files, post-Spec 1 residuals)

| # | File | Line(s) | What |
|---|---|---|---|
| 1 | `spur-acp/src/ext.rs` | 14, 18 | `KIRO_COMMANDS_EXECUTE` + `SPUR_KIRO_EXECUTE_RESPONSE` constants |
| 2 | `spur-core/src/orchestrator.rs` | 81 | `InteractiveInput::KiroExecute` variant |
| 3 | `spur-core/src/orchestrator.rs` | 523, 532 | Handler calls `call_ext(KIRO_COMMANDS_EXECUTE, …)` |
| 4 | `spur-core/src/orchestrator.rs` | 539 | Emits `SPUR_KIRO_EXECUTE_RESPONSE` |
| 5 | `spur-cli/src/main.rs` | 417–418 | CLI bridge maps `UserInput::KiroExecute` → `InteractiveInput::KiroExecute` |

Spec 1's affected-files list does not include `orchestrator.rs` or the CLI bridge mapping. Its success criteria grep for `Dispatch::KiroExecute`, `SubmitDecision::KiroExecute`, and `Action::KiroExecute` — but not `InteractiveInput::KiroExecute`. These are definitively Spec 2 scope.

## Goals

1. **Generalize `KiroExecute` → `VendorExec` end-to-end** — orchestrator and CLI bridge become generic pipes forwarding `(method, params)`.
2. **Static command declaration** — `[[agents.entries.commands.static]]` lets agents declare commands in config. Commands appear in the popup at startup, before the agent connects.
3. **Three-source merge** — `CommandRegistry` merges spur-local + static (config) + dynamic (runtime) with clear precedence.
4. **Delete all kiro-specific constants from the runtime path.**

## Non-goals

- Command argument validation or completion.
- Command groups, categories, or aliases.
- Config-driven spur-local commands.
- Agent disconnect → command cleanup (pre-existing, orthogonal).
- `CommandDiscoverySource` trait formalization (only if a 4th source appears).
- New agents (Spec 3+).

## Design

### Pillar 1 — VendorExec generalization

#### Rename chain

Spec 1 renames TUI-side variants. Spec 2 completes the chain through the runtime:

| Layer | Before | After |
|---|---|---|
| `spur-tui` app.rs | `UserInput::KiroExecute { session, command, args }` | `UserInput::VendorExec { session, method, params }` |
| `spur-cli` main.rs | maps KiroExecute → KiroExecute | maps VendorExec → VendorExec |
| `spur-core` orchestrator.rs | `InteractiveInput::KiroExecute { session, command, args }` | `InteractiveInput::VendorExec { session, method, params }` |

If Spec 1 already renames `UserInput`, Spec 2 verifies and moves on. The orchestrator handler is definitively Spec 2.

#### Orchestrator handler

Before (hardcoded method, hardcoded response, hardcoded params shape):

```rust
InteractiveInput::KiroExecute { session, command, args } => {
    let params = json!({
        "sessionId": b.acp_session_id,
        "command": command,
        "args": args,
    });
    let resp = b.connection
        .call_ext(KIRO_COMMANDS_EXECUTE, params).await;
    self.emit(AgentExtNotification {
        method: SPUR_KIRO_EXECUTE_RESPONSE.into(),
        params: resp,
    });
}
```

After (generic pipe):

```rust
InteractiveInput::VendorExec { session, method, mut params } => {
    if let Some(b) = brain.as_mut() {
        // Inject ACP session ID — TUI doesn't know it.
        if let Some(obj) = params.as_object_mut() {
            obj.insert("sessionId".into(), json!(b.acp_session_id));
        }
        match b.connection.call_ext(&method, params).await {
            Ok(resp) => {
                self.emit(SpurEvent::now(SpurEventBody::AgentExtNotification {
                    session: session.clone(),
                    method: format!("{}/response", method),
                    params: resp,
                }));
            }
            Err(e) => {
                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                    session,
                    message: format!("vendor-exec `{}` failed: {}", method, e),
                }));
            }
        }
    } else {
        warn!(method = %method, "VendorExec: no active brain session");
    }
}
```

Three changes:
- `method` arrives from caller (resolved from config at TUI layer).
- `params` pre-shaped by `args_template` hook at TUI layer; orchestrator injects `sessionId`.
- Response method derived by convention: `{method}/response`.

#### Response method convention

For kiro: `_kiro.dev/commands/execute` → response at `_kiro.dev/commands/execute/response`.

The TUI's response binding matches this:

```toml
[[agents.entries.commands.response]]
method = "_kiro.dev/commands/execute/response"
render = "system_note"
```

The old `SPUR_KIRO_EXECUTE_RESPONSE` constant (`_spur.dev/kiro/execute/response`) is deleted.

#### sessionId injection

The TUI layer doesn't know the ACP session ID (that's an orchestrator concern). The orchestrator injects it into `params` before `call_ext`. Single injection point, well-tested.

### Pillar 2 — Static command declaration

#### Config schema

```toml
[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"

[[agents.entries.commands.static]]
name = "model"
description = "Switch model"
hint = "[model-name]"
```

#### Rust type

```rust
// spur-acp/src/config/entries.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticCommandDecl {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub hint: Option<String>,
}
```

Added to `CommandsConfig`:

```rust
pub struct CommandsConfig {
    // ... existing fields from Spec 1 ...

    /// Commands declared in config, available before agent connects.
    #[serde(default, rename = "static")]
    pub static_commands: Vec<StaticCommandDecl>,
}
```

TOML key is `static` (reserved in Rust, hence `rename`).

#### CommandRegistry refactor

Three sources with clear precedence:

```rust
pub struct CommandRegistry {
    spur_local: Vec<CommandEntry>,
    static_commands: Vec<(String, Vec<CommandEntry>)>,  // (handle, entries)
    dynamic_commands: Vec<(String, Vec<CommandEntry>)>,  // (handle, entries)
    cache: RefCell<Option<CacheSnapshot>>,
}

impl CommandRegistry {
    pub fn from_configs(configs: &[AgentConfig]) -> Self {
        let static_commands = configs.iter()
            .filter(|c| !c.commands.static_commands.is_empty())
            .map(|c| {
                let handle = c.effective_handle();
                let entries = c.commands.static_commands.iter()
                    .map(|s| build_static_entry(&handle, &c.commands, s))
                    .collect();
                (handle, entries)
            })
            .collect();
        Self {
            spur_local: SpurLocalSource::entries(),
            static_commands,
            dynamic_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }
}
```

Static entries inherit `dispatch` from the parent `[commands]` block — no per-command dispatch needed.

#### Merge semantics

```
ensure_cache():
  1. spur-local entries
  2. + static entries (per agent)
  3. + dynamic entries (per agent) — override statics on (handle, name) match
  4. cross-agent collision → prefix disambiguation (existing logic)
```

Dynamic overwrites static for the same `(handle, name)`. Cross-agent collisions use the existing `/handle:name` prefix.

#### Timing

```
startup        → spur-local + static commands → popup ready immediately
agent connects → dynamic commands merge       → override statics (same handle+name)
agent disconnects → dynamic cleared           → statics reappear
```

### Wire constant cleanup

| Constant | File | Action |
|---|---|---|
| `KIRO_COMMANDS_EXECUTE` | ext.rs:14 | Delete — value lives in kiro config `exec_method` |
| `SPUR_KIRO_EXECUTE_RESPONSE` | ext.rs:18 | Delete — replaced by `{method}/response` convention |
| `KIRO_COMMANDS_AVAILABLE` | ext.rs:9 | Keep — still documents the wire format; no Rust code imports it after Spec 1 |

## Config examples

### Codex — zero-code onboarding

```toml
[[agents.entries]]
name = "codex"
command = "codex"
args = ["--full-auto"]
transport = "stream_json"
role = "worker"

[agents.entries.display]
handle = "codex"

[agents.entries.commands]
dispatch = "prompt_text"

[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"

[[agents.entries.commands.static]]
name = "model"
description = "Switch model"
hint = "[model-name]"
```

`/compact` and `/model` appear immediately. Dispatched as `ContentBlock::Text`. No Rust. No vendor-ext.

### Kiro — vendor-exec with dynamic + static fallback

```toml
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
method = "_kiro.dev/commands/execute/response"
render = "system_note"

[[agents.entries.commands.static]]
name = "help"
description = "Show kiro help"
```

`/help` visible before kiro connects. Once kiro advertises commands, dynamic entries override.

### Hypothetical new ACP agent — proves generalization

```toml
[agents.entries.commands]
dispatch = "vendor_exec"
exec_method = "_foo.dev/run"
args_template = "raw_rest"

[[agents.entries.commands.response]]
method = "_foo.dev/run/response"
render = "system_note"

[[agents.entries.commands.static]]
name = "status"
description = "Show agent status"
```

`/status` dispatched via `call_ext("_foo.dev/run", …)`. Response rendered as system note. Zero Rust.

## Migration

**Users:** kiro config response binding method updates:

```diff
 [[agents.entries.commands.response]]
-method = "_spur.dev/kiro/execute/response"
+method = "_kiro.dev/commands/execute/response"
 render = "system_note"
```

Missing response binding → startup warning with fix instructions.

**Developers:** `InteractiveInput::KiroExecute` no longer exists → compile error pointing to `VendorExec`.

## Testing

**Unit (8 new):**

1. `StaticCommandDecl` deserializes from `[[commands.static]]`.
2. `CommandRegistry::from_configs` loads static commands at construction.
3. Static commands appear in `list()` before any `set_agent_commands`.
4. After `set_agent_commands` with colliding name, dynamic wins.
5. After clearing dynamic commands, static reappears.
6. `InteractiveInput::VendorExec` with arbitrary method calls `call_ext` with that method.
7. Orchestrator injects `sessionId` into params.
8. Response emitted with `{method}/response`.

**Integration (2 new):**

1. Full round-trip: static command → submit → VendorExec → orchestrator → call_ext → response → system_note.
2. Codex-shaped config: `/compact` routes to `SubmitDecision::Send` with text.

## Affected files

**Modify:**

| File | Change |
|---|---|
| `spur-acp/src/config/entries.rs` | Add `StaticCommandDecl`, add `static_commands` to `CommandsConfig` |
| `spur-acp/src/ext.rs` | Delete `KIRO_COMMANDS_EXECUTE`, `SPUR_KIRO_EXECUTE_RESPONSE` |
| `spur-acp/src/lib.rs` | Update re-exports |
| `spur-core/src/orchestrator.rs` | `InteractiveInput::KiroExecute` → `VendorExec`, generic handler |
| `spur-cli/src/main.rs` | Update bridge mapping |
| `spur-tui/src/commands/registry.rs` | `from_configs`, three-source merge |
| `spur-tui/src/app.rs` | Pass configs to `CommandRegistry::from_configs` |

**Verify (done by Spec 1, confirm no regressions):**

| File | Expected state |
|---|---|
| `spur-tui/src/commands/entry.rs` | `Dispatch::VendorExec` exists |
| `spur-tui/src/commands/submit_router.rs` | `SubmitDecision::VendorExec` exists |
| `spur-tui/src/action.rs` | `Action::VendorExec` exists |
| `spur-tui/src/views/session_detail.rs` | Generic response binding loop exists |

## Success criteria

1. `grep -r "KiroExecute" crates/` → zero matches
2. `grep -r "KIRO_COMMANDS_EXECUTE" crates/` → zero matches
3. `grep -r "SPUR_KIRO_EXECUTE_RESPONSE" crates/` → zero matches
4. A `prompt_text` agent with `[[commands.static]]` shows commands without runtime discovery
5. A `vendor_exec` agent with non-kiro `exec_method` works end-to-end
6. Kiro regression tests pass
7. Codex config block works with zero Rust changes

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Spec 1 already renames some variants | Spec 2 verifies and completes; no conflict, just less work |
| `sessionId` injection in orchestrator is fragile | Single injection point, unit-tested |
| `{method}/response` convention is implicit | Documented here + config reference; explicit `response_method` field adds complexity for no practical gain |
| Static commands stale vs agent's real commands | Dynamic overrides statics — statics are fallback, not authoritative |
| Registry refactor breaks existing tests | Additive — `set_agent_commands` API preserved, `from_configs` is new constructor |

## What Spec 3 picks up

Codex onboarding as pure config: one `[[agents.entries]]` block with `transport = "stream_json"`, `dispatch = "prompt_text"`, and `[[commands.static]]` entries. Zero Rust. Spec 3 validates against a real codex binary and documents the onboarding cookbook.
