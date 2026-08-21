# `/configure` Settings Browser Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-21-configure-settings-browser-design.ipynb`
**Formal @spec cells:** `CONFIGURE-SECTION`, `GRAPH-EMBEDDING`, `SKILLS-PROJECTION`, `TUI-EDIT-MODE`, `SAVE-APPLY`
**Design epic:** `bd-ma96` (closed)

**Goal:** Extend TUI `/configure` from an agent-only browser into a sectioned settings browser over Agents, Graph, TUI, and Skills, with persist-then-apply saves.

**Architecture:** Keep `ViewId::AgentConfigBrowser { preselect }`. Parse `/configure` args with `ConfigureSection` (`agents|graph|tui|skills` vs agent name). Left pane lists sections; right pane is the existing agent editor or a dedicated pane module. Persist through `spur_acp::config::ConfigPatch` + `update_config`. Live-apply only after persist `Ok` (`SAVE-APPLY`). Graph embedding does not swap the process `OnceLock`; skills projection applies to newly reconciled sessions.

**Tech Stack:** Rust 2021 · `spur-tui` ratatui views · `spur-acp` `SpurConfig` / `update_config` · `spur-core` orchestrator `InteractiveInput` · ACP `SpurEventBody`

**Always use** `scripts/spur-cargo` (never bare `cargo`). `fmt` is local.

---

## File structure

| File | Owner task | Responsibility |
|---|---|---|
| `crates/spur-tui/src/configure_section.rs` | task-1 | `ConfigureSection` enum + `parse_configure_arg` |
| `crates/spur-tui/src/views/settings_graph.rs` | task-1 stub / task-4 fill | Graph embedding picker |
| `crates/spur-tui/src/views/settings_tui.rs` | task-1 stub / task-5 fill | TUI prefs pane |
| `crates/spur-tui/src/views/settings_skills.rs` | task-1 stub / task-6 fill | Skills projection pane |
| `crates/spur-tui/src/views/agent_config_browser.rs` | task-1 | Section list + dispatch to panes |
| `crates/spur-acp/src/config/mod.rs` | task-2 | `ConfigPatch` + `apply` |
| `crates/spur-acp/src/domain/events.rs` | task-2 | `ConfigUpdateResult` |
| `crates/spur-core/src/orchestrator/support.rs` | task-2 | persist-then-apply `apply_config_patch` |
| `crates/spur-tui/src/action.rs` | task-3 | `Action::ConfigSaveRequested` |
| `crates/spur-tui/src/app/action_routing/session_config.rs` | task-3 | send patch without optimistic apply; `/vim` persist |
| `crates/spur-tui/src/app/events.rs` | task-3 | apply on `ok` result events |
| `docs/user-docs/05-configuration.md` | task-7 | `/configure` user docs |

---

## DAG

```
task-1-section-shell ─────────────────────────────────────┐
task-2-persist-core ──► task-3-persist-tui ──┬─ task-4-graph-pane ──┐
                                             ├─ task-5-tui-pane ────┼─ task-7-docs
                                             └─ task-6-skills-pane ─┘
```

task-1 and task-2 have no dependencies (parallel). Panes must not edit `agent_config_browser.rs` or `session_config.rs`.

---

### Task 1: Section shell (`CONFIGURE-SECTION`)

**Task ID:** `task-1-section-shell`

**Files:**
- Create: `crates/spur-tui/src/configure_section.rs`
- Create: `crates/spur-tui/src/views/settings_graph.rs`
- Create: `crates/spur-tui/src/views/settings_tui.rs`
- Create: `crates/spur-tui/src/views/settings_skills.rs`
- Modify: `crates/spur-tui/src/lib.rs` (add `pub mod configure_section;`)
- Modify: `crates/spur-tui/src/views/mod.rs` (add `pub mod settings_graph; pub mod settings_tui; pub mod settings_skills;`)
- Modify: `crates/spur-tui/src/views/agent_config_browser.rs`
- Modify: `crates/spur-tui/src/commands/spur_local.rs` (description + hint)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `parse_configure_arg` is a total function over the four sections plus agent names
- [ ] `/configure graph|tui|skills` focuses that section; other tokens still preselect an agent
- [ ] Left pane lists `agents`, `graph`, `tui`, `skills`; agents pane still edits agents
- [ ] Non-agent sections render the stub pane (title only); later tasks own those files
- [ ] `scripts/spur-cargo test -p spur-tui configure_section -- --nocapture` passes
- [ ] `scripts/spur-cargo test -p spur-tui --lib agent_config_browser -- --nocapture` passes

**Suggested Worker:** kiro

**Scope Boundary:**
- IN scope: files listed above
- OUT of scope: persist/`UserInput`/`ConfigPatch`, filling pane widgets beyond a titled placeholder, `action.rs`
- If you need OUT-OF-SCOPE files, emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Write the failing tests** in `crates/spur-tui/src/configure_section.rs`

```rust
//! Section focus for `/configure` (`CONFIGURE-SECTION`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureSection {
    Agents,
    Graph,
    Tui,
    Skills,
}

impl ConfigureSection {
    pub const ALL: [Self; 4] = [Self::Agents, Self::Graph, Self::Tui, Self::Skills];

    pub fn parse_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "agents" | "agent" => Some(Self::Agents),
            "graph" => Some(Self::Graph),
            "tui" => Some(Self::Tui),
            "skills" | "skill" => Some(Self::Skills),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Graph => "graph",
            Self::Tui => "tui",
            Self::Skills => "skills",
        }
    }
}

/// Empty / omitted arg → agents. Reserved tokens → that section.
/// Any other token → agents + agent preselect (Phase 1).
pub fn parse_configure_arg(arg: &str) -> (ConfigureSection, Option<String>) {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return (ConfigureSection::Agents, None);
    }
    if let Some(section) = ConfigureSection::parse_token(trimmed) {
        return (section, None);
    }
    (ConfigureSection::Agents, Some(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arg_focuses_agents() {
        assert_eq!(
            parse_configure_arg(""),
            (ConfigureSection::Agents, None)
        );
        assert_eq!(
            parse_configure_arg("   "),
            (ConfigureSection::Agents, None)
        );
    }

    #[test]
    fn reserved_tokens_focus_sections() {
        assert_eq!(
            parse_configure_arg("graph"),
            (ConfigureSection::Graph, None)
        );
        assert_eq!(
            parse_configure_arg("TUI"),
            (ConfigureSection::Tui, None)
        );
        assert_eq!(
            parse_configure_arg("skills"),
            (ConfigureSection::Skills, None)
        );
        assert_eq!(
            parse_configure_arg("agents"),
            (ConfigureSection::Agents, None)
        );
    }

    #[test]
    fn unknown_token_is_agent_preselect() {
        assert_eq!(
            parse_configure_arg("kiro"),
            (ConfigureSection::Agents, Some("kiro".into()))
        );
    }
}
```

Put the tests at the bottom of the new file. First commit the tests + empty `todo!()` parse if you prefer strict TDD; then fill the impl in the same file (module is new, so the first compile of the tests fails until the types exist).

- [ ] **Step 2: Run tests to verify they fail** (before adding `parse_configure_arg`)

```bash
scripts/spur-cargo test -p spur-tui configure_section -- --nocapture
```

Expected: FAIL (module / function missing) then GREEN after Step 3.

- [ ] **Step 3: Wire the view**

`AgentConfigBrowserView` gains:

```rust
section: crate::configure_section::ConfigureSection,
graph_pane: crate::views::settings_graph::GraphPane,
tui_pane: crate::views::settings_tui::TuiPane,
skills_pane: crate::views::settings_skills::SkillsPane,
```

In `new` / `set_entries` / `apply_preselect`:

```rust
let (section, agent) = crate::configure_section::parse_configure_arg(preselect.unwrap_or(""));
self.section = section;
// existing agent preselect uses `agent.as_deref()`
```

Left pane: when `BrowserPane::Agents` and `section == Agents`, keep the agent list. Add a section-row list (Tab or a third pane state is NOT required): render section names above or as the first column. Simplest: extend `BrowserPane` with `Sections` as the leftmost list:

```rust
enum BrowserPane { Sections, Agents, Fields }
```

- Left: `ConfigureSection::ALL` labels; Enter/Right focuses that section's content.
- For `Agents`, the existing agent list + fields remain.
- For other sections, skip the agent list and render the stub pane in the right area.

Keep existing keybinds for the agents fields. Section list uses Up/Down. Do not change `Action` variants.

Stub pane files (task-4/5/6 replace bodies):

```rust
// crates/spur-tui/src/views/settings_graph.rs
use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, widgets::{Block, Borders, Paragraph}, Frame};
use crate::action::Action;

pub struct GraphPane;

impl GraphPane {
    pub fn new(_current: Option<&str>) -> Self { Self }
    pub fn render(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new("graph").block(Block::default().borders(Borders::ALL).title("graph")),
            area,
        );
    }
    pub fn handle_key(&mut self, _key: KeyEvent) -> Option<Action> { None }
}
```

Mirror for `TuiPane` (`title("tui")`) and `SkillsPane` (`title("skills")`).

`spur_local.rs` configure entry:

```rust
description: "Open settings browser".into(),
hint: Some("[section|agent-name]".into()),
```

Add a view test next to `preselect_focuses_named_agent`:

```rust
#[test]
fn graph_token_focuses_graph_section() {
    let view = AgentConfigBrowserView::new(vec![configured_agent()], Some("graph".into()));
    assert_eq!(
        view.section_for_test(),
        crate::configure_section::ConfigureSection::Graph
    );
}

#[test]
fn agent_token_still_preselects_agent() {
    let mut second = configured_agent();
    second.name = "kiro".into();
    let view = AgentConfigBrowserView::new(vec![configured_agent(), second], Some("kiro".into()));
    assert_eq!(view.section_for_test(), crate::configure_section::ConfigureSection::Agents);
    assert_eq!(view.selected_agent_name_for_test(), Some("kiro"));
}
```

Expose `section_for_test` under `#[cfg(any(test, debug_assertions))]` like `selected_agent_name_for_test`.

- [ ] **Step 4: Run tests**

```bash
scripts/spur-cargo test -p spur-tui configure_section -- --nocapture
scripts/spur-cargo test -p spur-tui --lib agent_config_browser -- --nocapture
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/configure_section.rs \
  crates/spur-tui/src/views/settings_graph.rs \
  crates/spur-tui/src/views/settings_tui.rs \
  crates/spur-tui/src/views/settings_skills.rs \
  crates/spur-tui/src/lib.rs \
  crates/spur-tui/src/views/mod.rs \
  crates/spur-tui/src/views/agent_config_browser.rs \
  crates/spur-tui/src/commands/spur_local.rs
git commit -m "feat(spur-tui): S-configure add sectioned /configure shell"
```

---

### Task 2: `ConfigPatch` + orchestrator persist (`SAVE-APPLY` core)

**Task ID:** `task-2-persist-core`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-acp/src/lib.rs` (re-export `ConfigPatch`, `SkillsProjectionMode`)
- Create: `crates/spur-acp/tests/config_patch.rs`
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/tests/executor_events_roundtrip.rs`
- Modify: `crates/spur-core/src/orchestrator/input.rs`
- Modify: `crates/spur-core/src/orchestrator/support.rs`
- Modify: `crates/spur-core/src/orchestrator/interactive_loop.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ConfigPatch::apply` writes exactly one section; unknown embedding alias returns `Err`
- [ ] Persist failure does not write `agent_configs` lock
- [ ] `ConfigUpdateResult` round-trips
- [ ] `update_agent_config` still works (delegate to patch apply) — existing delegation live-apply test still passes
- [ ] `scripts/spur-cargo test -p spur-acp config_patch -- --nocapture` passes
- [ ] `scripts/spur-cargo test -p spur-acp agent_config_update_result_roundtrips -- --nocapture` passes
- [ ] `scripts/spur-cargo test -p spur-core update_agent_config -- --nocapture` (or the existing live-apply test name) passes

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: files listed
- OUT of scope: TUI `Action` / `session_config.rs` / pane files / `/vim`
- Emit `scope_drift` if you need TUI wiring (that is task-3)

**Implementation:**

- [ ] **Step 1: Failing test** `crates/spur-acp/tests/config_patch.rs`

```rust
use spur_acp::config::{
    ConfigPatch, EditorMode, SkillsProjectionMode, SpurConfig,
};

#[test]
fn graph_patch_writes_canonical_alias() {
    let mut cfg = SpurConfig::default();
    ConfigPatch::GraphEmbeddingModel {
        alias: "coderank".into(),
    }
    .apply(&mut cfg)
    .unwrap();
    assert_eq!(cfg.graph.embedding_model.as_deref(), Some("coderank"));
}

#[test]
fn graph_patch_rejects_unknown_alias() {
    let mut cfg = SpurConfig::default();
    let err = ConfigPatch::GraphEmbeddingModel {
        alias: "not-a-model".into(),
    }
    .apply(&mut cfg)
    .expect_err("unknown alias");
    assert!(format!("{err:#}").contains("embedding"));
    assert!(cfg.graph.embedding_model.is_none());
}

#[test]
fn skills_and_tui_patches_do_not_clobber_siblings() {
    let mut cfg = SpurConfig::default();
    cfg.tui.theme = "light".into();
    ConfigPatch::TuiEditMode(EditorMode::Vim)
        .apply(&mut cfg)
        .unwrap();
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
    assert_eq!(cfg.tui.theme, "light");

    ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::AllActive)
        .apply(&mut cfg)
        .unwrap();
    assert_eq!(cfg.skills.projection_mode, SkillsProjectionMode::AllActive);
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
}

#[test]
fn section_id_matches_configure_tokens() {
    assert_eq!(
        ConfigPatch::GraphEmbeddingModel {
            alias: "nomic".into()
        }
        .section_id(),
        "graph"
    );
    assert_eq!(
        ConfigPatch::TuiDisablePasteBurst(true).section_id(),
        "tui"
    );
    assert_eq!(
        ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::CatalogOnly).section_id(),
        "skills"
    );
}
```

Canonical graph aliases (TUI writes only these): `nomic`, `coderank`, `jina-code`.

```rust
pub const GRAPH_EMBEDDING_ALIASES: &[&str] = &["nomic", "coderank", "jina-code"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigPatch {
    Agent {
        name: String,
        updated_entry: AgentConfig,
    },
    GraphEmbeddingModel { alias: String },
    TuiEditMode(EditorMode),
    TuiTheme(String),
    TuiDisablePasteBurst(bool),
    SkillsProjectionMode(SkillsProjectionMode),
}

impl ConfigPatch {
    pub fn section_id(&self) -> &'static str { /* agents|graph|tui|skills */ }

    pub fn apply(&self, cfg: &mut SpurConfig) -> anyhow::Result<()> {
        match self {
            Self::GraphEmbeddingModel { alias } => {
                if !GRAPH_EMBEDDING_ALIASES.contains(&alias.as_str()) {
                    anyhow::bail!("unsupported embedding model alias '{alias}'");
                }
                cfg.graph.embedding_model = Some(alias.clone());
            }
            Self::TuiEditMode(mode) => cfg.tui.edit_mode = *mode,
            Self::TuiTheme(name) => cfg.tui.theme = name.clone(),
            Self::TuiDisablePasteBurst(v) => cfg.tui.disable_paste_burst = *v,
            Self::SkillsProjectionMode(mode) => cfg.skills.projection_mode = *mode,
            Self::Agent { name, updated_entry } => {
                let slot = cfg
                    .agents
                    .entries
                    .iter_mut()
                    .find(|e| e.name == *name)
                    .ok_or_else(|| anyhow::anyhow!("agent '{name}' is not configured"))?;
                // copy only curated fields; keep identity from slot
                slot.args = updated_entry.args.clone();
                slot.additional_directories = updated_entry.additional_directories.clone();
                slot.capabilities = updated_entry.capabilities.clone();
                slot.skip_permissions = updated_entry.skip_permissions;
                slot.skip_permissions_args = updated_entry.skip_permissions_args.clone();
                slot.skip_permissions_session_mode =
                    updated_entry.skip_permissions_session_mode.clone();
                slot.profile = updated_entry.profile.clone();
            }
        }
        Ok(())
    }
}
```

Add `SpurEventBody::ConfigUpdateResult { section: String, ok: bool, message: String }` next to `AgentConfigUpdateResult`. Keep the agent variant. Round-trip test modeled on `agent_config_update_result_roundtrips`.

- [ ] **Step 2: Run tests (expect fail)**

```bash
scripts/spur-cargo test -p spur-acp config_patch -- --nocapture
```

- [ ] **Step 3: Orchestrator**

`InteractiveInput::UpdateConfig { patch: spur_acp::config::ConfigPatch }`. Keep `UpdateAgentConfig` and implement it by calling the new method.

In `support.rs`, add `apply_config_patch(&mut self, patch: ConfigPatch) -> Result<()>`:

1. For `Agent`, run the existing name-mismatch + absolute-directory checks **before** persist (same as `update_agent_config`).
2. `update_config(&config_path, |c| patch.apply(c))?;` — if this `Err`s, return immediately.
3. Only then mutate `self.config` and, for `Agent`, `*self.agent_configs.write()`.
4. Do **not** swap any embedding runtime.

`interactive_loop.rs`: handle `UpdateConfig` by `apply_config_patch`; emit `ConfigUpdateResult`. `UpdateAgentConfig` may keep emitting `AgentConfigUpdateResult` (existing TUI) **or** both; do not remove `AgentConfigUpdateResult`.

Refactor `update_agent_config` to construct `ConfigPatch::Agent` and call `apply_config_patch`, then map the result so the existing delegation test stays green.

- [ ] **Step 4: Run tests**

```bash
scripts/spur-cargo test -p spur-acp config_patch -- --nocapture
scripts/spur-cargo test -p spur-acp --test executor_events_roundtrip config_update -- --nocapture
scripts/spur-cargo test -p spur-core --lib -- --nocapture
```

If the last command is too broad, run the existing `update_agent_config` live-apply test in `delegation/mod.rs` by name.

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-acp/src/lib.rs \
  crates/spur-acp/tests/config_patch.rs crates/spur-acp/src/domain/events.rs \
  crates/spur-acp/tests/executor_events_roundtrip.rs \
  crates/spur-core/src/orchestrator/input.rs \
  crates/spur-core/src/orchestrator/support.rs \
  crates/spur-core/src/orchestrator/interactive_loop.rs
git commit -m "feat(spur-acp): S-configure add ConfigPatch persist-then-apply"
```

---

### Task 3: TUI persist-then-apply wire + `/vim` persist (`SAVE-APPLY` TUI)

**Task ID:** `task-3-persist-tui`

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app/mod.rs` (`UserInput`)
- Modify: `crates/spur-tui/src/app/action_routing/mod.rs`
- Modify: `crates/spur-tui/src/app/action_routing/session_config.rs`
- Modify: `crates/spur-tui/src/app/events.rs`
- Modify: `crates/spur-cli/src/main.rs` (`UserInput` → `InteractiveInput` map)

**Depends on:** `task-2-persist-core`

**Acceptance Criteria:**
- [ ] `AgentConfigSaveRequested` does **not** mutate `App.config` before the orchestrator result
- [ ] On `AgentConfigUpdateResult { ok: true }` (and `ConfigUpdateResult { ok: true }`), TUI applies in-memory state
- [ ] On `ok: false`, TUI does not apply; flash the error
- [ ] `/vim` persists `tui.edit_mode` via `ConfigPatch::TuiEditMode` then applies on success
- [ ] `scripts/spur-cargo test -p spur-tui --lib -- --nocapture` still passes existing session_config tests
- [ ] `scripts/spur-cargo test -p spur-cli` mapping compiles (at least `scripts/spur-cargo check -p spur-tui -p spur-cli -p spur-core`)

**Suggested Worker:** claude-code-acp

**Scope Boundary:**
- IN scope: files listed
- OUT of scope: pane widgets, `agent_config_browser.rs`, `configure_section.rs`, docs
- Emit `scope_drift` if you need to edit pane files

**Implementation:**

- [ ] **Step 1: Add action + input**

```rust
// action.rs
ConfigSaveRequested {
    patch: spur_acp::config::ConfigPatch,
},
```

Keep `AgentConfigSaveRequested`. Route both in `action_routing/mod.rs` into `process_session_config`.

```rust
// app/mod.rs UserInput
UpdateConfig {
    patch: spur_acp::config::ConfigPatch,
},
```

Keep `UpdateAgentConfig`. CLI map `UpdateConfig` → `InteractiveInput::UpdateConfig`.

- [ ] **Step 2: Fix optimistic apply** in `session_config.rs`

`AgentConfigSaveRequested` today mutates `self.config` then sends. Change to:

1. Validate the named agent exists (flash if not).
2. Do **not** write `self.config` / `replace_agent_config` / `sync_dashboard_workers`.
3. `try_send(UserInput::UpdateAgentConfig { .. })` (or `UpdateConfig { ConfigPatch::Agent { .. } }`).

On `SpurEventBody::AgentConfigUpdateResult` in `events.rs`:

- if `ok`: update `self.config.agents.entries`, `agent_config_browser.replace_agent_config`, `sync_dashboard_workers`, flash success
- else: flash failure only

On `ConfigUpdateResult`:

- if `ok && section == "tui"`: copy from a pending draft **or** re-read fields from the patch you sent. Simplest: keep `pending_config_patch: Option<ConfigPatch>` on `App`, set it when sending, on ok apply to `self.config` + live hooks, then clear.
- Live hooks:
  - `TuiEditMode` → set `self.edit_mode` (`From<EditorMode>`), `dashboard.set_edit_mode`, `session_detail.set_edit_mode`
  - `TuiTheme` → `load_runtime_theme` + replace `self.theme` (same as `/theme` apply, but **after** persist ok)
  - `TuiDisablePasteBurst` → `dashboard.set_disable_paste_burst` + `session_detail.set_disable_paste_burst`
  - Graph / Skills: update `self.config` only (no embedder swap, no session reproject)
- if `!ok`: clear pending, flash error, do not apply

`ToggleVimMode`: compute next `EditorMode`, `try_send(UserInput::UpdateConfig { patch: TuiEditMode(next) })`, do **not** toggle `self.edit_mode` until `ConfigUpdateResult` ok. If `user_input_tx` is `None` (tests), keep current in-memory toggle so unit tests without a backend do not freeze.

- [ ] **Step 3: Unit test the send-without-apply path**

If `App` is hard to construct, extract:

```rust
pub(crate) fn agent_save_should_mutate_before_send() -> bool {
    false
}

#[test]
fn save_apply_is_persist_then_apply() {
    assert!(!agent_save_should_mutate_before_send());
}
```

That is too weak as the only test — also add a `session_config` test module that documents the handler no longer assigns `config.agents.entries` before `try_send`. Prefer a focused test with a fake `mpsc` if `App` test helpers exist (`test_support`). If no App fixture, add a comment at the removed mutation and cover apply-on-event with a small helper:

```rust
pub fn apply_config_patch_locally(cfg: &mut SpurConfig, patch: &ConfigPatch) {
    let _ = patch.apply(cfg);
}
```

called only from the `ok` event arm. Test `apply_config_patch_locally` on a `SpurConfig`.

- [ ] **Step 4: Check + test**

```bash
scripts/spur-cargo check -p spur-tui -p spur-cli -p spur-core
scripts/spur-cargo test -p spur-tui --lib -- --nocapture
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/app/mod.rs \
  crates/spur-tui/src/app/action_routing/mod.rs \
  crates/spur-tui/src/app/action_routing/session_config.rs \
  crates/spur-tui/src/app/events.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-tui): S-configure persist-then-apply config saves"
```

---

### Task 4: Graph pane (`GRAPH-EMBEDDING`)

**Task ID:** `task-4-graph-pane`

**Files:**
- Modify: `crates/spur-tui/src/views/settings_graph.rs` **only**

**Depends on:** `task-1-section-shell`, `task-3-persist-tui`

**Acceptance Criteria:**
- [ ] Picker cycles `nomic` / `coderank` / `jina-code` only
- [ ] Save emits `Action::ConfigSaveRequested { patch: GraphEmbeddingModel { alias } }` with canonical alias (`jina-code` not `jina_code`)
- [ ] If `SPUR_EMBEDDING_MODEL` is set, render a one-line banner that env wins at read time
- [ ] Hint text: takes effect on next embedding load (restart)
- [ ] `scripts/spur-cargo test -p spur-tui --lib settings_graph -- --nocapture` passes

**Suggested Worker:** kiro

**Scope Boundary:**
- IN scope: `settings_graph.rs`
- OUT of scope: `agent_config_browser.rs`, orchestrator, other panes
- Emit `scope_drift` if the stub API from task-1 is insufficient — then add methods **in this file only**

**Implementation:**

- [ ] **Step 1: Tests** in `settings_graph.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_is_total_over_canonical_aliases() {
        let mut pane = GraphPane::new(None);
        assert_eq!(pane.selected_alias(), "nomic");
        pane.cycle();
        assert_eq!(pane.selected_alias(), "coderank");
        pane.cycle();
        assert_eq!(pane.selected_alias(), "jina-code");
        pane.cycle();
        assert_eq!(pane.selected_alias(), "nomic");
    }

    #[test]
    fn new_selects_current_alias() {
        let pane = GraphPane::new(Some("jina-code"));
        assert_eq!(pane.selected_alias(), "jina-code");
    }

    #[test]
    fn save_patch_uses_canonical_alias() {
        let pane = GraphPane::new(Some("coderank"));
        match pane.save_patch() {
            spur_acp::config::ConfigPatch::GraphEmbeddingModel { alias } => {
                assert_eq!(alias, "coderank");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run (fail)**

```bash
scripts/spur-cargo test -p spur-tui --lib settings_graph -- --nocapture
```

- [ ] **Step 3: Implement**

`GraphPane { aliases: [&'static str; 3], selected: usize }`. `new(current)` maps unknown/None → index 0 (`nomic`). `handle_key`: Left/Right or Enter cycles; the same save keybind the agent pane uses (whatever `agent_config_browser` already forwards via `graph_pane.handle_key`). Return `Some(Action::ConfigSaveRequested { patch: self.save_patch() })` on save.

`render`: list three aliases with a cursor on `selected`. If `std::env::var("SPUR_EMBEDDING_MODEL")` is `Ok`, add a line `SPUR_EMBEDDING_MODEL overrides this at read time`. Footer: `takes effect on next embedding load (restart)`.

Task-1 already constructs `GraphPane::new(_)` — keep `new(Option<&str>)` signature. The shell may pass `None` until you also thread `self.config.graph.embedding_model` **without** editing the shell if the stub already accepted `Option<&str>`. If the shell passes `None` only, still work; optionally document that the shell should pass current config — **do not edit the shell**. If current value is needed, read it inside `new` is impossible; keep `None` → nomic default for the stub constructor and select `nomic` until the user cycles. (Shell in task-1 should pass current model if easy; if task-1 already passes `_current`, task-4 uses it.)

If task-1's `GraphPane::new(_current: Option<&str>)` is unused, start using `_current`.

- [ ] **Step 4: Tests pass**

```bash
scripts/spur-cargo test -p spur-tui --lib settings_graph -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/settings_graph.rs
git commit -m "feat(spur-tui): S-configure graph embedding picker"
```

---

### Task 5: TUI pane (`TUI-EDIT-MODE` + theme + paste-burst)

**Task ID:** `task-5-tui-pane`

**Files:**
- Modify: `crates/spur-tui/src/views/settings_tui.rs` **only**

**Depends on:** `task-1-section-shell`, `task-3-persist-tui`

**Acceptance Criteria:**
- [ ] `edit_mode` cycles `emacs` / `vim` only
- [ ] `theme` cycles discovered names from `crate::theme::list_available_themes()` (built-in + project + user); unknown current name is not saved
- [ ] `disable_paste_burst` toggles bool
- [ ] Each field save emits the matching `ConfigPatch` variant via `Action::ConfigSaveRequested`
- [ ] `scripts/spur-cargo test -p spur-tui --lib settings_tui -- --nocapture` passes

**Suggested Worker:** kiro

**Scope Boundary:**
- IN scope: `settings_tui.rs`
- OUT of scope: `/theme` command in `overlays.rs`, `session_config.rs` `/vim` (task-3), other panes
- Emit `scope_drift` if you need `overlays.rs`

**Implementation:**

- [ ] **Step 1: Tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::config::{ConfigPatch, EditorMode};

    #[test]
    fn edit_mode_cycles_emacs_vim() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        assert!(matches!(pane.edit_mode_patch(), ConfigPatch::TuiEditMode(EditorMode::Emacs)));
        pane.cycle_edit_mode();
        assert!(matches!(pane.edit_mode_patch(), ConfigPatch::TuiEditMode(EditorMode::Vim)));
        pane.cycle_edit_mode();
        assert!(matches!(pane.edit_mode_patch(), ConfigPatch::TuiEditMode(EditorMode::Emacs)));
    }

    #[test]
    fn paste_burst_toggle() {
        let mut pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        pane.toggle_paste_burst();
        assert!(matches!(
            pane.paste_burst_patch(),
            ConfigPatch::TuiDisablePasteBurst(true)
        ));
    }

    #[test]
    fn theme_patch_uses_selected_name() {
        let pane = TuiPane::new_for_test(EditorMode::Emacs, "dark".into(), false);
        match pane.theme_patch() {
            ConfigPatch::TuiTheme(name) => assert_eq!(name, "dark"),
            other => panic!("{other:?}"),
        }
    }
}
```

`new_for_test` is `#[cfg(test)]` and does not call `list_available_themes`. Production `new(edit_mode, theme, disable_paste_burst)` loads available themes and if `theme` is not in the list, keep it selected but `theme_save_allowed()` is false until the user cycles onto a discovered name.

- [ ] **Step 2: Run fail / Step 3: Implement**

Fields: `edit_mode: EditorMode`, `theme: String`, `themes: Vec<String>`, `disable_paste_burst: bool`, `selected_field: usize` (0 mode, 1 theme, 2 paste). `handle_key`: Up/Down field; Left/Right or space cycles the focused field; save key emits **only the focused field's patch** (one `ConfigPatch` per save, not a combined struct).

`render`: three rows. Do not call `update_config` here.

- [ ] **Step 4: Test**

```bash
scripts/spur-cargo test -p spur-tui --lib settings_tui -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/settings_tui.rs
git commit -m "feat(spur-tui): S-configure TUI prefs pane"
```

---

### Task 6: Skills pane (`SKILLS-PROJECTION`)

**Task ID:** `task-6-skills-pane`

**Files:**
- Modify: `crates/spur-tui/src/views/settings_skills.rs` **only**

**Depends on:** `task-1-section-shell`, `task-3-persist-tui`

**Acceptance Criteria:**
- [ ] Mode cycles `catalog_only` / `all_active` only
- [ ] Save emits `ConfigPatch::SkillsProjectionMode`
- [ ] Hint: applies to newly reconciled sessions
- [ ] One-line consequence for `all_active` (larger projected skill set), no confirmation modal
- [ ] `scripts/spur-cargo test -p spur-tui --lib settings_skills -- --nocapture` passes

**Suggested Worker:** kiro

**Scope Boundary:**
- IN scope: `settings_skills.rs`
- OUT of scope: skill projection runtime, `bundled_dir`, other panes
- Emit `scope_drift` if you need `spur-core` session reconcile

**Implementation:**

- [ ] **Step 1: Tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::config::{ConfigPatch, SkillsProjectionMode};

    #[test]
    fn mode_cycles_catalog_only_all_active() {
        let mut pane = SkillsPane::new(SkillsProjectionMode::CatalogOnly);
        pane.cycle();
        match pane.save_patch() {
            ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::AllActive) => {}
            other => panic!("{other:?}"),
        }
        pane.cycle();
        match pane.save_patch() {
            ConfigPatch::SkillsProjectionMode(SkillsProjectionMode::CatalogOnly) => {}
            other => panic!("{other:?}"),
        }
    }
}
```

- [ ] **Step 2–4: Implement + test**

`SkillsPane { mode: SkillsProjectionMode }`. `handle_key` cycles on Left/Right/space; save → `Action::ConfigSaveRequested`. Render both labels, cursor on current, footer `applies to newly reconciled sessions`. When `AllActive` is selected, extra line: `projects every bundled and accepted pool skill (large context)`.

```bash
scripts/spur-cargo test -p spur-tui --lib settings_skills -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/settings_skills.rs
git commit -m "feat(spur-tui): S-configure skills projection pane"
```

---

### Task 7: User docs

**Task ID:** `task-7-docs`

**Files:**
- Modify: `docs/user-docs/05-configuration.md`
- Modify: `crates/spur-graph/README.md` only if it still says env-only for embedding (keep toml + `/configure graph` mention)

**Depends on:** `task-4-graph-pane`, `task-5-tui-pane`, `task-6-skills-pane`

**Acceptance Criteria:**
- [ ] Documents `/configure` sections, persist-then-apply, `/vim` now persists, embedding restart caveat, skills next-session caveat, env override for embedding
- [ ] No claim that `/configure` edits brain/cost/pm or agent identity
- [ ] No unimplemented features described

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: user-docs (+ graph README if it contradicts)
- OUT of scope: code

**Implementation:**

- [ ] **Step 1:** After the existing “Graph embedding model” section in `docs/user-docs/05-configuration.md`, add:

```markdown
## In-TUI settings (`/configure`)

`/configure` opens a settings browser (exclusive TUI command). Sections:

| Arg | Section | What it edits | When it applies |
|---|---|---|---|
| (none) or an agent name | Agents | curated worker fields (not name/command/transport/kind) | next delegation |
| `graph` | Graph | `embedding_model` = `nomic` \| `coderank` \| `jina-code` | next embedding load (restart) |
| `tui` | TUI | `edit_mode`, `theme`, `disable_paste_burst` | immediately after a successful save |
| `skills` | Skills | `projection_mode` = `catalog_only` \| `all_active` | newly reconciled sessions |

Saves write the repository `.spur/config.toml` via the same `update_config` helper as `/theme`. If the write fails, memory is not updated. `/theme` remains a shortcut. `/vim` now persists `tui.edit_mode` as well as toggling the current session.

`SPUR_EMBEDDING_MODEL` still overrides `graph.embedding_model` when set.
```

Keep the existing toml examples. Do not duplicate the whole agent schema.

- [ ] **Step 2:** Commit

```bash
git add docs/user-docs/05-configuration.md crates/spur-graph/README.md
git commit -m "docs: S-configure document /configure settings browser"
```

---

## Self-review

**Spec coverage:**
- `CONFIGURE-SECTION` → task-1
- `GRAPH-EMBEDDING` → task-4
- `TUI-EDIT-MODE` + theme + paste → task-5 (`/vim` persist → task-3)
- `SKILLS-PROJECTION` → task-6
- `SAVE-APPLY` → task-2 + task-3
- Command copy → task-1
- User docs → task-7
- Non-goals (brain/pm/identity/OnceLock/hot-reload) — no task implements them

**DAG:** two roots; three parallel panes; docs last. No cycles.

**File isolation:** panes exclusive; persist-tui exclusive on `session_config.rs`; shell exclusive on `agent_config_browser.rs`.
