# Agent Config Foundation — Spec 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the three hardcoded kiro branches in `spur-tui` (registry, session_detail ingest, session_detail response) by replacing them with config-driven hook bindings in `.spur/config.toml`, and fold the three scattered `skip_permissions_*` fields into a `[agents.entries.permissions]` sub-table.

**Architecture:** Hook enums and validator live in `spur-acp` (parse-time + pure validation, linked by both TUI and CLI). Hook behavior (building CommandEntry, running ingest/response) lives in `spur-tui` where it has `AgentConnection`/`SessionDetailView` in scope. A new `agents/entry_builder.rs` module replaces the `if handle == "kiro"` branch; the `KIRO_COMMANDS_AVAILABLE` / `SPUR_KIRO_EXECUTE_RESPONSE` arms in `session_detail.rs` become generic loops over `agent_cfg.commands.{ingest, response}`. `Dispatch::KiroExecute`, `SubmitDecision::KiroExecute`, `Action::KiroExecute`, `UserInput::KiroExecute`, `InteractiveInput::KiroExecute` all become `VendorExec { method, command, args }` in one atomic rename.

**Tech Stack:** Rust, serde/toml, clap, agent_client_protocol crate, ratatui.

**Reference spec:** `docs/superpowers/specs/2026-04-14-agent-config-foundation-design.md`
**Reference roadmap:** `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md`

---

## File structure after Spec 1

**Created:**

- `crates/spur-acp/src/config/mod.rs` — (optional) re-exports if we split `config.rs` into a module
- `crates/spur-acp/src/config/hooks.rs` — `DispatchKind`, `ArgsTemplateKind`, `IngestParserKind`, `ItemSchemaKind`, `ResponseRenderKind` enums (deserialize-only; no behavior)
- `crates/spur-acp/src/config/entries.rs` — `CommandsConfig`, `IngestBinding`, `ResponseBinding`, `DisplayConfig`, `PermissionsConfig` structs
- `crates/spur-acp/src/config/validator.rs` — `validate_agent_config(&AgentConfig) -> Result<(), Vec<ConfigError>>`
- `crates/spur-tui/src/agents/mod.rs` — re-exports
- `crates/spur-tui/src/agents/entry_builder.rs` — `build_entry(handle, &CommandsConfig, &AvailableCommand) -> CommandEntry`
- `crates/spur-tui/src/agents/ingest.rs` — `run_ingest_hook(&IngestBinding, &Value) -> Option<Vec<AvailableCommand>>`
- `crates/spur-cli/src/commands/mod.rs`
- `crates/spur-cli/src/commands/config_check.rs` — `run_config_check(repo_root)`
- `docs/spur/agent-config.md` — hook vocabulary + schema reference
- `.spur/config.toml.example` — ships with full claude + kiro blocks

**Modified:**

- `crates/spur-acp/src/config.rs` — add `commands`, `display`, `permissions` fields; add `effective_permissions()` accessor; rewire `effective_args()` through it
- `crates/spur-acp/src/lib.rs` — export new config types & validator
- `crates/spur-core/src/skip_perm.rs` — read `cfg.effective_permissions()` instead of flat fields
- `crates/spur-core/src/orchestrator.rs` — rename `InteractiveInput::KiroExecute → VendorExec { method, command, args }`; update worker arm to use the explicit method
- `crates/spur-tui/src/commands/entry.rs` — `Dispatch::KiroExecute → VendorExec { method, command, args_template }`
- `crates/spur-tui/src/commands/submit_router.rs` — rename `SubmitDecision::KiroExecute → VendorExec`; carry `method` from the `Dispatch::VendorExec` variant
- `crates/spur-tui/src/commands/registry.rs` — delete `agent_entry()`; registry holds `Vec<CommandEntry>` per handle rather than `Vec<AvailableCommand>`
- `crates/spur-tui/src/action.rs` — rename `Action::KiroExecute → VendorExec`
- `crates/spur-tui/src/app.rs` — rename `UserInput::KiroExecute → VendorExec`; rewire `Action::VendorExec` arm
- `crates/spur-tui/src/views/session_detail.rs` — add `agent_cfg: Arc<AgentConfig>`; replace the `if method == KIRO_COMMANDS_AVAILABLE` block with generic ingest/response loops; wire SubmitRouter decision `VendorExec { method, ... }` through
- `crates/spur-tui/src/views/session_detail.rs::new(...)` — gains `agent_cfg: Arc<AgentConfig>` parameter
- `crates/spur-cli/src/main.rs` — add `Commands::Config { command: ConfigCommands }` enum; wire `config check` subcommand
- `crates/spur-tui/tests/session_update_handling.rs` — construct kiro `AgentConfig` with ingest binding; assert registry populated via generic path
- `crates/spur-acp/tests/skip_permissions_config.rs` — add nested-shape & migration-equivalence tests

**Deleted:** nothing in Spec 1. Top-level `skip_permissions*` fields remain available as serde aliases.

---

## Task 1: Add PermissionsConfig + CommandsConfig + DisplayConfig types (schema only, not yet used)

**Files:**
- Create: `crates/spur-acp/src/config/hooks.rs`
- Create: `crates/spur-acp/src/config/entries.rs`
- Modify: `crates/spur-acp/src/config.rs` (convert single-file module into `config/` directory with `config/mod.rs` facade, or add inline — see step 1)
- Modify: `crates/spur-acp/src/lib.rs` (re-export new types)
- Test: `crates/spur-acp/tests/nested_config_shape.rs` (new)

- [ ] **Step 1: Decide module layout.**

`crates/spur-acp/src/config.rs` is 347 lines with multiple structs. Keep as a single file: add `mod hooks_inline;` isn't great. Instead, convert to a module:

```bash
mkdir -p crates/spur-acp/src/config
git mv crates/spur-acp/src/config.rs crates/spur-acp/src/config/mod.rs
```

Add sub-modules in subsequent steps.

- [ ] **Step 2: Write the failing test for the hook enums.**

Create `crates/spur-acp/tests/nested_config_shape.rs`:

```rust
//! Spec 1: parse `[agents.entries.commands]`, `[agents.entries.permissions]`,
//! and `[agents.entries.display]` sub-tables.

use spur_acp::config::{
    AgentConfig, ArgsTemplateKind, DispatchKind, IngestParserKind, ItemSchemaKind,
    ResponseRenderKind,
};

#[test]
fn parses_prompt_text_commands_block() {
    let toml_src = r#"
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"

[display]
handle = "claude"
display_name = "Claude"

[commands]
dispatch = "prompt_text"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.display.handle.as_deref(), Some("claude"));
    assert_eq!(cfg.commands.dispatch, DispatchKind::PromptText);
    assert!(cfg.commands.exec_method.is_none());
    assert!(cfg.commands.ingest.is_empty());
    assert!(cfg.commands.response.is_empty());
}

#[test]
fn parses_vendor_exec_commands_block() {
    let toml_src = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[commands]
dispatch = "vendor_exec"
exec_method = "_kiro.dev/commands/execute"
args_template = "raw_rest"

[[commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"

[[commands.response]]
method = "_kiro.dev/commands/execute"
render = "system_note"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.commands.dispatch, DispatchKind::VendorExec);
    assert_eq!(
        cfg.commands.exec_method.as_deref(),
        Some("_kiro.dev/commands/execute")
    );
    assert_eq!(cfg.commands.args_template, ArgsTemplateKind::RawRest);
    assert_eq!(cfg.commands.ingest.len(), 1);
    assert_eq!(cfg.commands.ingest[0].parser, IngestParserKind::JsonPathList);
    assert_eq!(cfg.commands.ingest[0].item_schema, ItemSchemaKind::AcpAvailableCommand);
    assert_eq!(cfg.commands.response.len(), 1);
    assert_eq!(cfg.commands.response[0].render, ResponseRenderKind::SystemNote);
}

#[test]
fn unknown_dispatch_kind_is_rejected() {
    let toml_src = r#"
name = "bogus"
command = "bogus-cli"
transport = "acp"

[commands]
dispatch = "teleport"
"#;
    let err = toml::from_str::<AgentConfig>(toml_src).expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("teleport") || msg.contains("unknown variant"),
        "expected unknown-variant error, got: {msg}"
    );
}

#[test]
fn permissions_nested_block_parses() {
    let toml_src = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[permissions]
skip = true
args = ["--trust-all-tools"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(cfg.permissions.skip);
    assert_eq!(cfg.permissions.args, vec!["--trust-all-tools".to_string()]);
    assert!(cfg.permissions.session_mode.is_none());
}
```

- [ ] **Step 3: Run the test to verify it fails (types don't exist yet).**

Run: `cargo test -p spur-acp --test nested_config_shape`
Expected: FAIL — `unresolved import spur_acp::config::DispatchKind` (and sibling unresolved types).

- [ ] **Step 4: Create the hook enums module.**

Create `crates/spur-acp/src/config/hooks.rs`:

```rust
//! Hook-ID enums for `[agents.entries.commands]`. Strongly typed on purpose
//! — serde rejects unknown variants at parse time, so a typo in
//! `.spur/config.toml` fails loudly with a clear error instead of silently
//! falling back.
//!
//! Each enum names a built-in hook registered in `spur-tui` (see
//! `crates/spur-tui/src/agents/`). The enum lives here because spur-acp
//! owns the config schema and must validate it at load time; hook *behavior*
//! lives in spur-tui where it has AgentConnection / SessionDetailView in
//! scope.

use serde::{Deserialize, Serialize};

/// How a selected slash-command is delivered to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchKind {
    /// Send `ContentBlock::Text("/name args")` on the agent's prompt stream.
    PromptText,
    /// Call a vendor extension RPC with a typed args payload.
    VendorExec,
}

impl Default for DispatchKind {
    fn default() -> Self {
        Self::PromptText
    }
}

/// How `/cmd rest-of-line` turns into the RPC args payload for vendor_exec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsTemplateKind {
    /// `"/cmd rest…"` → `{ "args": { "raw": "rest…" } }`. Today's kiro behavior.
    RawRest,
}

impl Default for ArgsTemplateKind {
    fn default() -> Self {
        Self::RawRest
    }
}

/// How to decode a vendor-ext notification payload into items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestParserKind {
    /// Look up `params[path]`, expect an array, decode each element via `item_schema`.
    JsonPathList,
}

/// Schema describing each element of a decoded ingest list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSchemaKind {
    /// `serde_json::from_value::<Vec<agent_client_protocol::AvailableCommand>>`.
    AcpAvailableCommand,
}

/// How to render a vendor-ext response in the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseRenderKind {
    /// Append a system-note trace entry with the raw params.
    SystemNote,
}
```

- [ ] **Step 5: Create the entries sub-structs.**

Create `crates/spur-acp/src/config/entries.rs`:

```rust
//! Sub-table shapes for `[[agents.entries]]` blocks added in Spec 1.
//!
//!   [agents.entries.commands]       → CommandsConfig
//!   [agents.entries.display]        → DisplayConfig
//!   [agents.entries.permissions]    → PermissionsConfig
//!
//! All are optional via `#[serde(default)]` on AgentConfig; omitting them
//! preserves today's hardcoded behavior (prompt_text dispatch, no vendor ext,
//! no bypass).

use serde::{Deserialize, Serialize};

use super::hooks::{
    ArgsTemplateKind, DispatchKind, IngestParserKind, ItemSchemaKind, ResponseRenderKind,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Short alias used as `/handle:cmd` on collision. Defaults to lowercase
    /// of `AgentConfig::name` when absent (resolved by
    /// `AgentConfig::effective_handle`, not by deserialize).
    #[serde(default)]
    pub handle: Option<String>,
    /// Reserved for future UX. Unused in Spec 1.
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandsConfig {
    #[serde(default)]
    pub dispatch: DispatchKind,

    /// Required when `dispatch = "vendor_exec"`. Full wire method, e.g.
    /// `"_kiro.dev/commands/execute"`. Validator rejects absent.
    #[serde(default)]
    pub exec_method: Option<String>,

    /// Required when `dispatch = "vendor_exec"`. How to shape args.
    #[serde(default)]
    pub args_template: ArgsTemplateKind,

    /// One entry per vendor-ext notification that advertises commands.
    #[serde(default)]
    pub ingest: Vec<IngestBinding>,

    /// One entry per vendor-ext method whose response is rendered in the trace.
    #[serde(default)]
    pub response: Vec<ResponseBinding>,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            dispatch: DispatchKind::default(),
            exec_method: None,
            args_template: ArgsTemplateKind::default(),
            ingest: Vec::new(),
            response: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestBinding {
    /// Wire method, e.g. `"_kiro.dev/commands/available"`.
    pub method: String,
    pub parser: IngestParserKind,
    /// Dotted JSON path (no array indexing) to the list inside `params`.
    pub path: String,
    pub item_schema: ItemSchemaKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseBinding {
    pub method: String,
    pub render: ResponseRenderKind,
}

/// Permission-bypass levers. Replaces the three flat `skip_permissions*`
/// fields on AgentConfig. Old configs keep working via `AgentConfig::
/// effective_permissions`, which falls back to flat fields when this
/// nested block is absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub skip: bool,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub session_mode: Option<String>,
}
```

- [ ] **Step 6: Wire the new modules into `config/mod.rs`.**

At the top of `crates/spur-acp/src/config/mod.rs` (previously `config.rs`), add:

```rust
pub mod entries;
pub mod hooks;

pub use entries::{
    CommandsConfig, DisplayConfig, IngestBinding, PermissionsConfig, ResponseBinding,
};
pub use hooks::{
    ArgsTemplateKind, DispatchKind, IngestParserKind, ItemSchemaKind, ResponseRenderKind,
};
```

- [ ] **Step 7: Add the three new fields to `AgentConfig`.**

In `crates/spur-acp/src/config/mod.rs`, add three fields to `AgentConfig` (positioned after `review`, before `skip_permissions`). Keep all existing flat fields unchanged:

```rust
    /// Per-agent display metadata (short handle, display name). Optional;
    /// defaults applied by `effective_handle`.
    #[serde(default)]
    pub display: DisplayConfig,

    /// Command dispatch / vendor-ext wiring. Optional; defaults to
    /// `prompt_text` dispatch with no vendor-ext ingest or response.
    #[serde(default)]
    pub commands: CommandsConfig,

    /// Permission-bypass levers. Replaces the three flat `skip_permissions*`
    /// fields; those remain for backward compatibility and are consulted by
    /// `effective_permissions` when this block is left at default.
    #[serde(default)]
    pub permissions: PermissionsConfig,
```

Import at the top of `config/mod.rs`:

```rust
use self::entries::{CommandsConfig, DisplayConfig, PermissionsConfig};
```

- [ ] **Step 8: Re-export from `lib.rs`.**

In `crates/spur-acp/src/lib.rs`, extend the existing `pub use config::` block (or add a new one):

```rust
pub use config::{
    AgentConfig, AgentsConfig, ArgsTemplateKind, CommandsConfig, DisplayConfig, DispatchKind,
    IngestBinding, IngestParserKind, ItemSchemaKind, PermissionsConfig, ResponseBinding,
    ResponseRenderKind, SpurConfig,
};
```

(Check existing `pub use` — merge rather than duplicate.)

- [ ] **Step 9: Run the failing test — it should now pass.**

Run: `cargo test -p spur-acp --test nested_config_shape`
Expected: PASS (all 4 tests).

Also run: `cargo build -p spur-acp`
Expected: PASS.

- [ ] **Step 10: Run the pre-existing skip_permissions round-trip test to confirm no regression.**

Run: `cargo test -p spur-acp --test skip_permissions_config`
Expected: PASS (the three flat fields still exist and still round-trip).

- [ ] **Step 11: Commit.**

```bash
git add crates/spur-acp/src/config/ crates/spur-acp/src/lib.rs crates/spur-acp/tests/nested_config_shape.rs
git commit -m "feat(spur-acp): add nested [commands] [display] [permissions] config blocks

Introduce DispatchKind, ArgsTemplateKind, IngestParserKind, ItemSchemaKind,
ResponseRenderKind enums (snake_case, serde rejects unknowns). Add
CommandsConfig, DisplayConfig, PermissionsConfig sub-structs on AgentConfig.
All three nested blocks default to empty; old configs with flat
skip_permissions_* fields still parse unchanged.

Dead-code at this point — no call sites read the new fields yet. Spec 1
task 1; follow-up tasks migrate the call sites and delete the hardcoded
kiro branches.

Spec: docs/superpowers/specs/2026-04-14-agent-config-foundation-design.md"
```

---

## Task 2: Migrate permission call sites to `effective_permissions()`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` — add `effective_permissions()` and rewire `effective_args()`
- Modify: `crates/spur-core/src/skip_perm.rs` — read nested form
- Modify: `crates/spur-core/src/orchestrator.rs` — no-op check (it reads `effective_args`)
- Test: `crates/spur-acp/tests/skip_permissions_config.rs` — add migration-equivalence test

- [ ] **Step 1: Write the failing migration-equivalence test.**

Append to `crates/spur-acp/tests/skip_permissions_config.rs`:

```rust
#[test]
fn flat_and_nested_permissions_yield_equivalent_effective() {
    let flat_toml = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
skip_permissions = true
skip_permissions_args = ["--trust-all-tools"]
skip_permissions_session_mode = "bypassPermissions"
"#;
    let nested_toml = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[permissions]
skip = true
args = ["--trust-all-tools"]
session_mode = "bypassPermissions"
"#;
    let flat: AgentConfig = toml::from_str(flat_toml).expect("flat");
    let nested: AgentConfig = toml::from_str(nested_toml).expect("nested");

    let flat_eff = flat.effective_permissions();
    let nested_eff = nested.effective_permissions();

    assert_eq!(flat_eff.skip, nested_eff.skip);
    assert_eq!(flat_eff.args, nested_eff.args);
    assert_eq!(flat_eff.session_mode, nested_eff.session_mode);

    // Also: effective_args must behave the same.
    assert_eq!(flat.effective_args(), nested.effective_args());
}

#[test]
fn nested_permissions_wins_when_both_present() {
    // Top-level flat fields are legacy; if a user has written a nested
    // [permissions] block, that is the source of truth.
    let toml_src = r#"
name = "mixed"
command = "x"
transport = "acp"
skip_permissions = false
skip_permissions_args = ["--ignored-flat"]

[permissions]
skip = true
args = ["--wins"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    let eff = cfg.effective_permissions();
    assert!(eff.skip);
    assert_eq!(eff.args, vec!["--wins".to_string()]);
}
```

- [ ] **Step 2: Run the test — should fail (method doesn't exist).**

Run: `cargo test -p spur-acp --test skip_permissions_config flat_and_nested_permissions_yield_equivalent_effective`
Expected: FAIL — `no method named effective_permissions`.

- [ ] **Step 3: Implement `effective_permissions` on AgentConfig.**

In `crates/spur-acp/src/config/mod.rs`, replace the existing `impl AgentConfig` block with:

```rust
impl AgentConfig {
    /// The effective permissions for this agent, merging the legacy flat
    /// `skip_permissions*` fields with the newer `[permissions]` nested
    /// block. Precedence: if the nested block has ANY non-default value
    /// (`skip`, `args`, or `session_mode`), it wins entirely. Otherwise the
    /// flat fields are promoted into a `PermissionsConfig`.
    ///
    /// The flat fields are retained for one release cycle for back-compat.
    /// New configs should write the nested form.
    pub fn effective_permissions(&self) -> PermissionsConfig {
        let nested_is_default = !self.permissions.skip
            && self.permissions.args.is_empty()
            && self.permissions.session_mode.is_none();
        if nested_is_default {
            PermissionsConfig {
                skip: self.skip_permissions,
                args: self.skip_permissions_args.clone(),
                session_mode: self.skip_permissions_session_mode.clone(),
            }
        } else {
            self.permissions.clone()
        }
    }

    /// Args to pass when spawning this agent. Concatenates `args` with the
    /// effective `permissions.args` iff `permissions.skip` is true. Single
    /// source of truth for `spur-core`'s spawn paths — do not read
    /// `self.args` directly when spawning.
    pub fn effective_args(&self) -> Vec<String> {
        let mut out = self.args.clone();
        let perms = self.effective_permissions();
        if perms.skip {
            out.extend(perms.args.iter().cloned());
        }
        out
    }
}
```

- [ ] **Step 4: Run the new tests — should now pass.**

Run: `cargo test -p spur-acp --test skip_permissions_config`
Expected: PASS (all old + 2 new tests).

- [ ] **Step 5: Update `skip_perm.rs` to read from `effective_permissions()`.**

In `crates/spur-core/src/skip_perm.rs::apply_bypass_session_mode`, replace the `if !cfg.skip_permissions { return; }` + `cfg.skip_permissions_session_mode.as_deref()` pattern with:

```rust
async fn apply_bypass_session_mode(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    session_id: SessionId,
    phase: &'static str,
) {
    let perms = cfg.effective_permissions();
    if !perms.skip {
        return;
    }
    let Some(mode) = perms.session_mode.as_deref() else {
        return;
    };

    let sid_for_log = session_id.0.to_string();
    let req = SetSessionModeRequest::new(session_id, mode.to_string());

    if let Err(e) = conn.set_session_mode(req).await {
        tracing::warn!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            phase,
            error = %e,
            "skip_permissions: set_session_mode failed; relying on L2 auto-approve"
        );
    } else {
        tracing::debug!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            phase,
            "skip_permissions: set_session_mode applied"
        );
    }
}
```

- [ ] **Step 6: Check orchestrator for any other flat-field reads.**

Run: `grep -rn "skip_permissions" crates/spur-core/src/ crates/spur-acp/src/`

Any hit outside `config/mod.rs` (declarations), `skip_perm.rs` (already updated), and `effective_args` (reads flat → already wrapped in `effective_permissions`) must be migrated the same way. Expected: no orphan hits; `effective_args()` is the only other reader.

- [ ] **Step 7: Run the full workspace test suite.**

Run: `cargo test --workspace`
Expected: PASS. Previously-existing tests continue to work because they construct `AgentConfig` with flat fields, and `effective_permissions()` promotes flat-to-nested transparently.

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-acp/src/config/mod.rs \
        crates/spur-acp/tests/skip_permissions_config.rs \
        crates/spur-core/src/skip_perm.rs
git commit -m "feat(spur-acp): AgentConfig::effective_permissions merges flat+nested

Callers now read cfg.effective_permissions() (returns a PermissionsConfig)
instead of cfg.skip_permissions / cfg.skip_permissions_args /
cfg.skip_permissions_session_mode. Nested [permissions] wins when present;
otherwise flat fields are promoted. effective_args() routes through the
same accessor.

Migrated: spur-core's skip_perm.rs. All old tests pass; adds two migration
tests in skip_permissions_config.rs proving flat and nested configs yield
identical effective behavior.

Spec 1 task 2."
```

---

## Task 3: `agents/entry_builder.rs` (pure, not yet used)

**Files:**
- Create: `crates/spur-tui/src/agents/mod.rs`
- Create: `crates/spur-tui/src/agents/entry_builder.rs`
- Create: `crates/spur-tui/src/agents/ingest.rs`
- Modify: `crates/spur-tui/src/lib.rs` (add `pub mod agents;`)
- Test: inline `#[cfg(test)] mod tests` in each new file.

- [ ] **Step 1: Write the failing test for `build_entry`.**

Create `crates/spur-tui/src/agents/entry_builder.rs`:

```rust
//! Pure function that turns an agent-advertised `AvailableCommand` plus
//! its owning agent's `CommandsConfig` into a `CommandEntry`. Replaces
//! the old `registry::agent_entry()` which hardcoded `if handle == "kiro"`.
//!
//! Pure (no I/O, no state) so it can be unit-tested in isolation.

use spur_acp::{AvailableCommand, AvailableCommandInput, CommandsConfig, DispatchKind};

use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

pub fn build_entry(
    handle: &str,
    cfg: &CommandsConfig,
    cmd: &AvailableCommand,
) -> CommandEntry {
    let hint = match &cmd.input {
        Some(AvailableCommandInput::Unstructured(u)) => Some(u.hint.clone()),
        _ => None,
    };

    let dispatch = match cfg.dispatch {
        DispatchKind::PromptText => Dispatch::PromptText {
            normalized: format!("/{}", cmd.name),
        },
        DispatchKind::VendorExec => {
            // Validator guarantees exec_method is present for vendor_exec
            // before we ever reach this path. Panic is correct here — it
            // indicates a missed validation bug, not a user error.
            let method = cfg
                .exec_method
                .clone()
                .expect("validator guarantees exec_method for vendor_exec");
            Dispatch::VendorExec {
                method,
                command: cmd.name.clone(),
                args_template: cfg.args_template,
            }
        }
    };

    CommandEntry {
        name: cmd.name.clone(),
        description: cmd.description.clone(),
        hint,
        source: CommandSource::Agent {
            handle: handle.to_string(),
        },
        dispatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{ArgsTemplateKind, CommandsConfig, DispatchKind};

    fn cmd(name: &str) -> AvailableCommand {
        AvailableCommand {
            name: name.into(),
            description: "desc".into(),
            input: None,
            meta: None,
        }
    }

    #[test]
    fn prompt_text_config_builds_prompt_text_dispatch() {
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let entry = build_entry("claude", &cfg, &cmd("compact"));
        match entry.dispatch {
            Dispatch::PromptText { normalized } => assert_eq!(normalized, "/compact"),
            other => panic!("expected PromptText, got {other:?}"),
        }
    }

    #[test]
    fn vendor_exec_config_builds_vendor_exec_dispatch() {
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = build_entry("kiro", &cfg, &cmd("context"));
        match entry.dispatch {
            Dispatch::VendorExec { method, command, args_template } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(command, "context");
                assert_eq!(args_template, ArgsTemplateKind::RawRest);
            }
            other => panic!("expected VendorExec, got {other:?}"),
        }
    }
}
```

Note: this file references `Dispatch::VendorExec` which doesn't exist yet — it's added atomically in Task 4. Until Task 4 lands, this file won't compile. That's the reason we hold off running the tests until Task 4.

- [ ] **Step 2: Create the agents module facade.**

Create `crates/spur-tui/src/agents/mod.rs`:

```rust
//! Config-driven dispatch hooks. Types declared in `spur-acp::config`
//! (strongly-typed deserialize); behavior implemented here where it has
//! access to `AgentConnection`, `AvailableCommand`, and `SessionDetailView`.
//!
//! Spec 1 ships seven hooks:
//!
//! | Hook ID                 | Kind                     | Where implemented    |
//! |-------------------------|--------------------------|----------------------|
//! | prompt_text             | DispatchKind             | entry_builder + submit_router |
//! | vendor_exec             | DispatchKind             | entry_builder + submit_router + orchestrator |
//! | raw_rest                | ArgsTemplateKind         | submit_router |
//! | json_path_list          | IngestParserKind         | ingest::run_ingest_hook |
//! | acp_available_command   | ItemSchemaKind           | ingest::run_ingest_hook |
//! | system_note             | ResponseRenderKind       | session_detail::render_response |
//! | files                   | MentionSourceKind        | (Spec 2; no behavior today) |

pub mod entry_builder;
pub mod ingest;

pub use entry_builder::build_entry;
pub use ingest::run_ingest_hook;
```

- [ ] **Step 3: Create the ingest module.**

Create `crates/spur-tui/src/agents/ingest.rs`:

```rust
//! Runtime impl of ingest hooks. `run_ingest_hook(binding, params)`
//! dispatches on the binding's parser + item_schema enums and returns the
//! decoded items (or None on parse failure).

use serde_json::Value;
use spur_acp::{AvailableCommand, IngestBinding, IngestParserKind, ItemSchemaKind};

/// Decode a vendor-ext notification's params payload into AvailableCommands
/// according to `binding`. Returns `None` if the expected path is missing or
/// the payload doesn't match the item schema — the caller should log and
/// move on rather than treat this as fatal.
pub fn run_ingest_hook(binding: &IngestBinding, params: &Value) -> Option<Vec<AvailableCommand>> {
    match binding.parser {
        IngestParserKind::JsonPathList => {
            let list = lookup_dotted_path(params, &binding.path)?;
            match binding.item_schema {
                ItemSchemaKind::AcpAvailableCommand => {
                    serde_json::from_value::<Vec<AvailableCommand>>(list.clone()).ok()
                }
            }
        }
    }
}

/// Walk a dotted path through a JSON value. Does not support array indexing
/// — `binding.path` is expected to be a field-name path like
/// `"availableCommands"` or `"result.items"`.
fn lookup_dotted_path(root: &Value, path: &str) -> Option<Value> {
    let mut current = root;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_path_list_decodes_available_commands() {
        let binding = IngestBinding {
            method: "_kiro.dev/commands/available".into(),
            parser: IngestParserKind::JsonPathList,
            path: "availableCommands".into(),
            item_schema: ItemSchemaKind::AcpAvailableCommand,
        };
        let params = serde_json::json!({
            "availableCommands": [
                { "name": "context", "description": "manage context" }
            ]
        });
        let out = run_ingest_hook(&binding, &params).expect("decoded");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "context");
    }

    #[test]
    fn missing_path_returns_none() {
        let binding = IngestBinding {
            method: "x".into(),
            parser: IngestParserKind::JsonPathList,
            path: "nope".into(),
            item_schema: ItemSchemaKind::AcpAvailableCommand,
        };
        let params = serde_json::json!({ "something_else": [] });
        assert!(run_ingest_hook(&binding, &params).is_none());
    }

    #[test]
    fn dotted_path_traverses_nested() {
        let binding = IngestBinding {
            method: "x".into(),
            parser: IngestParserKind::JsonPathList,
            path: "result.items".into(),
            item_schema: ItemSchemaKind::AcpAvailableCommand,
        };
        let params = serde_json::json!({
            "result": { "items": [{ "name": "a", "description": "b" }] }
        });
        let out = run_ingest_hook(&binding, &params).expect("decoded");
        assert_eq!(out.len(), 1);
    }
}
```

- [ ] **Step 4: Register the new module in `lib.rs`.**

In `crates/spur-tui/src/lib.rs`, add after the existing `pub mod` declarations:

```rust
pub mod agents;
```

- [ ] **Step 5: Confirm compile failure is localized (expected until Task 4).**

Run: `cargo build -p spur-tui`
Expected: FAIL — `Dispatch::VendorExec` not found (refs from `entry_builder.rs`). This is the expected state; Task 4 introduces the variant.

Don't commit yet. This file is included in Task 4's atomic commit, because its compile-pass depends on Task 4's enum rename. **Proceed directly to Task 4 without committing Task 3.**

---

## Task 4: Rename Dispatch::KiroExecute → VendorExec atomically across the vertical slice

This is the biggest task in the plan. The variant's shape changes (adds `method: String`), so a partial rename cannot compile. **All six enum sites change in a single commit, along with Task 3's new files.**

**Files:**
- Modify: `crates/spur-tui/src/commands/entry.rs` (Dispatch enum)
- Modify: `crates/spur-tui/src/commands/submit_router.rs` (SubmitDecision enum + dispatch arms)
- Modify: `crates/spur-tui/src/commands/registry.rs` (delete `agent_entry`, change `set_agent_commands` signature)
- Modify: `crates/spur-tui/src/action.rs` (Action::KiroExecute → VendorExec)
- Modify: `crates/spur-tui/src/app.rs` (UserInput::KiroExecute → VendorExec; Action::VendorExec arm)
- Modify: `crates/spur-tui/src/views/session_detail.rs` (SubmitDecision arm at line 656)
- Modify: `crates/spur-cli/src/main.rs` (UserInput::VendorExec conversion arm)
- Modify: `crates/spur-core/src/orchestrator.rs` (InteractiveInput::KiroExecute → VendorExec; worker arm)

- [ ] **Step 1: Rename `Dispatch::KiroExecute` in `commands/entry.rs`.**

In `crates/spur-tui/src/commands/entry.rs`, replace the `Dispatch` enum:

```rust
use crate::action::Action;
use spur_acp::ArgsTemplateKind;

/// An entry displayed in the slash-command popup.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
    pub source: CommandSource,
    pub dispatch: Dispatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    Spur,
    Agent { handle: String },
}

/// How a selected `CommandEntry` should be executed.
#[derive(Debug, Clone)]
pub enum Dispatch {
    /// Fire an `Action` directly, close the popup, do not send a message.
    SpurLocal(Action),
    /// Send the normalized text as a `ContentBlock::Text` to the current agent.
    PromptText { normalized: String },
    /// Invoke an agent-specific vendor extension RPC. Generic replacement
    /// for the previous KiroExecute variant.
    VendorExec {
        /// Full wire method (e.g. `"_kiro.dev/commands/execute"`).
        method: String,
        /// The command name (no leading slash).
        command: String,
        /// How to shape rest-of-line text into the RPC args payload.
        args_template: ArgsTemplateKind,
    },
}
```

Note the new `use spur_acp::ArgsTemplateKind;` import at the top. Remove the old `use serde_json::Value;` import (no longer needed here).

- [ ] **Step 2: Update `SubmitDecision` and `route()` in `submit_router.rs`.**

In `crates/spur-tui/src/commands/submit_router.rs`:

Replace the module doc comment's `KiroExecute` line with `VendorExec`.

Replace the `SubmitDecision` enum:

```rust
/// What the controller should do with an Enter-submitted InputBar.
#[derive(Debug)]
pub enum SubmitDecision {
    Send {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    Local {
        action: Action,
    },
    /// Generic vendor-extension dispatch. Carries the full wire method and
    /// the rendered args payload — the consumer (app.rs → orchestrator)
    /// calls `connection.call_ext(method, args)`.
    VendorExec {
        method: String,
        command: String,
        args: Value,
    },
    Empty,
}
```

Replace the `Dispatch::KiroExecute { command, args: _seed } => { ... }` arm with:

```rust
                Dispatch::VendorExec { method, command, args_template } => {
                    let rest = rest_after_first_token(text);
                    let args = match args_template {
                        spur_acp::ArgsTemplateKind::RawRest => {
                            if rest.is_empty() {
                                serde_json::json!({})
                            } else {
                                serde_json::json!({ "args": { "raw": rest } })
                            }
                        }
                    };
                    SubmitDecision::VendorExec { method, command, args }
                }
```

- [ ] **Step 3: Delete `agent_entry` + hardcoded kiro branch in `registry.rs`.**

In `crates/spur-tui/src/commands/registry.rs`:

Change `set_agent_commands` to accept pre-built entries (the caller — session_detail — will call `build_entry` itself now that it has access to `CommandsConfig`):

```rust
use std::cell::RefCell;
use std::collections::HashSet;

use super::entry::{CommandEntry, CommandSource};
use super::spur_local::SpurLocalSource;

pub struct CommandRegistry {
    agent_commands: Vec<(String, Vec<CommandEntry>)>,
    cache: RefCell<Option<CacheSnapshot>>,
}

struct CacheSnapshot {
    entries: Vec<CommandEntry>,
    colliding: HashSet<String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            agent_commands: Vec::new(),
            cache: RefCell::new(None),
        }
    }

    /// Replace the full command set for an agent handle. Entries are
    /// pre-built by the caller via `agents::build_entry`.
    pub fn set_agent_commands(&mut self, handle: &str, entries: Vec<CommandEntry>) {
        if let Some(slot) = self.agent_commands.iter_mut().find(|(h, _)| h == handle) {
            slot.1 = entries;
        } else {
            self.agent_commands.push((handle.to_string(), entries));
        }
        *self.cache.borrow_mut() = None;
    }

    fn ensure_cache(&self) {
        let mut slot = self.cache.borrow_mut();
        if slot.is_some() {
            return;
        }
        let mut entries = SpurLocalSource::entries();
        for (_handle, agent_entries) in &self.agent_commands {
            entries.extend(agent_entries.iter().cloned());
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

The old `fn agent_entry(...)` at the bottom of the file is **deleted entirely**. Also remove the now-unused import `use spur_acp::{AvailableCommand, AvailableCommandInput};`.

- [ ] **Step 4: Rename `Action::KiroExecute` → `Action::VendorExec` in `action.rs`.**

In `crates/spur-tui/src/action.rs`, replace the `KiroExecute` variant with:

```rust
    /// Invoke an agent vendor-extension RPC.
    VendorExec {
        session: SessionId,
        /// Full wire method (e.g. `"_kiro.dev/commands/execute"`).
        method: String,
        command: String,
        args: serde_json::Value,
    },
```

Delete the old `KiroExecute { session, command, args }` variant.

- [ ] **Step 5: Rename `UserInput::KiroExecute` → `UserInput::VendorExec` in `app.rs`.**

In `crates/spur-tui/src/app.rs`, replace the `UserInput::KiroExecute` variant:

```rust
    VendorExec {
        session: SessionId,
        method: String,
        command: String,
        args: serde_json::Value,
    },
```

And replace the `Action::KiroExecute` handler arm (around line 629):

```rust
            Action::VendorExec { session, method, command, args } => {
                if let Some(ref mut detail) = self.session_detail {
                    let handle = detail.agent_handle_for_commands();
                    detail.push_system_note(format!(
                        "\u{27e8}{handle}\u{27e9} /{} queued",
                        command
                    ));
                }
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::VendorExec {
                        session,
                        method,
                        command,
                        args,
                    });
                }
            }
```

Note the `handle` now comes from the session's own `agent_handle_for_commands()` instead of the hardcoded `\u{27e8}kiro\u{27e9}` string — this is part of the generic-ization.

- [ ] **Step 6: Wire the SubmitDecision handler in `session_detail.rs:656`.**

In `crates/spur-tui/src/views/session_detail.rs`, replace the arm at ~line 656:

```rust
                        SubmitDecision::VendorExec { method, command, args } => {
                            Some(Action::VendorExec {
                                session: self.session_id.clone(),
                                method,
                                command,
                                args,
                            })
                        }
```

(The previous `SubmitDecision::KiroExecute { command, args } => { Some(Action::KiroExecute { ... }) }` arm is gone.)

- [ ] **Step 7: Update the CLI conversion in `spur-cli/src/main.rs`.**

In `crates/spur-cli/src/main.rs` around line 417, replace the `UserInput::KiroExecute` arm with:

```rust
                        spur_tui::UserInput::VendorExec { session, method, command, args } => {
                            spur_core::InteractiveInput::VendorExec {
                                session,
                                method,
                                command,
                                args,
                            }
                        }
```

- [ ] **Step 8: Rename `InteractiveInput::KiroExecute` → `VendorExec` in `orchestrator.rs`.**

In `crates/spur-core/src/orchestrator.rs`, replace the `KiroExecute` variant on `InteractiveInput`:

```rust
    /// Invoke an agent vendor-extension RPC on the active brain session.
    /// No-op if there is no active brain session. The method name and args
    /// are chosen by the TUI's config-driven dispatch path — the
    /// orchestrator is agnostic to specific extensions.
    VendorExec {
        session: SessionId,
        method: String,
        command: String,
        args: serde_json::Value,
    },
```

And replace the worker arm (around line 522) that handles the call:

```rust
                // ── VendorExec ───────────────────────────────────────────
                InteractiveInput::VendorExec { session, method, command, args } => {
                    if let Some(b) = brain.as_mut() {
                        let params = serde_json::json!({
                            "sessionId": b.acp_session_id,
                            "command": command,
                            "args": args,
                        });
                        match b.connection.call_ext(&method, params).await {
                            Ok(resp) => {
                                self.emit(SpurEvent::now(
                                    SpurEventBody::AgentExtNotification {
                                        session: session.clone(),
                                        method: spur_acp::ext::SPUR_KIRO_EXECUTE_RESPONSE.into(),
                                        params: resp,
                                    },
                                ));
                            }
                            Err(e) => {
                                warn!(
                                    brain = %b.brain_name,
                                    method = %method,
                                    command = %command,
                                    error = %e,
                                    "vendor exec call failed"
                                );
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session,
                                    message: format!("vendor exec failed: {}", e),
                                }));
                            }
                        }
                    } else {
                        warn!(method = %method, command = %command,
                            "VendorExec received but no active brain session");
                    }
                }
```

**Note:** the response method name on the SpurEvent is still `SPUR_KIRO_EXECUTE_RESPONSE`. That's a wire-format constant carried over from kiro; generalizing it is Spec 3+ work. For Spec 1 we keep the existing naming — the response binding loop in session_detail (Task 5) will match on that constant via config rather than hardcoded, but the constant itself stays named.

- [ ] **Step 9: Build the workspace to confirm the rename is complete.**

Run: `cargo build --workspace`
Expected: PASS. Any remaining `KiroExecute` reference will fail compilation — fix it in place before proceeding.

Verify zero remaining references:

Run: `grep -rn "KiroExecute" crates/`
Expected: no matches (comments, tests, and docs all updated).

- [ ] **Step 10: Run the tests — the `entry_builder` and `ingest` unit tests now compile.**

Run: `cargo test -p spur-tui agents::`
Expected: PASS (5 tests: 2 in entry_builder, 3 in ingest).

- [ ] **Step 11: Run the full test suite.**

Run: `cargo test --workspace`

One test will fail: `crates/spur-tui/tests/session_update_handling.rs::kiro_available_notification_populates_registry`. It calls `view.handle_spur_event` which still runs the hardcoded kiro branch — that branch survives to Task 5. The test itself is updated in Task 5; for now, it compiles but the registry path is still the old one. Expected behavior: PASS (no test broke compile; the hardcoded branch in session_detail still works and populates the registry with the legacy path).

Actually, one subtle issue: `CommandRegistry::set_agent_commands` now takes `Vec<CommandEntry>` not `Vec<AvailableCommand>`. The old `session_detail.rs:984` still calls `set_agent_commands("kiro", parsed)` with a `Vec<AvailableCommand>` — this will NOT compile.

Fix that in Step 12 below (the bridge to Task 5).

- [ ] **Step 12: Bridge the hardcoded session_detail branch to the new signature.**

The hardcoded `if method == KIRO_COMMANDS_AVAILABLE` block in `session_detail.rs:979` currently does:

```rust
self.command_registry.set_agent_commands("kiro", parsed);
```

where `parsed: Vec<AvailableCommand>`. Temporarily build a minimal kiro `CommandsConfig` on the fly and call `build_entry`:

```rust
                if method == spur_acp::ext::KIRO_COMMANDS_AVAILABLE {
                    if let Some(arr) = params.get("availableCommands").cloned() {
                        if let Ok(parsed) =
                            serde_json::from_value::<Vec<spur_acp::AvailableCommand>>(arr)
                        {
                            // Transitional: build entries through the new pure
                            // helper using a synthetic kiro CommandsConfig. Task 5
                            // replaces this block entirely with a config-driven loop.
                            let kiro_cfg = spur_acp::CommandsConfig {
                                dispatch: spur_acp::DispatchKind::VendorExec,
                                exec_method: Some(
                                    spur_acp::ext::KIRO_COMMANDS_EXECUTE.to_string(),
                                ),
                                args_template: spur_acp::ArgsTemplateKind::RawRest,
                                ingest: vec![],
                                response: vec![],
                            };
                            let entries: Vec<_> = parsed
                                .iter()
                                .map(|c| crate::agents::build_entry("kiro", &kiro_cfg, c))
                                .collect();
                            self.command_registry.set_agent_commands("kiro", entries);
                        }
                    }
                } else if method == spur_acp::ext::SPUR_KIRO_EXECUTE_RESPONSE {
                    self.push_system_note(format!(
                        "\u{27e8}kiro\u{27e9} response: {}",
                        params
                    ));
                }
```

- [ ] **Step 13: Re-run the workspace tests.**

Run: `cargo test --workspace`
Expected: PASS — including `kiro_available_notification_populates_registry` which now exercises the generic path even via the temporary bridge.

- [ ] **Step 14: Commit.**

```bash
git add crates/spur-tui/src/agents/ \
        crates/spur-tui/src/lib.rs \
        crates/spur-tui/src/commands/entry.rs \
        crates/spur-tui/src/commands/submit_router.rs \
        crates/spur-tui/src/commands/registry.rs \
        crates/spur-tui/src/action.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-cli/src/main.rs \
        crates/spur-core/src/orchestrator.rs
git commit -m "refactor: rename KiroExecute → VendorExec across the vertical slice

Six enums across three crates flip in one commit because the variant's
shape changes (adds method: String) and a partial rename doesn't compile.
Replaces the hardcoded 'if handle == \"kiro\"' branch in registry.rs with
agents::build_entry, a pure function driven by CommandsConfig.

CommandRegistry::set_agent_commands now takes Vec<CommandEntry> (pre-built
by the caller) instead of Vec<AvailableCommand> — the caller is in a better
position to supply the agent's CommandsConfig.

Transitional bridge in session_detail.rs synthesizes a minimal kiro config
on the fly so the KIRO_COMMANDS_AVAILABLE arm keeps working; Task 5
replaces that arm with a config-driven ingest loop.

Spec 1 task 3 + 4."
```

---

## Task 5: Config-driven ingest/response in session_detail.rs

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (add `agent_cfg`, replace hardcoded arms)
- Modify: `crates/spur-tui/src/app.rs` (pass `agent_cfg` into SessionDetailView constructor)
- Modify: `crates/spur-tui/tests/session_update_handling.rs` (construct config with ingest binding)

- [ ] **Step 1: Add `agent_cfg` field + constructor parameter.**

In `crates/spur-tui/src/views/session_detail.rs`, add to the struct (near `role`):

```rust
    /// The AgentConfig backing this session. Owns the CommandsConfig used by
    /// the ingest/response loops below, and the effective permissions.
    agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
```

Update `new()` signature and body:

```rust
    pub fn new(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
        agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
    ) -> Self {
        Self {
            session_id,
            agent_name,
            role,
            agent_cfg,
            react_trace: ReactTrace::new(),
            // ... rest unchanged
```

- [ ] **Step 2: Replace the hardcoded `AgentExtNotification` handler.**

In `crates/spur-tui/src/views/session_detail.rs`, replace the entire `SpurEventBody::AgentExtNotification { session, method, params } => { ... }` arm (lines ~975-993) with:

```rust
            SpurEventBody::AgentExtNotification { session, method, params } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                let handle = self.agent_handle_for_commands();
                let cfg = self.agent_cfg.clone();

                // Ingest bindings: decode params → CommandEntry list → registry.
                for binding in &cfg.commands.ingest {
                    if &binding.method != method {
                        continue;
                    }
                    if let Some(parsed) = crate::agents::run_ingest_hook(binding, params) {
                        let entries: Vec<_> = parsed
                            .iter()
                            .map(|c| crate::agents::build_entry(&handle, &cfg.commands, c))
                            .collect();
                        self.command_registry.set_agent_commands(&handle, entries);
                    }
                }

                // Response bindings: render the payload according to `render` kind.
                for binding in &cfg.commands.response {
                    if &binding.method != method {
                        continue;
                    }
                    match binding.render {
                        spur_acp::ResponseRenderKind::SystemNote => {
                            self.push_system_note(format!(
                                "\u{27e8}{handle}\u{27e9} response: {}",
                                params
                            ));
                        }
                    }
                }
            }
```

Note the temporary bridge added in Task 4 step 12 is deleted here — the arm above is the final form.

- [ ] **Step 3: Update the App to thread `agent_cfg` into SessionDetailView::new.**

In `crates/spur-tui/src/app.rs`, find each `SessionDetailView::new(...)` call. Pass an `Arc<AgentConfig>` derived from the App's loaded `SpurConfig`.

The App already knows which agent this session uses (via `agent_name`). Resolve the AgentConfig:

```rust
let agent_cfg = self
    .config
    .agents
    .entries
    .iter()
    .find(|e| e.name == agent_name)
    .cloned()
    .map(std::sync::Arc::new)
    .unwrap_or_else(|| {
        // Fallback: synthetic default config for agents not in the .toml.
        // This should not happen in normal flows; emit a warning trace.
        std::sync::Arc::new(spur_acp::AgentConfig {
            name: agent_name.clone(),
            command: String::new(),
            args: vec![],
            transport: spur_acp::types::TransportKind::Acp,
            role: spur_acp::types::AgentRole::Both,
            capabilities: vec![],
            cost_tier: spur_acp::types::CostTier::Medium,
            rate_limit_window: None,
            review: Default::default(),
            display: Default::default(),
            commands: Default::default(),
            permissions: Default::default(),
            skip_permissions: false,
            skip_permissions_args: vec![],
            skip_permissions_session_mode: None,
        })
    });

let view = SessionDetailView::new(
    session_id,
    agent_name,
    role,
    cwd,
    agent_cfg,
);
```

Apply the same update to every `SessionDetailView::new` call in `app.rs` (typically two: one for fresh sessions, one for resume). If the App holds a pre-indexed `HashMap<String, Arc<AgentConfig>>`, use that instead. If it doesn't today, add a helper:

```rust
impl App {
    fn resolve_agent_config(&self, name: &str) -> std::sync::Arc<spur_acp::AgentConfig> {
        self.config
            .agents
            .entries
            .iter()
            .find(|e| e.name == name)
            .cloned()
            .map(std::sync::Arc::new)
            .unwrap_or_else(|| std::sync::Arc::new(Self::fallback_agent_config(name)))
    }

    fn fallback_agent_config(name: &str) -> spur_acp::AgentConfig {
        spur_acp::AgentConfig {
            name: name.to_string(),
            command: String::new(),
            args: vec![],
            transport: spur_acp::types::TransportKind::Acp,
            role: spur_acp::types::AgentRole::Both,
            capabilities: vec![],
            cost_tier: spur_acp::types::CostTier::Medium,
            rate_limit_window: None,
            review: Default::default(),
            display: Default::default(),
            commands: Default::default(),
            permissions: Default::default(),
            skip_permissions: false,
            skip_permissions_args: vec![],
            skip_permissions_session_mode: None,
        }
    }
}
```

- [ ] **Step 4: Update the session_update_handling test to pass an AgentConfig.**

In `crates/spur-tui/tests/session_update_handling.rs`, replace `kiro_available_notification_populates_registry`:

```rust
#[test]
fn kiro_available_notification_populates_registry() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_tui::views::View;

    let sid = SessionId("kiro-test-session".to_string());

    let kiro_cfg = std::sync::Arc::new(spur_acp::AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: spur_acp::CommandsConfig {
            dispatch: spur_acp::DispatchKind::VendorExec,
            exec_method: Some(spur_acp::ext::KIRO_COMMANDS_EXECUTE.to_string()),
            args_template: spur_acp::ArgsTemplateKind::RawRest,
            ingest: vec![spur_acp::IngestBinding {
                method: spur_acp::ext::KIRO_COMMANDS_AVAILABLE.to_string(),
                parser: spur_acp::IngestParserKind::JsonPathList,
                path: "availableCommands".to_string(),
                item_schema: spur_acp::ItemSchemaKind::AcpAvailableCommand,
            }],
            response: vec![],
        },
        permissions: Default::default(),
        skip_permissions: false,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: None,
    });

    let mut view = spur_tui::views::session_detail::SessionDetailView::new(
        sid.clone(),
        "kiro".to_string(),
        "brain".to_string(),
        std::path::PathBuf::from("."),
        kiro_cfg,
    );

    let params = serde_json::json!({
        "sessionId": sid.0,
        "availableCommands": [
            { "name": "context", "description": "manage context" }
        ]
    });
    let ev = SpurEvent::now(SpurEventBody::AgentExtNotification {
        session: sid,
        method: spur_acp::ext::KIRO_COMMANDS_AVAILABLE.to_string(),
        params,
    });
    view.handle_spur_event(&ev);

    let entries = view.command_registry().list();
    assert!(
        entries.iter().any(|e| e.name == "context"
            && matches!(
                &e.source,
                spur_tui::commands::CommandSource::Agent { handle } if handle == "kiro"
            )),
        "context not populated as kiro agent command: {:?}",
        entries
            .iter()
            .map(|e| (e.name.clone(), e.source.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn kiro_execute_response_renders_as_system_note() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_tui::views::View;

    let sid = SessionId("kiro-exec-session".to_string());

    let kiro_cfg = std::sync::Arc::new(spur_acp::AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: spur_acp::CommandsConfig {
            dispatch: spur_acp::DispatchKind::VendorExec,
            exec_method: Some(spur_acp::ext::KIRO_COMMANDS_EXECUTE.to_string()),
            args_template: spur_acp::ArgsTemplateKind::RawRest,
            ingest: vec![],
            response: vec![spur_acp::ResponseBinding {
                method: spur_acp::ext::SPUR_KIRO_EXECUTE_RESPONSE.to_string(),
                render: spur_acp::ResponseRenderKind::SystemNote,
            }],
        },
        permissions: Default::default(),
        skip_permissions: false,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: None,
    });

    let mut view = spur_tui::views::session_detail::SessionDetailView::new(
        sid.clone(),
        "kiro".to_string(),
        "brain".to_string(),
        std::path::PathBuf::from("."),
        kiro_cfg,
    );

    let ev = SpurEvent::now(SpurEventBody::AgentExtNotification {
        session: sid,
        method: spur_acp::ext::SPUR_KIRO_EXECUTE_RESPONSE.to_string(),
        params: serde_json::json!({"stdout": "ok"}),
    });
    view.handle_spur_event(&ev);

    // The system note should be present as a trace entry. The easiest
    // observation: the trace should have at least one entry whose text
    // contains the expected kiro-tagged response.
    // (SessionDetailView exposes react_trace via a test helper or
    // push_system_note inserts a known kind.)
    // Use the existing react_trace_entries() accessor if present; else
    // check via a more generic test seam. See Step 5 if needed.
    let last_trace = view.trace_snapshot_for_test();
    assert!(
        last_trace.iter().any(|t| t.contains("kiro") && t.contains("response")),
        "expected a kiro-tagged response system note; got {last_trace:?}"
    );
}
```

- [ ] **Step 5: If `trace_snapshot_for_test` doesn't exist, add a minimal test accessor.**

In `crates/spur-tui/src/views/session_detail.rs` (within `impl SessionDetailView`), add:

```rust
    /// Test-only accessor: flattened trace text for each entry, oldest→newest.
    /// Used by integration tests in `tests/session_update_handling.rs`.
    #[doc(hidden)]
    pub fn trace_snapshot_for_test(&self) -> Vec<String> {
        self.react_trace
            .entries_for_test()
            .iter()
            .map(|e| e.text.clone())
            .collect()
    }
```

Check whether `ReactTrace::entries_for_test` exists (it was added for the mermaid work). If not, add it in `crates/spur-tui/src/components/react_trace.rs`:

```rust
    #[doc(hidden)]
    pub fn entries_for_test(&self) -> &[crate::components::react_trace::TraceEntry] {
        &self.entries
    }
```

(`entries_mut_for_test` was added earlier in the mermaid work; this is the read-only sibling.)

- [ ] **Step 6: Build and run tests.**

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: PASS — including both `kiro_available_notification_populates_registry` and the new `kiro_execute_response_renders_as_system_note`.

- [ ] **Step 7: Verify the success-criteria greps.**

Run: `grep -rn "if handle == \"kiro\"" crates/`
Expected: no matches.

Run: `grep -rn "KIRO_COMMANDS_AVAILABLE" crates/spur-tui/`
Expected: only the `session_update_handling.rs` test file matches (building the config), not `session_detail.rs` or any runtime code.

Run: `grep -rn "KIRO_COMMANDS_AVAILABLE" crates/spur-tui/src/`
Expected: zero matches — the runtime no longer names the kiro method.

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/components/react_trace.rs \
        crates/spur-tui/tests/session_update_handling.rs
git commit -m "refactor(spur-tui): delete hardcoded kiro branches; drive ingest/response from AgentConfig

session_detail.rs no longer names KIRO_COMMANDS_AVAILABLE or
SPUR_KIRO_EXECUTE_RESPONSE; the AgentExtNotification arm is now a generic
loop over agent_cfg.commands.{ingest, response}. SessionDetailView gains
an Arc<AgentConfig> constructor arg; App resolves it from SpurConfig at
session creation time.

The kiro behavior is preserved by the '.spur/config.toml.example' kiro
block (added in Task 8). Adding a new agent with vendor-ext behavior is
now pure TOML + a new [agents.entries] block.

Tests: session_update_handling gains kiro_execute_response_renders_as_system_note;
existing kiro_available_notification_populates_registry updated to
construct an AgentConfig rather than relying on the hardcoded method name.

Spec 1 task 5. Closes the 'delete hardcoded kiro branches' item on the
roadmap success criteria."
```

---

## Task 6: Validator (two rules)

**Files:**
- Create: `crates/spur-acp/src/config/validator.rs`
- Modify: `crates/spur-acp/src/config/mod.rs` (export validator)
- Modify: `crates/spur-acp/src/lib.rs` (re-export)
- Modify: `crates/spur-tui/src/app.rs` (invoke at startup)
- Test: inline `#[cfg(test)] mod tests` in validator.rs

- [ ] **Step 1: Write failing tests for the validator.**

Create `crates/spur-acp/src/config/validator.rs`:

```rust
//! Startup validation for AgentConfig. Strongly-typed deserialize already
//! rejects unknown enum variants — this validator handles rules that
//! require cross-field knowledge.
//!
//! Rules (Spec 1):
//!   R1 (FATAL):  dispatch = "vendor_exec" requires exec_method to be set.
//!   R3 (WARN):   permissions.skip = true with no explicit mechanism
//!                (args empty AND session_mode absent) will rely solely
//!                on L2 auto-approve; flag so users notice.
//!
//! R2 (hook-ID registry lookup) is covered by serde enum parsing and is
//! intentionally out of scope for Spec 1 — see the roadmap for when/if
//! to add it.

use super::entries::CommandsConfig;
use super::hooks::DispatchKind;
use super::AgentConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    VendorExecMissingMethod { agent: String },
    SkipPermissionsNoExplicitMechanism { agent: String, note: String },
}

impl ConfigError {
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::VendorExecMissingMethod { .. })
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VendorExecMissingMethod { agent } => write!(
                f,
                "{agent}: dispatch = \"vendor_exec\" requires [agents.entries.commands] exec_method"
            ),
            Self::SkipPermissionsNoExplicitMechanism { agent, note } => {
                write!(f, "{agent}: permissions.skip = true with no explicit mechanism — {note}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Validate a single AgentConfig. Returns `Ok(())` on success, or a
/// Vec<ConfigError> containing all problems found (may mix fatal + warn).
/// Callers inspect `ConfigError::is_fatal` to decide whether to refuse
/// to start the agent.
pub fn validate_agent_config(cfg: &AgentConfig) -> Result<(), Vec<ConfigError>> {
    let mut errors = Vec::new();

    // R1: vendor_exec dispatch requires exec_method.
    if matches!(cfg.commands.dispatch, DispatchKind::VendorExec)
        && cfg.commands.exec_method.is_none()
    {
        errors.push(ConfigError::VendorExecMissingMethod {
            agent: cfg.name.clone(),
        });
    }

    // R3: skip_permissions with no mechanism → WARN.
    let perms = cfg.effective_permissions();
    if perms.skip && perms.args.is_empty() && perms.session_mode.is_none() {
        errors.push(ConfigError::SkipPermissionsNoExplicitMechanism {
            agent: cfg.name.clone(),
            note: "relying on L2 auto-approve only; consider setting permissions.args or permissions.session_mode".into(),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::entries::{CommandsConfig, PermissionsConfig};

    fn base_cfg(name: &str) -> AgentConfig {
        AgentConfig {
            name: name.into(),
            command: "x".into(),
            args: vec![],
            transport: crate::types::TransportKind::Acp,
            role: crate::types::AgentRole::Both,
            capabilities: vec![],
            cost_tier: crate::types::CostTier::Medium,
            rate_limit_window: None,
            review: Default::default(),
            display: Default::default(),
            commands: Default::default(),
            permissions: Default::default(),
            skip_permissions: false,
            skip_permissions_args: vec![],
            skip_permissions_session_mode: None,
        }
    }

    #[test]
    fn r1_vendor_exec_without_exec_method_is_fatal() {
        let mut cfg = base_cfg("kiro");
        cfg.commands.dispatch = DispatchKind::VendorExec;
        cfg.commands.exec_method = None;
        let err = validate_agent_config(&cfg).expect_err("should error");
        assert_eq!(err.len(), 1);
        assert!(err[0].is_fatal());
        assert!(matches!(err[0], ConfigError::VendorExecMissingMethod { .. }));
    }

    #[test]
    fn r1_vendor_exec_with_exec_method_passes() {
        let mut cfg = base_cfg("kiro");
        cfg.commands.dispatch = DispatchKind::VendorExec;
        cfg.commands.exec_method = Some("_kiro.dev/commands/execute".into());
        validate_agent_config(&cfg).expect("should pass");
    }

    #[test]
    fn r3_skip_without_mechanism_is_warning() {
        let mut cfg = base_cfg("bogus");
        cfg.permissions = PermissionsConfig {
            skip: true,
            args: vec![],
            session_mode: None,
        };
        let err = validate_agent_config(&cfg).expect_err("should warn");
        assert_eq!(err.len(), 1);
        assert!(!err[0].is_fatal(), "R3 must be warning, not fatal");
        assert!(matches!(err[0], ConfigError::SkipPermissionsNoExplicitMechanism { .. }));
    }

    #[test]
    fn r3_skip_with_session_mode_passes() {
        let mut cfg = base_cfg("claude");
        cfg.permissions = PermissionsConfig {
            skip: true,
            args: vec![],
            session_mode: Some("bypassPermissions".into()),
        };
        validate_agent_config(&cfg).expect("should pass");
    }

    #[test]
    fn r3_skip_via_legacy_flat_fields_also_counts_as_mechanism() {
        // effective_permissions merges flat into nested; if user has flat
        // skip_permissions_session_mode set, R3 should not warn.
        let mut cfg = base_cfg("claude-legacy");
        cfg.skip_permissions = true;
        cfg.skip_permissions_session_mode = Some("bypassPermissions".into());
        validate_agent_config(&cfg).expect("should pass via legacy flat");
    }
}
```

- [ ] **Step 2: Register validator in config module.**

In `crates/spur-acp/src/config/mod.rs`, add:

```rust
pub mod validator;
pub use validator::{validate_agent_config, ConfigError};
```

In `crates/spur-acp/src/lib.rs`:

```rust
pub use config::{validate_agent_config, ConfigError};
```

- [ ] **Step 3: Run the validator tests.**

Run: `cargo test -p spur-acp config::validator`
Expected: PASS (all 5 tests).

- [ ] **Step 4: Invoke the validator at App startup.**

In `crates/spur-tui/src/app.rs`, in the existing `App::new(...)` (or wherever the `SpurConfig` is first available), iterate entries and report:

```rust
// Validate every agent entry. Fatal errors abort the agent (but we don't
// crash the whole TUI — other agents may still work). Warnings are logged
// and we continue.
for entry in &self.config.agents.entries {
    match spur_acp::validate_agent_config(entry) {
        Ok(()) => {}
        Err(errors) => {
            for e in errors {
                if e.is_fatal() {
                    tracing::error!(agent = %entry.name, error = %e,
                        "agent config validation failed; this agent will not be usable");
                } else {
                    tracing::warn!(agent = %entry.name, warning = %e,
                        "agent config validation warning");
                }
            }
        }
    }
}
```

Place this after the config is loaded and before any agent connection is spawned. If `App::new` already has a config-load block, add this immediately after.

- [ ] **Step 5: Run the workspace tests.**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-acp/src/config/validator.rs \
        crates/spur-acp/src/config/mod.rs \
        crates/spur-acp/src/lib.rs \
        crates/spur-tui/src/app.rs
git commit -m "feat(spur-acp): validator for [commands] + [permissions] consistency

Two rules in Spec 1:

  R1 (FATAL): dispatch = \"vendor_exec\" requires exec_method
  R3 (WARN):  permissions.skip = true with no explicit mechanism —
             user is relying solely on L2 auto-approve

R2 (hook-ID registry lookup) intentionally omitted — strongly-typed serde
deserialize already rejects unknown hook IDs at parse time.

Invoked from App::new at startup; fatal errors log at error level but do
not crash the TUI (other agents may still be usable). Warnings log at
warn level and flow through.

This closes follow-up F2 (silent skip-permissions-without-mechanism) as
an instance of the general validator pattern.

Spec 1 task 6."
```

---

## Task 7: `spur config check` subcommand

**Files:**
- Create: `crates/spur-cli/src/commands/mod.rs`
- Create: `crates/spur-cli/src/commands/config_check.rs`
- Modify: `crates/spur-cli/src/main.rs` (add Commands::Config + dispatch)
- Test: `crates/spur-cli/tests/config_check.rs` (new)

- [ ] **Step 1: Write the failing CLI integration test.**

Create `crates/spur-cli/tests/config_check.rs`:

```rust
//! Integration test for `spur config check`. Builds the binary and invokes
//! it against a temporary config file — verifies exit codes and diagnostic
//! output.

use std::io::Write;
use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    // cargo sets CARGO_BIN_EXE_spur for integration tests.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

fn write_config(contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let spur_dir = dir.path().join(".spur");
    std::fs::create_dir_all(&spur_dir).expect("mkdir .spur");
    let mut f = std::fs::File::create(spur_dir.join("config.toml")).expect("create toml");
    f.write_all(contents.as_bytes()).expect("write");
    dir
}

#[test]
fn config_check_passes_on_valid_config() {
    let dir = write_config(r#"
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"

[agents.entries.commands]
dispatch = "prompt_text"
"#);
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["config", "check"])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "expected 0 exit; stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn config_check_fails_on_vendor_exec_without_method() {
    let dir = write_config(r#"
[[agents.entries]]
name = "broken-kiro"
command = "x"
transport = "acp"

[agents.entries.commands]
dispatch = "vendor_exec"
"#);
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["config", "check"])
        .output()
        .expect("spawn");
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout = {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("broken-kiro") && stderr.contains("exec_method"),
        "expected broken-kiro/exec_method in stderr; got: {stderr}"
    );
}
```

Add `tempfile = "3"` to `crates/spur-cli/Cargo.toml` under `[dev-dependencies]` if not already present.

- [ ] **Step 2: Run the failing test.**

Run: `cargo test -p spur-cli --test config_check`
Expected: FAIL — `config check` subcommand doesn't exist; spur currently exits with a clap "unrecognized subcommand" error.

- [ ] **Step 3: Create `commands/config_check.rs`.**

Create `crates/spur-cli/src/commands/mod.rs`:

```rust
pub mod config_check;
```

Create `crates/spur-cli/src/commands/config_check.rs`:

```rust
//! `spur config check` — validates `.spur/config.toml` without starting
//! any agents. Exit 0 if all entries pass; exit 1 if any entry produces
//! a fatal ConfigError. Warnings are reported to stderr but do not flip
//! the exit code.
//!
//! The validation logic itself is in `spur_acp::validate_agent_config`;
//! this module only loads the config, iterates, and formats output.

use std::path::Path;

use spur_acp::{validate_agent_config, SpurConfig};

/// Returns the exit code: 0 on success, 1 on any fatal error.
pub fn run(repo_root: &Path) -> anyhow::Result<i32> {
    let cfg = load_spur_config(repo_root)?;

    if cfg.agents.entries.is_empty() {
        eprintln!("no agents configured in .spur/config.toml");
        return Ok(0);
    }

    let mut fatal_count = 0_usize;
    let mut warn_count = 0_usize;

    for entry in &cfg.agents.entries {
        match validate_agent_config(entry) {
            Ok(()) => {
                println!("\u{2713} {}", entry.name);
            }
            Err(errors) => {
                for e in errors {
                    if e.is_fatal() {
                        eprintln!("\u{2717} {}", e);
                        fatal_count += 1;
                    } else {
                        eprintln!("\u{26a0} {}", e);
                        warn_count += 1;
                    }
                }
            }
        }
    }

    if fatal_count > 0 {
        eprintln!(
            "\nconfig check FAILED: {fatal_count} fatal, {warn_count} warning(s)"
        );
        Ok(1)
    } else {
        if warn_count > 0 {
            eprintln!("\nconfig check OK with {warn_count} warning(s)");
        }
        Ok(0)
    }
}

fn load_spur_config(repo_root: &Path) -> anyhow::Result<SpurConfig> {
    let path = repo_root.join(".spur").join("config.toml");
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    let cfg: SpurConfig = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(cfg)
}
```

- [ ] **Step 4: Wire the subcommand in `main.rs`.**

In `crates/spur-cli/src/main.rs`, add `mod commands;` near the other module declarations.

Add a new variant to the `Commands` enum:

```rust
    /// Validate .spur/config.toml shape
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
```

And a new subcommand enum:

```rust
#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate that every [agents.entries] block has a coherent configuration.
    Check,
}
```

In the main `match cli.command` block, add:

```rust
        Commands::Config { command } => match command {
            ConfigCommands::Check => {
                let exit = commands::config_check::run(&repo_root)?;
                std::process::exit(exit);
            }
        },
```

- [ ] **Step 5: Run the failing test — should now pass.**

Run: `cargo test -p spur-cli --test config_check`
Expected: PASS (both tests).

- [ ] **Step 6: Run workspace tests.**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-cli/src/commands/ \
        crates/spur-cli/src/main.rs \
        crates/spur-cli/tests/config_check.rs \
        crates/spur-cli/Cargo.toml
git commit -m "feat(spur-cli): add \`spur config check\` subcommand

Validates .spur/config.toml without starting any agents. Exit 0 on pass;
exit 1 on any fatal ConfigError (currently only R1: vendor_exec without
exec_method). Warnings (R3: skip-with-no-mechanism) print but do not
flip the exit code.

Useful in CI to catch bad configs before they reach the TUI, and as a
diagnostic when an agent silently fails to start.

Spec 1 task 7."
```

---

## Task 8: Example config + docs

**Files:**
- Create: `.spur/config.toml.example` (or update existing)
- Create: `docs/spur/agent-config.md`

- [ ] **Step 1: Check for existing example config.**

Run: `ls .spur/ | grep -i example`
Expected: either a file exists (update it) or doesn't (create new).

- [ ] **Step 2: Write the example config.**

Create/overwrite `.spur/config.toml.example`:

```toml
# .spur/config.toml.example — ships with claude + kiro worked examples.
# Copy to .spur/config.toml and edit as needed.

[brain]
default = "claude-code-acp"
fallback = ["kiro"]

# ─── Claude (prompt-text dispatch, bypass via ACP session mode) ───────
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
capabilities = ["general", "code", "reasoning"]
cost_tier = "medium"

[agents.entries.display]
handle = "claude"
display_name = "Claude"

[agents.entries.commands]
dispatch = "prompt_text"

[agents.entries.permissions]
skip = true
session_mode = "bypassPermissions"

# ─── Kiro (vendor-exec dispatch, bypass via spawn args) ───────────────
[[agents.entries]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
role = "both"
capabilities = ["kiro-native", "specs"]
cost_tier = "medium"

[agents.entries.display]
handle = "kiro"
display_name = "Kiro"

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
skip = true
args = ["--trust-all-tools"]
```

- [ ] **Step 3: Write the reference documentation.**

Create `docs/spur/agent-config.md`:

```markdown
# Agent configuration reference

This doc describes the `[[agents.entries]]` schema in `.spur/config.toml`.
It was introduced by Spec 1 (2026-04-14). The shape is additive — old
configs that only set `name`, `command`, `args`, `transport`, etc., keep
working unchanged.

## Top-level fields (unchanged from earlier)

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `name` | String | yes | Unique agent id used by `[brain]` and session routing |
| `command` | String | yes | Binary to spawn |
| `args` | Vec<String> | no | CLI args appended to `command` |
| `transport` | enum | yes | `acp`, `stream-json`, `cli-wrap`, or `stdio` |
| `role` | enum | no | `brain`, `worker`, or `both` (default: `worker`) |
| `capabilities` | Vec<String> | no | Routing tags |
| `cost_tier` | enum | no | `low`, `medium`, `high` (default: `medium`) |
| `rate_limit_window` | Duration | no | e.g. `"5m"` |
| `review` | sub-table | no | Per-agent human-review policy |

## `[agents.entries.display]`

| Field | Default | Notes |
|-------|---------|-------|
| `handle` | lowercase(name) | Short alias for `/handle:cmd` on collision |
| `display_name` | name | Reserved for UX (unused in Spec 1) |

## `[agents.entries.commands]`

| Field | Type | Default | When required |
|-------|------|---------|---------------|
| `dispatch` | DispatchKind | `prompt_text` | always optional |
| `exec_method` | String | — | required when `dispatch = "vendor_exec"` |
| `args_template` | ArgsTemplateKind | `raw_rest` | consulted only for `vendor_exec` |
| `ingest` | [[IngestBinding]] | empty | each binding matches one vendor-ext notification method |
| `response` | [[ResponseBinding]] | empty | each binding matches one vendor-ext response method |

**DispatchKind:** `prompt_text`, `vendor_exec`
**ArgsTemplateKind:** `raw_rest`

### IngestBinding

```toml
[[agents.entries.commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"
```

| Field | Kind | Notes |
|-------|------|-------|
| `method` | String | Full wire method to match |
| `parser` | IngestParserKind | Currently only `json_path_list` |
| `path` | String | Dotted JSON path (no `[…]` indexing) |
| `item_schema` | ItemSchemaKind | Currently only `acp_available_command` |

### ResponseBinding

```toml
[[agents.entries.commands.response]]
method = "_kiro.dev/commands/execute"
render = "system_note"
```

**ResponseRenderKind:** `system_note`

## `[agents.entries.permissions]`

Replaces the three flat `skip_permissions*` fields. Old flat fields
still work (promoted transparently by `AgentConfig::effective_permissions`)
but are slated for removal in a future release.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `skip` | bool | false | Enables bypass mode |
| `args` | Vec<String> | empty | Appended to spawn args when `skip = true` |
| `session_mode` | Option<String> | None | ACP session mode set post-new_session when `skip = true` |

## Built-in hook vocabulary

| Hook ID | Kind | Description |
|---------|------|-------------|
| `prompt_text` | DispatchKind | Send `ContentBlock::Text("/cmd args")` on prompt stream |
| `vendor_exec` | DispatchKind | Call a vendor extension RPC |
| `raw_rest` | ArgsTemplateKind | `"/cmd rest…"` → `{ args: { raw: "rest…" } }` |
| `json_path_list` | IngestParserKind | Array at JSON path → items via `item_schema` |
| `acp_available_command` | ItemSchemaKind | Decode each item as `AvailableCommand` |
| `system_note` | ResponseRenderKind | Append trace entry formatted as `⟨handle⟩ response: {params}` |
| `files` | MentionSourceKind | Filesystem file mentions (Spec 2; not wired yet) |

## Validation

On startup, and via `spur config check`, each entry is validated:

- **R1 (fatal):** `dispatch = "vendor_exec"` requires `exec_method`. Agent refuses to start.
- **R3 (warning):** `permissions.skip = true` with no `args` and no `session_mode`. Relies on L2 auto-approve only.

Strongly-typed deserialize covers misspellings: `dispatch = "teleport"` fails at parse with a clear error listing the accepted variants.

## Adding a new agent

For an agent whose behavior matches an existing hook combination, onboarding is a single `[[agents.entries]]` block. See `.spur/config.toml.example` for worked examples (claude + kiro).

If the agent exhibits a genuinely novel dispatch/ingest shape, add a new hook enum variant in `crates/spur-acp/src/config/hooks.rs` plus its implementation in `crates/spur-tui/src/agents/`, then reference it from config. That's out of scope for Spec 1; see the roadmap.
```

- [ ] **Step 4: Commit.**

```bash
git add .spur/config.toml.example docs/spur/agent-config.md
git commit -m "docs: agent-config schema reference + worked .spur/config.toml.example

Ships a drop-in example with full claude-code-acp (prompt_text dispatch,
session-mode bypass) and kiro (vendor_exec dispatch, spawn-args bypass)
blocks so new installs get a working config. docs/spur/agent-config.md
lists every sub-table, every field, and every built-in hook ID with its
purpose.

Spec 1 task 8."
```

---

## Final verification

- [ ] **Step 1: Full workspace test run.**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 2: Clippy.**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Success-criteria greps from the spec.**

Run each:

```bash
grep -rn "if handle == \"kiro\"" crates/
grep -rn "KIRO_COMMANDS_AVAILABLE" crates/spur-tui/src/
grep -rn "Dispatch::KiroExecute\|SubmitDecision::KiroExecute\|Action::KiroExecute" crates/
grep -rn "UserInput::KiroExecute\|InteractiveInput::KiroExecute" crates/
```

Expected: **zero matches** for each.

- [ ] **Step 4: Manual smoke test (optional but recommended).**

Run: `cargo run --bin spur -- config check`
Expected: green `\u2713` lines for every agent in the current `.spur/config.toml`, exit 0.

Run: `cargo run --bin spur -- watch`
Smoke test: open a kiro session, type `/` — the kiro-advertised commands should still appear in the popup; selecting one should still dispatch via vendor_exec. Verify the response renders as a system note. The end-user behavior should be identical to pre-Spec-1; what changed is how it's plumbed.
