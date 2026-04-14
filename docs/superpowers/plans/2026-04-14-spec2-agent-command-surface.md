# Spec 2 — Agent Command Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish decoupling the command dispatch pipeline from kiro. Generalize the orchestrator's `VendorExec` handler to a pure `(method, params)` pipe, add `[[commands.static]]` config declarations so agents can surface commands at startup, and delete the last hardcoded kiro wire constants.

**Architecture:**
1. **Pillar 1 — `VendorExec` becomes a generic pipe.** `InteractiveInput::VendorExec` / `UserInput::VendorExec` / `Action::VendorExec` / `SubmitDecision::VendorExec` / `Dispatch::VendorExec` all carry `(method, params: Value)` instead of `(method, command, args)`. Params-shaping moves from the orchestrator into `submit_router::route` under `args_template`. The orchestrator injects `sessionId` and emits the response as `AgentExtNotification { method: "{method}/response", … }`. This deletes `SPUR_KIRO_EXECUTE_RESPONSE`.
2. **Pillar 2 — static command declarations.** `[[agents.entries.commands.static]]` TOML blocks declare `(name, description, hint?)`. Deserialized into `StaticCommandDecl`, held on `CommandsConfig::static_commands`. `CommandRegistry` gains a `from_configs(&[AgentConfig])` constructor and three internal sources: `spur_local`, `static_commands`, `dynamic_commands`. Dynamic entries override statics on `(handle, name)` match; cross-agent collisions still use the existing prefix logic.

**Tech Stack:** Rust (spur-acp · spur-core · spur-cli · spur-tui), serde TOML, tokio runtime.

---

## Pre-flight

- [ ] Confirm you're on `main` with a clean tree; Spec 1 commits `ddf1d9a..81a5974` are reachable.
- [ ] Confirm these are present (Spec 1 done): `InteractiveInput::VendorExec`, `UserInput::VendorExec`, `Action::VendorExec`, `SubmitDecision::VendorExec`, `Dispatch::VendorExec`, `DispatchKind::VendorExec`. This plan only changes their *field shapes*, not their names.

Quick sanity check:

```bash
grep -rn "VendorExec" crates/ | wc -l   # expect many hits
grep -rn "KiroExecute" crates/          # expect 0 hits outside comments/docs
```

If either fails, stop and reconcile with the reviewer before proceeding.

---

## Task 1: `StaticCommandDecl` type + `CommandsConfig.static_commands` + `AgentConfig::effective_handle`

**Why:** Additive foundation for Pillar 2. Also fills in `effective_handle`, which the docstrings on `AgentConfig::display` already *promise* but nobody implemented — today the TUI computes the handle inline as `agent_name.to_lowercase()` in `session_detail::agent_handle_for_commands`. Centralizing it on `AgentConfig` lets `from_configs` (Task 3) and `session_detail` share the same logic.

**Files:**
- Modify: `crates/spur-acp/src/config/entries.rs`
- Modify: `crates/spur-acp/src/config/mod.rs` (add `effective_handle` method; update `pub use entries::{…}` to export `StaticCommandDecl`)
- Modify: `crates/spur-acp/src/lib.rs` (re-export `StaticCommandDecl`)
- Modify: `crates/spur-tui/src/views/session_detail.rs` (replace `agent_handle_for_commands` inline body with call to `effective_handle` — low-risk cleanup)

- [ ] **Step 1: Write the failing deserialize test**

At the bottom of `crates/spur-acp/src/config/entries.rs` add (if the file already has a `#[cfg(test)]` module, append the tests inside it; otherwise create the module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_command_decl_deserialize_minimal() {
        let toml = r#"
            name = "compact"
            description = "Compact conversation history"
        "#;
        let decl: StaticCommandDecl = toml::from_str(toml).unwrap();
        assert_eq!(decl.name, "compact");
        assert_eq!(decl.description, "Compact conversation history");
        assert_eq!(decl.hint, None);
    }

    #[test]
    fn static_command_decl_deserialize_with_hint() {
        let toml = r#"
            name = "model"
            description = "Switch model"
            hint = "[model-name]"
        "#;
        let decl: StaticCommandDecl = toml::from_str(toml).unwrap();
        assert_eq!(decl.hint.as_deref(), Some("[model-name]"));
    }

    #[test]
    fn commands_config_static_key_deserializes() {
        // TOML key is `static` (reserved Rust word); serde rename must work.
        let toml = r#"
            dispatch = "prompt_text"
            [[static]]
            name = "compact"
            description = "Compact history"
            [[static]]
            name = "model"
            description = "Switch model"
            hint = "[name]"
        "#;
        let cfg: CommandsConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.static_commands.len(), 2);
        assert_eq!(cfg.static_commands[0].name, "compact");
        assert_eq!(cfg.static_commands[1].hint.as_deref(), Some("[name]"));
    }

    #[test]
    fn commands_config_static_default_empty() {
        let toml = r#"dispatch = "prompt_text""#;
        let cfg: CommandsConfig = toml::from_str(toml).unwrap();
        assert!(cfg.static_commands.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-acp config::entries::tests:: --no-fail-fast`
Expected: FAIL — `StaticCommandDecl` undefined and `CommandsConfig` has no `static_commands` field.

- [ ] **Step 3: Add the type and field**

In `crates/spur-acp/src/config/entries.rs`, append the new type (after `ResponseBinding`):

```rust
/// Commands declared in config, available before the agent connects.
/// Dispatch/args_template/etc. come from the parent `CommandsConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticCommandDecl {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub hint: Option<String>,
}
```

Extend `CommandsConfig` — add the field (keep existing fields exactly as they are):

```rust
    /// One entry per vendor-ext method whose response is rendered in the trace.
    #[serde(default)]
    pub response: Vec<ResponseBinding>,

    /// Commands declared in config, visible in the popup at startup before
    /// the agent connects. Dynamic commands (received via ingest) override
    /// these on `(handle, name)` match.
    #[serde(default, rename = "static")]
    pub static_commands: Vec<StaticCommandDecl>,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-acp config::entries::tests::`
Expected: PASS (4 tests).

- [ ] **Step 5: Add `AgentConfig::effective_handle`**

In `crates/spur-acp/src/config/mod.rs`, inside `impl AgentConfig`, add:

```rust
    /// The short handle used as `/handle:cmd` on collision and as the
    /// key under which an agent's commands register. Prefers
    /// `display.handle` when set, otherwise falls back to
    /// `name.to_lowercase()`.
    pub fn effective_handle(&self) -> String {
        self.display
            .handle
            .clone()
            .unwrap_or_else(|| self.name.to_lowercase())
    }
```

- [ ] **Step 6: Wire it through session_detail**

In `crates/spur-tui/src/views/session_detail.rs`, replace `agent_handle_for_commands`:

```rust
    pub(crate) fn agent_handle_for_commands(&self) -> String {
        self.agent_cfg.effective_handle()
    }
```

Note: keep the method (don't inline it at call sites) — it's a useful abstraction layer and is already exercised by tests.

- [ ] **Step 7: Verify re-exports**

Open `crates/spur-acp/src/config/mod.rs` — the `pub use entries::{ ... }` line uses an explicit list. Add `StaticCommandDecl` to it. Also open `crates/spur-acp/src/lib.rs` and add `StaticCommandDecl` to the `pub use config::{ ... }` list. Then:

```bash
cargo build -p spur-acp -p spur-tui
```

Expected: clean build, no dead-code warnings.

- [ ] **Step 8: Unit test for `effective_handle`**

Append to `crates/spur-acp/src/config/mod.rs` (inside an existing `#[cfg(test)] mod tests` block, or create one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_handle_prefers_display_handle() {
        let mut cfg = AgentConfig::with_defaults("ClaudeCode");
        cfg.display.handle = Some("cc".into());
        assert_eq!(cfg.effective_handle(), "cc");
    }

    #[test]
    fn effective_handle_falls_back_to_lowercased_name() {
        let cfg = AgentConfig::with_defaults("ClaudeCode");
        assert_eq!(cfg.effective_handle(), "claudecode");
    }
}
```

Run: `cargo test -p spur-acp config::tests::`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-acp/src/config/entries.rs crates/spur-acp/src/config/mod.rs crates/spur-acp/src/lib.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-acp): StaticCommandDecl + CommandsConfig.static_commands + AgentConfig::effective_handle"
```

---

## Task 2: Generalize `VendorExec` signature to `(method, params)`

**Why:** The orchestrator currently hardcodes `{sessionId, command, args}` as the params shape. For a second vendor-exec agent to work, params-shaping must move out of the orchestrator and into the config-driven `args_template` at the TUI layer. The orchestrator becomes a pure pipe that only knows how to inject `sessionId`.

**Touches six enum sites (already carry `method` after Spec 1 — this task only changes their payload fields):**

| Crate | File | Variant |
|---|---|---|
| spur-acp | (dispatch kind carrier) `CommandsConfig` | (no change — args_template already lives here) |
| spur-tui | `commands/entry.rs` | `Dispatch::VendorExec` |
| spur-tui | `commands/submit_router.rs` | `SubmitDecision::VendorExec` |
| spur-tui | `action.rs` | `Action::VendorExec` |
| spur-tui | `app.rs` | `UserInput::VendorExec` |
| spur-core | `orchestrator.rs` | `InteractiveInput::VendorExec` |

Each goes from `{ method: String, command: String, args: Value }` to `{ method: String, params: Value }`.

**Files:**
- Modify: `crates/spur-tui/src/commands/entry.rs`
- Modify: `crates/spur-tui/src/commands/submit_router.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/agents/entry_builder.rs` (panicking test + `command` field drop)
- Modify: `crates/spur-cli/src/main.rs` (bridge mapping)
- Modify: `crates/spur-core/src/orchestrator.rs` (handler + variant)
- Modify: `crates/spur-tui/tests/session_update_handling.rs` (may carry old field shape)
- Test: `crates/spur-tui/src/commands/submit_router.rs` (new routing test)
- Test: `crates/spur-core/tests/vendor_exec_handler.rs` (new — optional; skip if `orchestrator::run_interactive` isn't unit-testable without a real brain)

### Step 2.1: Update the submit_router to produce `params`

- [ ] **Step 2.1.1: Write failing test in submit_router**

Append to `crates/spur-tui/src/commands/submit_router.rs` inside the existing `mod sessions_slash_tests` (rename module to `mod tests` if you prefer):

```rust
    #[test]
    fn vendor_exec_raw_rest_produces_params_with_command_and_args() {
        use spur_acp::{ArgsTemplateKind, AvailableCommand, CommandsConfig, DispatchKind};

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = crate::agents::build_entry(
            "kiro",
            &cfg,
            &AvailableCommand::new("context", "Show context"),
        );
        registry.set_agent_commands("kiro", vec![entry]);

        let decision = route("/context some rest", &[], &registry, false);
        match decision {
            SubmitDecision::VendorExec { method, params } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(
                    params,
                    serde_json::json!({
                        "command": "context",
                        "args": { "raw": "some rest" },
                    })
                );
            }
            other => panic!("expected VendorExec, got {:?}", other),
        }
    }

    #[test]
    fn vendor_exec_raw_rest_empty_args_still_includes_command() {
        use spur_acp::{ArgsTemplateKind, AvailableCommand, CommandsConfig, DispatchKind};

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = crate::agents::build_entry(
            "kiro",
            &cfg,
            &AvailableCommand::new("help", "help"),
        );
        registry.set_agent_commands("kiro", vec![entry]);

        let decision = route("/help", &[], &registry, false);
        match decision {
            SubmitDecision::VendorExec { params, .. } => {
                assert_eq!(params, serde_json::json!({ "command": "help" }));
            }
            other => panic!("expected VendorExec, got {:?}", other),
        }
    }
```

- [ ] **Step 2.1.2: Run test → FAIL with field mismatch**

Run: `cargo test -p spur-tui commands::submit_router -- vendor_exec`
Expected: compile errors — `SubmitDecision::VendorExec` still has `{ method, command, args }`.

- [ ] **Step 2.1.3: Change `SubmitDecision::VendorExec` field shape**

In `crates/spur-tui/src/commands/submit_router.rs`:

```rust
    VendorExec {
        method: String,
        params: Value,
    },
```

And rewrite the dispatch arm inside `route()`:

```rust
                Dispatch::VendorExec { method, command, args_template } => {
                    let rest = rest_after_first_token(text);
                    let params = match args_template {
                        spur_acp::ArgsTemplateKind::RawRest => {
                            if rest.is_empty() {
                                serde_json::json!({ "command": command })
                            } else {
                                serde_json::json!({
                                    "command": command,
                                    "args": { "raw": rest },
                                })
                            }
                        }
                    };
                    SubmitDecision::VendorExec { method, params }
                }
```

Notice: `Dispatch::VendorExec` still has `command` and `args_template` inside the *builder*-side dispatch variant — those are metadata used at entry-build time. They're unchanged by Task 2. Only the *wire-side* triad (`SubmitDecision`, `Action`, `UserInput`, `InteractiveInput`) flips to `(method, params)`.

- [ ] **Step 2.1.4: Tests pass**

Run: `cargo test -p spur-tui commands::submit_router -- vendor_exec`
Expected: PASS — but other callers now fail to compile. Proceed to 2.2.

### Step 2.2: Thread `params` through the TUI

- [ ] **Step 2.2.1: Update `Action::VendorExec`**

`crates/spur-tui/src/action.rs`:

```rust
    VendorExec {
        session: spur_acp::SessionId,
        method: String,
        params: serde_json::Value,
    },
```

- [ ] **Step 2.2.2: Update `SessionDetailView`'s submit branch**

Open `crates/spur-tui/src/views/session_detail.rs`, find the `SubmitDecision::VendorExec` arm (search `VendorExec { method, command, args }`), and rewrite:

```rust
                        SubmitDecision::VendorExec { method, params } => {
                            Some(Action::VendorExec {
                                session: self.session_id.clone(),
                                method,
                                params,
                            })
                        }
```

- [ ] **Step 2.2.3: Update `UserInput::VendorExec` + app.rs Action handler**

`crates/spur-tui/src/app.rs`:

```rust
// enum UserInput
    VendorExec {
        session: spur_acp::SessionId,
        method: String,
        params: serde_json::Value,
    },
```

```rust
// Action handler arm
            Action::VendorExec { session, method, params } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::VendorExec {
                        session,
                        method,
                        params,
                    });
                }
            }
```

- [ ] **Step 2.2.4: Update CLI bridge**

`crates/spur-cli/src/main.rs` (search `UserInput::VendorExec`):

```rust
                        spur_tui::UserInput::VendorExec { session, method, params } => {
                            spur_core::InteractiveInput::VendorExec {
                                session,
                                method,
                                params,
                            }
                        }
```

- [ ] **Step 2.2.5: Update `InteractiveInput::VendorExec` + orchestrator handler**

`crates/spur-core/src/orchestrator.rs`:

```rust
    /// Invoke an agent vendor-extension RPC on the active brain session.
    /// No-op if there is no active brain session. The method name and params
    /// are chosen by the TUI's config-driven dispatch path — the
    /// orchestrator is agnostic to specific extensions. `sessionId` is
    /// injected into `params` here (the TUI doesn't know ACP session IDs).
    VendorExec {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
```

Handler body (replace the whole `InteractiveInput::VendorExec { … }` arm):

```rust
                InteractiveInput::VendorExec { session, method, mut params } => {
                    if let Some(b) = brain.as_mut() {
                        // Inject ACP session ID — TUI doesn't know it.
                        if let Some(obj) = params.as_object_mut() {
                            obj.insert(
                                "sessionId".into(),
                                serde_json::json!(b.acp_session_id),
                            );
                        }
                        match b.connection.call_ext(&method, params).await {
                            Ok(resp) => {
                                self.emit(SpurEvent::now(
                                    SpurEventBody::AgentExtNotification {
                                        session: session.clone(),
                                        method: format!("{}/response", method),
                                        params: resp,
                                    },
                                ));
                            }
                            Err(e) => {
                                warn!(
                                    brain = %b.brain_name,
                                    method = %method,
                                    error = %e,
                                    "vendor exec call failed"
                                );
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session,
                                    message: format!(
                                        "vendor exec `{}` failed: {}", method, e
                                    ),
                                }));
                            }
                        }
                    } else {
                        warn!(method = %method, "VendorExec received but no active brain session");
                    }
                }
```

Note: `SPUR_KIRO_EXECUTE_RESPONSE` is no longer imported. Leave the constant in `ext.rs` for now — Task 4 deletes it.

- [ ] **Step 2.2.6: Fix the entry_builder panic-test**

In `crates/spur-tui/src/agents/entry_builder.rs`, the existing test `vendor_exec_config_builds_vendor_exec_dispatch` destructures `Dispatch::VendorExec { method, command, args_template }` — this shape is *unchanged* (it's the builder-side, not wire-side). Leave it alone. The `other => panic!(…)` arm is fine.

- [ ] **Step 2.2.7: Fix existing tests that use the old `{command, args}` triad**

`crates/spur-tui/tests/session_update_handling.rs` — search for `SPUR_KIRO_EXECUTE_RESPONSE`. If any test builds a `SubmitDecision::VendorExec` or `Action::VendorExec` with the old shape, migrate. If tests only reference the wire constants, Task 4 will handle them; keep them compiling by replacing with string literals if needed (ugly but temporary).

Do a grep to find all sites:

```bash
grep -rn "command:" crates/spur-tui/src | grep -i vendor
grep -rn "SubmitDecision::VendorExec" crates/
grep -rn "Action::VendorExec" crates/
grep -rn "InteractiveInput::VendorExec" crates/
grep -rn "UserInput::VendorExec" crates/
```

Every hit must match the new `{ method, params }` shape.

- [ ] **Step 2.2.8: Build and run full test suite**

```bash
cargo build --workspace
cargo test --workspace --no-fail-fast
```

Expected: all tests pass. If `session_update_handling.rs` still fails because it asserts on `SPUR_KIRO_EXECUTE_RESPONSE` in an emitted `AgentExtNotification`, update the assertion to `"_kiro.dev/commands/execute/response"` — that's the new convention-derived method. (This foreshadows Task 4.)

- [ ] **Step 2.2.9: Commit**

```bash
git add -u
git commit -m "refactor(spur): VendorExec carries (method, params); orchestrator is a pure pipe"
```

---

## Task 3: `CommandRegistry` three-source refactor

**Why:** Today `CommandRegistry` has one source (dynamic per-agent). Spec 2 needs three: spur-local, static-from-config, and dynamic. Dynamic must override static on `(handle, name)` match so that when an agent comes online and advertises its real command set, it supersedes the config's fallback.

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs`
- Modify: `crates/spur-tui/src/agents/entry_builder.rs` (add `build_static_entry` sibling)
- Modify: `crates/spur-tui/src/agents/mod.rs` (re-export)

### Step 3.1: Add `build_static_entry`

- [ ] **Step 3.1.1: Write failing test**

Append to `crates/spur-tui/src/agents/entry_builder.rs` inside the `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn build_static_entry_prompt_text_dispatch() {
        use spur_acp::StaticCommandDecl;
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let decl = StaticCommandDecl {
            name: "compact".into(),
            description: "Compact history".into(),
            hint: None,
        };
        let entry = build_static_entry("codex", &cfg, &decl);
        assert_eq!(entry.name, "compact");
        assert_eq!(entry.description, "Compact history");
        match entry.dispatch {
            Dispatch::PromptText { normalized } => assert_eq!(normalized, "/compact"),
            other => panic!("expected PromptText, got {other:?}"),
        }
    }

    #[test]
    fn build_static_entry_vendor_exec_dispatch() {
        use spur_acp::{ArgsTemplateKind, StaticCommandDecl};
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let decl = StaticCommandDecl {
            name: "help".into(),
            description: "Help".into(),
            hint: None,
        };
        let entry = build_static_entry("kiro", &cfg, &decl);
        match entry.dispatch {
            Dispatch::VendorExec { method, command, args_template } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(command, "help");
                assert_eq!(args_template, ArgsTemplateKind::RawRest);
            }
            other => panic!("expected VendorExec, got {other:?}"),
        }
    }

    #[test]
    fn build_static_entry_preserves_hint() {
        use spur_acp::StaticCommandDecl;
        let cfg = CommandsConfig::default();
        let decl = StaticCommandDecl {
            name: "model".into(),
            description: "Switch model".into(),
            hint: Some("[name]".into()),
        };
        let entry = build_static_entry("codex", &cfg, &decl);
        assert_eq!(entry.hint.as_deref(), Some("[name]"));
    }
```

- [ ] **Step 3.1.2: Run → FAIL**

Run: `cargo test -p spur-tui agents::entry_builder -- build_static_entry`
Expected: FAIL — `build_static_entry` undefined.

- [ ] **Step 3.1.3: Implement `build_static_entry`**

Append to `crates/spur-tui/src/agents/entry_builder.rs` (outside `#[cfg(test)]`):

```rust
/// Like `build_entry` but sourced from a config-declared
/// `StaticCommandDecl`. The dispatch is derived from the parent
/// `CommandsConfig` — static decls inherit dispatch semantics from their
/// agent's `[commands]` block.
pub fn build_static_entry(
    handle: &str,
    cfg: &CommandsConfig,
    decl: &spur_acp::StaticCommandDecl,
) -> CommandEntry {
    let dispatch = match cfg.dispatch {
        DispatchKind::PromptText => Dispatch::PromptText {
            normalized: format!("/{}", decl.name),
        },
        DispatchKind::VendorExec => {
            let method = cfg
                .exec_method
                .clone()
                .expect("validator guarantees exec_method for vendor_exec");
            Dispatch::VendorExec {
                method,
                command: decl.name.clone(),
                args_template: cfg.args_template,
            }
        }
    };

    CommandEntry {
        name: decl.name.clone(),
        description: decl.description.clone(),
        hint: decl.hint.clone(),
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch,
    }
}
```

Expose it via `crates/spur-tui/src/agents/mod.rs`:

```rust
pub use entry_builder::{build_entry, build_static_entry};
```

- [ ] **Step 3.1.4: Tests pass**

Run: `cargo test -p spur-tui agents::entry_builder`
Expected: PASS.

### Step 3.2: Registry three-source refactor

- [ ] **Step 3.2.1: Write failing tests**

At the bottom of `crates/spur-tui/src/commands/registry.rs` create a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
    use spur_acp::{AgentConfig, CommandsConfig, DispatchKind, StaticCommandDecl};

    fn config_with_static(name: &str, handle: &str, statics: Vec<&str>) -> AgentConfig {
        let mut cfg = AgentConfig::with_defaults(name);
        cfg.display.handle = Some(handle.to_string());
        cfg.commands = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            static_commands: statics
                .into_iter()
                .map(|n| StaticCommandDecl {
                    name: n.into(),
                    description: format!("{n} desc"),
                    hint: None,
                })
                .collect(),
            ..Default::default()
        };
        cfg
    }

    #[test]
    fn from_configs_loads_static_commands_at_construction() {
        let cfg = config_with_static("codex", "codex", vec!["compact", "model"]);
        let registry = CommandRegistry::from_configs(&[cfg]);
        let names: Vec<_> = registry.list().iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"compact".to_string()));
        assert!(names.contains(&"model".to_string()));
    }

    #[test]
    fn from_configs_without_statics_is_empty() {
        let cfg = AgentConfig::with_defaults("codex");
        let registry = CommandRegistry::from_configs(&[cfg]);
        // Only spur-local commands present; no agent commands.
        assert!(registry.list().iter().all(|e| matches!(e.source, CommandSource::Spur)));
    }

    #[test]
    fn dynamic_overrides_static_on_same_handle_name() {
        let cfg = config_with_static("codex", "codex", vec!["compact"]);
        let mut registry = CommandRegistry::from_configs(&[cfg]);
        // Static /compact has description "compact desc". Now advertise
        // dynamic /compact with a different description.
        let dynamic = CommandEntry {
            name: "compact".into(),
            description: "DYNAMIC DESC".into(),
            hint: None,
            source: CommandSource::Agent { handle: "codex".into() },
            dispatch: Dispatch::PromptText { normalized: "/compact".into() },
        };
        registry.set_agent_commands("codex", vec![dynamic]);
        let compacts: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact")
            .collect();
        assert_eq!(compacts.len(), 1, "dynamic must replace static, not coexist");
        assert_eq!(compacts[0].description, "DYNAMIC DESC");
    }

    #[test]
    fn clearing_dynamic_reveals_static_again() {
        let cfg = config_with_static("codex", "codex", vec!["compact"]);
        let mut registry = CommandRegistry::from_configs(&[cfg]);
        // Override, then clear with empty list.
        let dynamic = CommandEntry {
            name: "compact".into(),
            description: "DYNAMIC".into(),
            hint: None,
            source: CommandSource::Agent { handle: "codex".into() },
            dispatch: Dispatch::PromptText { normalized: "/compact".into() },
        };
        registry.set_agent_commands("codex", vec![dynamic]);
        registry.set_agent_commands("codex", vec![]); // agent disconnected / no commands
        let compacts: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "compact")
            .collect();
        assert_eq!(compacts.len(), 1);
        assert_eq!(compacts[0].description, "compact desc", "static should reappear");
    }

    #[test]
    fn cross_agent_same_name_still_disambiguates_with_prefix() {
        let codex = config_with_static("codex", "codex", vec!["help"]);
        let kiro = config_with_static("kiro", "kiro", vec!["help"]);
        let registry = CommandRegistry::from_configs(&[codex, kiro]);
        let help_entries: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|e| e.name == "help")
            .collect();
        assert_eq!(help_entries.len(), 2);
        // Prefix disambiguation still works
        let codex_help = help_entries
            .iter()
            .find(|e| matches!(&e.source, CommandSource::Agent { handle } if handle == "codex"))
            .unwrap();
        assert_eq!(registry.canonical_typed_form(codex_help), "/codex:help");
    }
}
```

- [ ] **Step 3.2.2: Run → FAIL**

Run: `cargo test -p spur-tui commands::registry`
Expected: FAIL — `CommandRegistry::from_configs` undefined; dynamic-overrides-static not implemented.

- [ ] **Step 3.2.3: Refactor `CommandRegistry`**

Rewrite the struct + inherent impls in `crates/spur-tui/src/commands/registry.rs`:

```rust
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::entry::{CommandEntry, CommandSource};
use super::spur_local::SpurLocalSource;
use spur_acp::AgentConfig;

/// Merges spur-local, static (config), and dynamic (runtime) slash
/// commands.
///
/// Collision rules:
/// * Same `(handle, name)` across static + dynamic → dynamic wins.
/// * Different handles with the same `name` → popup shows both; resolver
///   uses prefix disambiguation via `canonical_typed_form`.
pub struct CommandRegistry {
    /// Per-agent static commands from `[[commands.static]]`.
    static_commands: Vec<(String, Vec<CommandEntry>)>,
    /// Per-agent commands received via ingest at runtime.
    dynamic_commands: Vec<(String, Vec<CommandEntry>)>,
    /// Lazy merged view. Rebuilt on any mutation.
    cache: RefCell<Option<CacheSnapshot>>,
}

struct CacheSnapshot {
    entries: Vec<CommandEntry>,
    colliding: HashSet<String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            static_commands: Vec::new(),
            dynamic_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }

    /// Build a registry pre-populated with static commands from `configs`.
    /// Static commands become visible in the popup before any agent
    /// connects; dynamic commands received later override these on
    /// `(handle, name)` match.
    pub fn from_configs(configs: &[AgentConfig]) -> Self {
        let static_commands = configs
            .iter()
            .filter(|c| !c.commands.static_commands.is_empty())
            .map(|c| {
                let handle = c.effective_handle();
                let entries = c
                    .commands
                    .static_commands
                    .iter()
                    .map(|decl| crate::agents::build_static_entry(&handle, &c.commands, decl))
                    .collect();
                (handle, entries)
            })
            .collect();
        Self {
            static_commands,
            dynamic_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }

    /// Replace the full dynamic command set for an agent handle. Entries
    /// are pre-built by the caller via `agents::build_entry`.
    pub fn set_agent_commands(&mut self, handle: &str, entries: Vec<CommandEntry>) {
        if let Some(slot) = self.dynamic_commands.iter_mut().find(|(h, _)| h == handle) {
            slot.1 = entries;
        } else {
            self.dynamic_commands.push((handle.to_string(), entries));
        }
        *self.cache.borrow_mut() = None;
    }

    fn ensure_cache(&self) {
        let mut slot = self.cache.borrow_mut();
        if slot.is_some() {
            return;
        }

        // Build (handle, name) → dynamic-entry index for O(1) override lookup.
        let mut dynamic_index: HashMap<(&str, &str), &CommandEntry> = HashMap::new();
        for (handle, entries) in &self.dynamic_commands {
            for e in entries {
                dynamic_index.insert((handle.as_str(), e.name.as_str()), e);
            }
        }

        let mut entries = SpurLocalSource::entries();

        // Static entries — include only if not overridden by a dynamic entry
        // at the same (handle, name).
        for (handle, statics) in &self.static_commands {
            for s in statics {
                if !dynamic_index.contains_key(&(handle.as_str(), s.name.as_str())) {
                    entries.push(s.clone());
                }
            }
        }

        // Dynamic entries — always included.
        for (_handle, dyn_entries) in &self.dynamic_commands {
            entries.extend(dyn_entries.iter().cloned());
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut colliding: HashSet<String> = HashSet::new();
        for e in &entries {
            if !seen.insert(e.name.clone()) {
                colliding.insert(e.name.clone());
            }
        }
        *slot = Some(CacheSnapshot { entries, colliding });
    }

    pub fn list(&self) -> Vec<CommandEntry> {
        self.ensure_cache();
        self.cache.borrow().as_ref().unwrap().entries.clone()
    }

    pub fn canonical_typed_form(&self, entry: &CommandEntry) -> String {
        self.ensure_cache();
        let colliding = self
            .cache
            .borrow()
            .as_ref()
            .unwrap()
            .colliding
            .contains(&entry.name);
        if colliding {
            match &entry.source {
                CommandSource::Spur => format!("/spur:{}", entry.name),
                CommandSource::Agent { handle } => format!("/{}:{}", handle, entry.name),
            }
        } else {
            format!("/{}", entry.name)
        }
    }

    pub fn resolve(&self, text: &str) -> Option<CommandEntry> {
        let rest = text.strip_prefix('/')?;
        let first_token = rest.split_whitespace().next()?;
        self.ensure_cache();
        let cache = self.cache.borrow();
        let entries = &cache.as_ref().unwrap().entries;
        if let Some((source, name)) = first_token.split_once(':') {
            return entries.iter().find(|e| {
                e.name == name
                    && match (&e.source, source) {
                        (CommandSource::Spur, "spur") => true,
                        (CommandSource::Agent { handle }, s) => handle == s,
                        _ => false,
                    }
            }).cloned();
        }
        let mut candidates: Vec<_> = entries.iter().filter(|e| e.name == first_token).collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|e| match &e.source {
            CommandSource::Spur => 0,
            CommandSource::Agent { .. } => 1,
        });
        candidates.into_iter().next().cloned()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

Note the renamed internal field `agent_commands` → `dynamic_commands`. If any external caller referenced it through a public accessor, update accordingly. (`list()` + `set_agent_commands()` are the public surface.)

- [ ] **Step 3.2.4: Tests pass**

Run: `cargo test -p spur-tui commands::registry`
Expected: 5 new tests PASS.

- [ ] **Step 3.2.5: Full workspace build**

Run: `cargo test --workspace --no-fail-fast`
Expected: everything green.

- [ ] **Step 3.2.6: Commit**

```bash
git add -u
git commit -m "refactor(spur-tui): CommandRegistry three-source merge (spur-local + static + dynamic)"
```

---

## Task 4: Wire static commands at startup + delete kiro wire constants

**Why:** Task 3 added the capacity for `from_configs`, but the TUI's `App` still constructs the registry with `CommandRegistry::new()`. This task rewires `App` to use `from_configs(&configs)` and migrates `.spur/config.toml.example` so kiro's response binding matches the new `{method}/response` convention. With that in place, `KIRO_COMMANDS_EXECUTE` and `SPUR_KIRO_EXECUTE_RESPONSE` are fully dead and can be deleted.

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (registry construction)
- Modify: `.spur/config.toml.example`
- Modify: `crates/spur-acp/src/ext.rs` (delete constants)
- Modify: `crates/spur-acp/src/lib.rs` (remove re-exports of deleted constants, if any)
- Modify: `crates/spur-tui/tests/session_update_handling.rs` (replace constant refs with string literals, or delete tests now meaningless)
- Modify: `docs/spur/agent-config.md` (new `[[commands.static]]` section + response-method convention)

- [ ] **Step 4.1: Find the registry-construction site in `App::new`/`new_with_config`**

```bash
grep -n "CommandRegistry::new\|CommandRegistry::default" crates/spur-tui/src/app.rs
```

Expected: one or two hits.

- [ ] **Step 4.2: Rewrite registry construction to use `from_configs`**

Within `App::new_with_config`, replace the `CommandRegistry::new()` call with:

```rust
let registry = spur_tui::commands::CommandRegistry::from_configs(
    &spur_config.agents.entries,
);
```

(Exact path may vary — match the existing import style.) For `App::new` (the no-config fallback used in tests / `new_app`), keep `CommandRegistry::new()`. Alternatively, if `App::new_with_config` is the only prod path, delete or deprecate the legacy `new`. Check test_support usage first; keep back-compat if it's invoked from tests.

- [ ] **Step 4.3: Migrate `.spur/config.toml.example`**

In the kiro block, replace the response binding's method:

```diff
 [[agents.entries.commands.response]]
-method = "_spur.dev/kiro/execute/response"
+method = "_kiro.dev/commands/execute/response"
 render = "system_note"
```

Also append a `[[agents.entries.commands.static]]` example inside the kiro block (keeps the config reference illustrative):

```toml
[[agents.entries.commands.static]]
name = "help"
description = "Show kiro help"
```

And add a new top-level codex example block (commented out by default — users uncomment as needed) demonstrating pure-config onboarding:

```toml
# # Codex — zero-Rust onboarding example. Uncomment to enable.
# [[agents.entries]]
# name = "codex"
# command = "codex"
# args = ["--full-auto"]
# transport = "stream_json"
# role = "worker"
#
# [agents.entries.display]
# handle = "codex"
#
# [agents.entries.commands]
# dispatch = "prompt_text"
#
# [[agents.entries.commands.static]]
# name = "compact"
# description = "Compact conversation history"
#
# [[agents.entries.commands.static]]
# name = "model"
# description = "Switch model"
# hint = "[model-name]"
```

- [ ] **Step 4.4: Delete `KIRO_COMMANDS_EXECUTE` + `SPUR_KIRO_EXECUTE_RESPONSE`**

Edit `crates/spur-acp/src/ext.rs` — delete lines 11–18 (keeping `KIRO_COMMANDS_AVAILABLE`). Final file content:

```rust
//! Vendor-extension method names used across spur.
//!
//! The ACP protocol reserves `_<vendor>.dev/...` methods for out-of-spec
//! features. These constants keep the wire-format strings in one place.

/// Kiro vendor extension — notification: available commands advertised by kiro.
///
/// Payload shape: `{ sessionId: String, availableCommands: [AvailableCommand] }`.
pub const KIRO_COMMANDS_AVAILABLE: &str = "_kiro.dev/commands/available";
```

- [ ] **Step 4.5: Fix now-broken references**

```bash
grep -rn "KIRO_COMMANDS_EXECUTE\|SPUR_KIRO_EXECUTE_RESPONSE" crates/
```

All surviving hits live in test files. In each, replace the constant reference with the literal string form that matches the new wire convention:

| Old constant | Replacement literal |
|---|---|
| `KIRO_COMMANDS_EXECUTE` | `"_kiro.dev/commands/execute"` |
| `SPUR_KIRO_EXECUTE_RESPONSE` | `"_kiro.dev/commands/execute/response"` |

For any test assertion that checks the emitted response method, ensure it asserts the new `{method}/response` form.

- [ ] **Step 4.6: Update `lib.rs` re-exports if needed**

```bash
grep -n "KIRO_COMMANDS_EXECUTE\|SPUR_KIRO_EXECUTE_RESPONSE" crates/spur-acp/src/lib.rs
```

If found, remove those re-export entries.

- [ ] **Step 4.7: Update docs/spur/agent-config.md**

Read the current file. Add a new subsection under the commands block docs, e.g.:

```markdown
#### `[[agents.entries.commands.static]]` — config-declared commands

Static commands appear in the `/` popup at startup, before the agent
connects. Each entry is a triple:

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Command name without leading `/` |
| `description` | yes | Shown in popup |
| `hint` | no | Argument placeholder, e.g. `[model-name]` |

Dispatch is inherited from the parent `[commands]` block — static decls
don't repeat it. For `dispatch = "vendor_exec"`, static commands dispatch
via the same `exec_method` + `args_template` as dynamic ones. When an
agent advertises a command with the same name (via the `ingest` hook),
the dynamic entry overrides the static one on `(handle, name)` match.

Typical use: agents that don't expose a discovery endpoint (codex,
gemini-cli) still get a usable command menu.

#### Response-method convention

For `dispatch = "vendor_exec"`, `[[commands.response]]` methods follow
`{exec_method}/response` — e.g. exec `_kiro.dev/commands/execute`
produces responses at `_kiro.dev/commands/execute/response`. The
orchestrator is the source of truth: it appends `/response` to the
method string when re-emitting the call result as an
`AgentExtNotification`.
```

Exact placement depends on the existing doc structure — integrate in the right neighborhood.

- [ ] **Step 4.8: Full build + test**

```bash
cargo test --workspace --no-fail-fast
```

Expected: all green. Verify success criteria greps:

```bash
grep -rn "KiroExecute" crates/                  # 0 hits
grep -rn "KIRO_COMMANDS_EXECUTE" crates/        # 0 hits
grep -rn "SPUR_KIRO_EXECUTE_RESPONSE" crates/   # 0 hits
```

All three must be empty.

- [ ] **Step 4.9: Commit**

```bash
git add -u
git commit -m "feat(spur): wire static commands at startup; delete kiro wire constants"
```

---

## Task 5: Integration tests + spec success-criteria validation

**Why:** Spec §Testing calls for two integration tests that cross the stack. These guard against regressions in the dispatch pipeline and prove the codex-shaped "zero-Rust" path works.

**Files:**
- Create: `crates/spur-tui/tests/static_command_end_to_end.rs`
- Create: `crates/spur-tui/tests/codex_prompt_text_dispatch.rs` (or merge both into one file)

- [ ] **Step 5.1: Write integration test — static command routes through submit_router → Action → UserInput**

Create `crates/spur-tui/tests/static_command_end_to_end.rs`:

```rust
//! Integration test: a static vendor_exec command declared in config
//! becomes a VendorExec Action with the correct params shape when
//! submitted. This exercises Registry → submit_router → Action in one
//! test, which (per Spec 2) should go green without any code changes
//! beyond the 5-task plan.

use spur_acp::{
    AgentConfig, ArgsTemplateKind, CommandsConfig, DispatchKind, StaticCommandDecl,
};
use spur_tui::commands::submit_router::{route, SubmitDecision};
use spur_tui::commands::CommandRegistry;

fn kiro_config_with_static_help() -> AgentConfig {
    let mut cfg = AgentConfig::with_defaults("kiro");
    cfg.display.handle = Some("kiro".into());
    cfg.commands = CommandsConfig {
        dispatch: DispatchKind::VendorExec,
        exec_method: Some("_kiro.dev/commands/execute".into()),
        args_template: ArgsTemplateKind::RawRest,
        static_commands: vec![StaticCommandDecl {
            name: "help".into(),
            description: "Show kiro help".into(),
            hint: None,
        }],
        ..Default::default()
    };
    cfg
}

#[test]
fn static_vendor_exec_command_routes_to_submit_decision_vendor_exec() {
    let cfg = kiro_config_with_static_help();
    let registry = CommandRegistry::from_configs(&[cfg]);

    // Static /help must be resolvable before any set_agent_commands call.
    let decision = route("/help", &[], &registry, false);
    match decision {
        SubmitDecision::VendorExec { method, params } => {
            assert_eq!(method, "_kiro.dev/commands/execute");
            assert_eq!(params, serde_json::json!({ "command": "help" }));
        }
        other => panic!("expected VendorExec, got {:?}", other),
    }
}

#[test]
fn static_prompt_text_command_routes_to_send_with_text() {
    // Codex-shaped: dispatch = prompt_text, static /compact → Send {Text("/compact")}.
    let mut cfg = AgentConfig::with_defaults("codex");
    cfg.display.handle = Some("codex".into());
    cfg.commands = CommandsConfig {
        dispatch: DispatchKind::PromptText,
        static_commands: vec![StaticCommandDecl {
            name: "compact".into(),
            description: "Compact history".into(),
            hint: None,
        }],
        ..Default::default()
    };
    let registry = CommandRegistry::from_configs(&[cfg]);

    let decision = route("/compact", &[], &registry, false);
    match decision {
        SubmitDecision::Send { blocks, .. } => {
            let text = spur_tui::commands::submit_router::blocks_preview(&blocks);
            assert_eq!(text, "/compact");
        }
        other => panic!("expected Send, got {:?}", other),
    }
}
```

Adjust the import paths if `submit_router` / `CommandRegistry` are private — in that case, expose through `pub use` on the crate root or use `#[cfg(test)]` helpers already present in `test_support`.

- [ ] **Step 5.2: Run → PASS (the whole point is that Tasks 1–4 already implement this)**

Run: `cargo test -p spur-tui --test static_command_end_to_end`
Expected: PASS. If FAIL, the failure points at a regression in Tasks 1–4; fix there, not in the test.

- [ ] **Step 5.3: Verify success criteria from Spec 2 §Success criteria**

Run the three greps one more time plus the broader workspace test:

```bash
grep -rn "KiroExecute" crates/                  # 0
grep -rn "KIRO_COMMANDS_EXECUTE" crates/        # 0
grep -rn "SPUR_KIRO_EXECUTE_RESPONSE" crates/   # 0
cargo test --workspace --no-fail-fast           # all green
```

- [ ] **Step 5.4: Commit**

```bash
git add -u
git commit -m "test(spur-tui): integration tests for static-command end-to-end dispatch"
```

---

## Self-review

After all tasks land, run one sweep:

1. **Greps:** the three success-criteria patterns above return zero.
2. **Types consistent:** `SubmitDecision::VendorExec { method, params }` matches `Action::VendorExec { session, method, params }` matches `UserInput::VendorExec { session, method, params }` matches `InteractiveInput::VendorExec { session, method, params }`.
3. **Dispatch::VendorExec is unchanged:** builder-side metadata still has `{ method, command, args_template }`. Those `command` + `args_template` fields are read inside `submit_router::route` to shape `params`; they must not be conflated with the wire-side triad.
4. **Static ≠ dynamic:** static entries stored in `CommandRegistry::static_commands`; dynamic in `dynamic_commands`. `set_agent_commands` only touches the dynamic slot. `from_configs` only populates the static slot.
5. **Spec §What Spec 3 picks up** is intentionally out of scope — no new agent configs in Spec 2 code (only `.spur/config.toml.example` gains a commented codex block).

---

## Risks & recovery

| Risk | Recovery |
|---|---|
| Task 2 breaks `session_update_handling.rs` tests mid-refactor | Update test assertions to the new response method in the same commit — don't split. |
| `App::new` without config is still used by `test_support::new_app` and callers | Keep `App::new` calling `CommandRegistry::new()` (empty). Only `new_with_config` uses `from_configs`. |
| `StaticCommandDecl::hint` semantics diverge from `AvailableCommandInput::Unstructured.hint` | They don't — both surface as `Option<String>` on `CommandEntry::hint`. If drift appears, `build_static_entry` is where to reconcile. |
| `effective_handle` not implemented on `AgentConfig` yet | It is — added in Spec 1. Verify: `grep -n "fn effective_handle" crates/spur-acp/src/config/mod.rs`. If missing, stop and fix Spec 1's delta first. |

---

## Execution

This plan is ready for **superpowers:subagent-driven-development**. Each task commits independently; reviewers can validate per task.
