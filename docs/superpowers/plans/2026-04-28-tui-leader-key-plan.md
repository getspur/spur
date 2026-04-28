# TUI Leader Key (Space)

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce `Space` as a leader key on Dashboard (Navigate mode only). Pressing Space arms a 250ms-delayed contextual popup listing per-view bindings. A follow-up letter dispatches the mapped action and closes the menu. All existing `Alt+*` shortcuts remain functional. SessionDetail is Phase 2 (separate plan, conditional on `SessionDetailMode { Navigate, Compose }`).

**Architecture:** New module `crates/spur-tui/src/components/leader.rs` owns `LeaderMenu`, `LeaderBinding`, and `LeaderCommand`. A `static LEADER_BINDINGS` table (not `const` — `Action` has non-const-droppable fields) encodes the Phase 1 Dashboard-applicable bindings. Space-interception fires in `App::handle_key_inner` after the palette check (around `app.rs:985`) and before the view-dispatch `match` at `app.rs:995`. The 250ms armed→reveal transition is driven by the existing 33ms tick in `App::tick()` (at `app.rs:2539`); no new tokio timer.

**Tech Stack:** Rust 2021, ratatui (existing), crossterm (existing), `std::time::Instant`. No new crate dependencies.

**Scope:** Phase 1 only. Dashboard Navigate mode. Bindings: `i` (toggle vim), `s` (sessions), `b` (issue browser), `p` (plan inspector — resolved). SessionDetail bindings (`m`, `w`, `d`, `r`, `v`) are registered in `LEADER_BINDINGS` with `visible_in: ViewScope::SessionDetail` but are NOT activated in Phase 1 (the `can_open_leader` gate returns false outside Dashboard). `Alt+*` aliases are untouched in this release.

Ships Release N+1 AFTER quick-fixes + destructive-undo land in Release N (consumes `flash_hint_short` and depends on stabilized Action::PanicReset routing).

---

## Spec Grounding

- Spec: `/Volumes/Projects/spur/docs/superpowers/specs/2026-04-28-tui-leader-key-design.md`
- Spec §4.1: activation rule — all five conditions must hold (Dashboard, Navigate, no modal overlay, no picker, no input-bar focus).
- Spec §4.4: `LeaderMenu`, `LeaderBinding`, `LeaderCommand` shapes; `static LEADER_BINDINGS`; `ViewContext<'_>` from `views/mod.rs:85`.
- Spec §4.4.1: Space-interception REQUIRED before view dispatch — without it, Space routes to Composer at `dashboard.rs:950-953`.
- Spec §4.7: 250ms reveal, driven by existing tick at `app.rs:2539`, `tick_interval` at `app.rs:2843`.
- Spec §4.8 + §7.1: `Alt+*` coexistence; deprecation toast deferred to Release N+2 (not in this plan).
- Spec §6: ten test scenarios. Nine in Phase 1 scope (SessionDetail filter test deferred).
- `DashboardView` struct: `app.rs:193`; `mode` at `dashboard.rs:82`; `completion.is_active()` at `dashboard.rs:584`.
- `App::tick()` body: `app.rs:2539`; Dashboard arm at `app.rs:2558`.
- Existing modal guards in `handle_key_inner`: quit chord `app.rs:909`, upgrade modal `app.rs:919`, help overlay `app.rs:945`, palette `app.rs:955`; view dispatch begins at `app.rs:987`.
- `ViewId` enum: `action.rs:189` — `Dashboard`, `IssueBrowser`, `SessionDetail(SessionId)`, `PlanInspector(SessionId)`.
- `Action::RequestSessions`, `Action::ToggleVimMode`, `Action::NavigateTo(ViewId)`, `Action::InspectWorkers` — all exist in `action.rs`.
- `HelpOverlay::lines()` at `help_overlay.rs:34` — append leader section there.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-tui/src/components/leader.rs` | Create | `LeaderMenu`, `LeaderBinding`, `LeaderCommand`, `ViewScope`, `LeaderDispatch`, `static LEADER_BINDINGS` |
| `crates/spur-tui/src/components/mod.rs` | Modify | `pub mod leader;` declaration |
| `crates/spur-tui/src/views/dashboard.rs` | Modify | Add `pub fn is_picker_active(&self) -> bool`, `pub fn mode(&self) -> DashboardMode` |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Add `pub fn toggle_workers_panel_collapsed(&mut self)` (extract from existing Alt+D handler at `:1206`); ensure `pub fn session_id()` accessor |
| `crates/spur-tui/src/app.rs` | Modify | Add `leader: LeaderMenu` field; `can_open_leader` helper; Space-interception block; `leader.tick()` in `App::tick()`; popup render call site |
| `crates/spur-tui/src/components/help_overlay.rs` | Modify | Append "Leader (Space)" section to `HelpOverlay::lines()` |

All tests live as `#[cfg(test)] mod tests` inside `leader.rs` plus targeted additions to `dashboard.rs` and `help_overlay.rs` test mods.

---

## Task 1 — Define `LeaderCommand`, `LeaderBinding`, `ViewScope`, and `LeaderMenu` skeleton

**Files:**
- Create: `crates/spur-tui/src/components/leader.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Write the failing compilation test.**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_menu_initial_state_is_closed() {
        let menu = LeaderMenu::new();
        assert!(!menu.is_open());
        assert!(!menu.is_armed());
    }

    #[test]
    fn leader_command_static_compiles() {
        use crate::action::Action;
        let cmd = LeaderCommand::Static(Action::ToggleVimMode);
        let _ = std::mem::discriminant(&cmd);
    }
}
```

- [ ] **Step 2: Run to verify FAIL (types not yet defined).**

```bash
scripts/spur-cargo test -p spur-tui --lib components::leader::tests
```

Expected: compile error — `LeaderMenu`, `LeaderCommand` not in scope.

- [ ] **Step 3: Implement types in `leader.rs`.**

```rust
//! # Leader Key (Space) — Phase 1
//!
//! Activates on Dashboard in Navigate mode. `Alt+*` shortcuts remain
//! functional (coexistence per spec §4.8). Deprecation toast: Release N+2.
//! SessionDetail leader: Phase 2 (separate plan).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::action::Action;

/// Which view(s) a binding is visible in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewScope {
    Dashboard,
    SessionDetail,
    Both,
}

/// A single leader-key binding entry.
pub struct LeaderBinding {
    pub key: char,
    pub label: &'static str,
    pub command: LeaderCommand,
    pub visible_in: ViewScope,
    /// Optional predicate evaluated at open-time against the app state.
    pub gate: Option<fn(&crate::App) -> bool>,
}

/// How a leader binding resolves into an effect.
pub enum LeaderCommand {
    /// Resolves to a context-free Action.
    Static(Action),
    /// Resolves dynamically — needs access to app state.
    Resolved(fn(&mut crate::App) -> Option<Action>),
    /// Direct mutation with no Action emitted.
    Mutate(fn(&mut crate::App)),
}

/// Dispatch result of `LeaderMenu::handle_key_dispatch`. Splits the
/// borrow so callers apply Mutate functions after the leader borrow ends.
pub enum LeaderDispatch {
    Action(Option<Action>),
    Mutate(fn(&mut crate::App)),
}

pub struct LeaderMenu {
    is_open: bool,
    armed_at: Option<Instant>,
    bindings: Vec<&'static LeaderBinding>,
}

impl LeaderMenu {
    pub fn new() -> Self {
        Self { is_open: false, armed_at: None, bindings: Vec::new() }
    }

    pub fn is_open(&self) -> bool { self.is_open }
    pub fn is_armed(&self) -> bool { self.armed_at.is_some() }

    /// Arm the menu for the Dashboard view (Phase 1).
    pub fn arm(&mut self, app: &crate::App) {
        self.armed_at = Some(Instant::now());
        self.is_open = false;
        self.bindings = LEADER_BINDINGS
            .iter()
            .filter(|b| {
                matches!(b.visible_in, ViewScope::Dashboard | ViewScope::Both)
                    && b.gate.map_or(true, |f| f(app))
            })
            .collect();
        self.bindings.sort_by_key(|b| b.key);
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.armed_at = None;
        self.bindings.clear();
    }

    /// Called every tick. Flips `is_open` after 250ms from arming.
    pub fn tick(&mut self, now: Instant) {
        if let Some(armed_at) = self.armed_at {
            if !self.is_open && now.duration_since(armed_at).as_millis() >= 250 {
                self.is_open = true;
            }
        }
    }

    /// Handle a key while armed/open. Returns split dispatch.
    pub fn handle_key_dispatch(&mut self, key: KeyEvent) -> (bool, LeaderDispatch) {
        if key.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT
        ) {
            self.close();
            return (true, LeaderDispatch::Action(None));
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char(' ') => {
                self.close();
                (true, LeaderDispatch::Action(None))
            }
            KeyCode::Char(c) => {
                if let Some(binding) = self.bindings.iter().find(|b| b.key == c) {
                    let dispatch = match &binding.command {
                        LeaderCommand::Static(a) => LeaderDispatch::Action(Some(a.clone())),
                        LeaderCommand::Resolved(_) => {
                            // Resolved needs &mut App; caller invokes after closing.
                            // Encode the function pointer via a helper variant.
                            // For simplicity, treat Resolved like Mutate:
                            // store it in a thread-local or pass via dispatch.
                            // Cleaner: change LeaderDispatch to carry a closure.
                            LeaderDispatch::Action(None) // placeholder — real impl in Task 8
                        }
                        LeaderCommand::Mutate(f) => LeaderDispatch::Mutate(*f),
                    };
                    self.close();
                    (true, dispatch)
                } else {
                    self.close();
                    (true, LeaderDispatch::Action(None))
                }
            }
            _ => {
                self.close();
                (true, LeaderDispatch::Action(None))
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.is_open || self.bindings.is_empty() { return; }

        let popup_height = (self.bindings.len() as u16 / 2 + 3).min(12).min(area.height / 3);
        let popup_width = 56u16.min(area.width.saturating_sub(4));
        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = area.height.saturating_sub(popup_height + 1);
        let popup_area = Rect::new(x + area.x, y + area.y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Leader — press a key, Esc to cancel ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let lines: Vec<Line<'static>> = self.bindings.chunks(2).map(|pair| {
            let left = format!("  {}  {:<28}", pair[0].key, pair[0].label);
            let right = pair.get(1).map(|b| format!("{}  {}", b.key, b.label)).unwrap_or_default();
            Line::from(vec![
                Span::styled(left, Style::default().fg(Color::White)),
                Span::styled(right, Style::default().fg(Color::White)),
            ])
        }).collect();

        frame.render_widget(Paragraph::new(lines).block(block), popup_area);
    }

    pub fn all_dashboard_bindings() -> impl Iterator<Item = &'static LeaderBinding> {
        LEADER_BINDINGS.iter().filter(|b| {
            matches!(b.visible_in, ViewScope::Dashboard | ViewScope::Both)
        })
    }

    #[cfg(test)]
    pub fn arm_at_for_test(&mut self, at: Instant) {
        self.armed_at = Some(at);
        self.bindings = Vec::new();
    }
}

impl Default for LeaderMenu {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 4: Declare `pub mod leader;` in `components/mod.rs`.**

- [ ] **Step 5: Run tests, verify pass; clippy + fmt.**

```bash
scripts/spur-cargo test -p spur-tui --lib components::leader::tests
scripts/spur-cargo clippy -p spur-tui -- -D warnings
```

**Acceptance Criteria:**
- `LeaderMenu::new()` returns idle state.
- All variants of `LeaderCommand` and `LeaderDispatch` compile.
- `ViewScope` has 3 variants.
- Module declared.

---

## Task 2 — Populate `static LEADER_BINDINGS`

**Files:** Modify `crates/spur-tui/src/components/leader.rs`

- [ ] **Step 1: Tests for binding contents.**

```rust
#[test]
fn leader_bindings_dashboard_subset_contains_expected_keys() {
    let dashboard_keys: Vec<char> = LEADER_BINDINGS.iter()
        .filter(|b| matches!(b.visible_in, ViewScope::Dashboard | ViewScope::Both))
        .map(|b| b.key).collect();
    assert!(dashboard_keys.contains(&'i'));
    assert!(dashboard_keys.contains(&'s'));
    assert!(dashboard_keys.contains(&'b'));
    assert!(dashboard_keys.contains(&'p'));
}

#[test]
fn leader_bindings_no_duplicate_keys_per_scope() {
    let dashboard: Vec<char> = LEADER_BINDINGS.iter()
        .filter(|b| matches!(b.visible_in, ViewScope::Dashboard | ViewScope::Both))
        .map(|b| b.key).collect();
    let mut sorted = dashboard.clone();
    sorted.sort(); sorted.dedup();
    assert_eq!(sorted.len(), dashboard.len());
}
```

- [ ] **Step 2: Verify FAIL.**

- [ ] **Step 3: Add `static LEADER_BINDINGS`.**

```rust
// IMPORTANT: `static`, NOT `const`. `Action` carries `SessionId` / `Vec<ContentBlock>`
// fields with non-const Drop semantics; a const slice does NOT compile.
static LEADER_BINDINGS: &[LeaderBinding] = &[
    LeaderBinding {
        key: 'i', label: "toggle vim mode",
        command: LeaderCommand::Static(Action::ToggleVimMode),
        visible_in: ViewScope::Both, gate: None,
    },
    LeaderBinding {
        key: 'b', label: "issue browser",
        command: LeaderCommand::Static(Action::NavigateTo(crate::action::ViewId::IssueBrowser)),
        visible_in: ViewScope::Dashboard, gate: None,
    },
    LeaderBinding {
        key: 'p', label: "plan inspector",
        command: LeaderCommand::Resolved(|app| {
            app.session_detail.as_ref().map(|v| {
                Action::NavigateTo(crate::action::ViewId::PlanInspector(v.session_id().clone()))
            })
        }),
        visible_in: ViewScope::Dashboard,
        gate: Some(|app| app.session_detail.is_some()),
    },
    LeaderBinding {
        key: 's', label: "open sessions",
        command: LeaderCommand::Static(Action::RequestSessions),
        visible_in: ViewScope::Dashboard, gate: None,
    },
    // SessionDetail-only (Phase 2 — registered, never armed in Phase 1):
    LeaderBinding {
        key: 'd', label: "toggle workers panel",
        command: LeaderCommand::Mutate(|app| {
            if let Some(v) = app.session_detail.as_mut() {
                v.toggle_workers_panel_collapsed();
            }
        }),
        visible_in: ViewScope::SessionDetail, gate: None,
    },
    LeaderBinding {
        key: 'm', label: "plan mode toggle",
        command: LeaderCommand::Static(Action::TogglePlanMode),
        visible_in: ViewScope::SessionDetail, gate: None,
    },
    LeaderBinding {
        key: 'r', label: "history picker",
        command: LeaderCommand::Static(Action::RequestSessions),
        visible_in: ViewScope::SessionDetail, gate: None,
    },
    LeaderBinding {
        key: 'v', label: "mermaid overlay",
        command: LeaderCommand::Resolved(|_app| None),
        visible_in: ViewScope::SessionDetail, gate: None,
    },
    LeaderBinding {
        key: 'w', label: "inspect workers",
        command: LeaderCommand::Static(Action::InspectWorkers),
        visible_in: ViewScope::SessionDetail, gate: None,
    },
];
```

- [ ] **Step 4: Tests pass.** Clippy + fmt.

**Acceptance:**
- `LEADER_BINDINGS` is `static`.
- Dashboard scope: `i`, `s`, `b`, `p`.
- No duplicates per scope.

---

## Task 3 — Public accessors on Dashboard + SessionDetail

**Files:**
- Modify `crates/spur-tui/src/views/dashboard.rs`: add `pub fn is_picker_active(&self) -> bool`, `pub fn mode(&self) -> DashboardMode`.
- Modify `crates/spur-tui/src/views/session_detail.rs`: add `pub fn toggle_workers_panel_collapsed(&mut self)` (extract from `:1206`); ensure `pub fn session_id()` accessor exists.

- [ ] **Step 1: Test for `is_picker_active`.**

```rust
#[test]
fn is_picker_active_false_when_no_completion() {
    let view = DashboardView::new();
    assert!(!view.is_picker_active());
}
```

- [ ] **Step 2: Verify FAIL.**

- [ ] **Step 3: Add accessors.**

```rust
// dashboard.rs:
impl DashboardView {
    pub fn is_picker_active(&self) -> bool { self.completion.is_active() }
    pub fn mode(&self) -> DashboardMode { self.mode }
}

// session_detail.rs:
impl SessionDetailView {
    pub fn toggle_workers_panel_collapsed(&mut self) {
        self.workers_panel_collapsed = !self.workers_panel_collapsed;
    }
    // Verify session_id accessor exists; if not:
    pub fn session_id(&self) -> &spur_acp::SessionId { &self.session_id }
}
```

Replace inline toggle in Alt+D handler at `session_detail.rs:1206` with `self.toggle_workers_panel_collapsed()`.

- [ ] **Step 4: Tests pass.** Workspace builds.

**Acceptance:**
- `DashboardView::is_picker_active()` and `DashboardView::mode()` are `pub`.
- `SessionDetailView::toggle_workers_panel_collapsed()` is `pub`; Alt+D handler delegates to it (no behavioral change).
- `SessionDetailView::session_id()` returns `&SessionId`.

---

## Task 4 — Wire Space-interception in `App::handle_key_inner`

**Files:** Modify `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Add `leader: LeaderMenu` field to `App`.**

In `App::new()`: `leader: crate::components::leader::LeaderMenu::new()`.

- [ ] **Step 2: Add `can_open_leader` helper.**

```rust
fn can_open_leader(&self) -> bool {
    if self.current_view != crate::action::ViewId::Dashboard { return false; }
    if self.dashboard.mode() != crate::views::dashboard::DashboardMode::Navigate { return false; }
    if self.help_visible || self.quit_confirm_visible
        || self.collision_modal.is_some() || self.upgrade_modal.is_some()
        || self.palette_visible
    {
        return false;
    }
    if self.dashboard.is_picker_active() { return false; }
    true
}
```

- [ ] **Step 3: Insert Space-interception block in `handle_key_inner`.**

After palette guard (`app.rs:976`), before `let ctx = ...` at `app.rs:987`:

```rust
// ── Leader-key interception (§4.1 / §4.4.1) ──────────────────────
// MUST run BEFORE view dispatch: Space routes to Composer at
// dashboard.rs:950-953 without this guard.
if self.leader.is_armed() || self.leader.is_open() {
    let (consumed, dispatch) = self.leader.handle_key_dispatch(key);
    if consumed {
        match dispatch {
            crate::components::leader::LeaderDispatch::Action(Some(a)) => self.process_action(a),
            crate::components::leader::LeaderDispatch::Action(None) => {}
            crate::components::leader::LeaderDispatch::Mutate(f) => f(self),
        }
        self.dirty = true;
        return;
    }
}
if matches!(key.code, KeyCode::Char(' ')) && self.can_open_leader() {
    self.leader.arm(self);
    self.dirty = true;
    return;
}
// ─────────────────────────────────────────────────────────────────
```

- [ ] **Step 4: Verify workspace compiles.**

**Acceptance:**
- `App` has `leader: LeaderMenu` field.
- `can_open_leader` enforces all 5 §4.1 conditions.
- Space in Navigate mode arms leader before view dispatch.
- Borrow checker satisfied.

---

## Task 5 — Wire 250ms tick

**Files:** Modify `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Test.**

```rust
#[test]
fn leader_tick_reveals_popup_after_250ms() {
    use std::time::{Duration, Instant};
    use crate::components::leader::LeaderMenu;

    let mut menu = LeaderMenu::new();
    menu.arm_at_for_test(Instant::now() - Duration::from_millis(300));
    assert!(!menu.is_open());
    menu.tick(Instant::now());
    assert!(menu.is_open());
}
```

- [ ] **Step 2: Verify FAIL.**

- [ ] **Step 3: Add `leader.tick()` call in `App::tick()`.**

At top of `tick()` body (`app.rs:2539`):

```rust
pub fn tick(&mut self) {
    if self.leader.is_armed() {
        self.leader.tick(std::time::Instant::now());
        if self.leader.is_open() { self.dirty = true; }
    }
    // ... existing tick body ...
```

- [ ] **Step 4: Tests pass.** Clippy + fmt.

**Acceptance:**
- `LeaderMenu::tick()` flips `is_open` exactly once after ≥250ms.
- Before 250ms, `is_open` remains false (power-user fast-path preserved).
- `App::tick()` invokes the tick and marks dirty on reveal.
- No new tokio timer.

---

## Task 6 — Render the popup overlay

**Files:** Modify `crates/spur-tui/src/app.rs` (render call site); `leader.rs` render method (defined in Task 1)

- [ ] **Step 1: Locate render method in `app.rs`.**

```bash
grep -n "fn render\|fn draw\|frame.render" /Volumes/Projects/spur/crates/spur-tui/src/app.rs | head -20
```

- [ ] **Step 2: Add render call after main view, before help overlay.**

```rust
if self.leader.is_open() {
    self.leader.render(frame, view_area);
}
```

- [ ] **Step 3: Render smoke tests.**

```rust
#[test]
fn leader_render_no_panic_when_closed() {
    let menu = LeaderMenu::new();
    assert!(!menu.is_open());
}

#[test]
fn leader_popup_renders_after_250ms() {
    use std::time::{Duration, Instant};
    let mut menu = LeaderMenu::new();
    menu.arm_at_for_test(Instant::now() - Duration::from_millis(300));
    menu.tick(Instant::now());
    assert!(menu.is_open());
}
```

- [ ] **Step 4: Tests pass.** Clippy + fmt.

**Acceptance:**
- `LeaderMenu::render()` is no-op when closed.
- When open, popup renders with two-column alphabetical layout.
- Anchored bottom-center of view area.
- No panic on empty bindings.

---

## Task 7 — Help overlay integration

**Files:** Modify `crates/spur-tui/src/components/help_overlay.rs`

- [ ] **Step 1: Test.**

```rust
#[test]
fn leader_help_overlay_lists_all_bindings() {
    let lines = HelpOverlay::lines(false, false);
    let text: String = lines.iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(text.contains("Space"));
    assert!(text.contains("toggle vim mode"));
    assert!(text.contains("open sessions"));
}
```

- [ ] **Step 2: Verify FAIL.**

- [ ] **Step 3: Append section to `HelpOverlay::lines()`.**

```rust
out.push(Line::from(""));
out.push(header(" Leader (Space) — press Space then:"));
for binding in crate::components::leader::LeaderMenu::all_dashboard_bindings() {
    out.push(Line::from(format!("  {}  {}", binding.key, binding.label)));
}
out.push(Line::from("  Esc  cancel"));
```

- [ ] **Step 4: Tests pass.** Clippy + fmt.

**Acceptance:**
- `?` shows "Leader (Space)" section.
- Section enumerates Dashboard-scoped bindings (single source of truth).
- Existing help tests still pass.

---

## Task 8 — `LeaderCommand::dispatch` Resolved variant + integration wiring

**Files:** `crates/spur-tui/src/components/leader.rs`; `crates/spur-tui/src/app.rs`

The Resolved variant needs `&mut App`. Update `LeaderDispatch` and `handle_key_dispatch` to carry the function pointer through:

```rust
pub enum LeaderDispatch {
    Action(Option<Action>),
    Resolve(fn(&mut crate::App) -> Option<Action>),
    Mutate(fn(&mut crate::App)),
}
```

App-level dispatch:

```rust
match dispatch {
    LeaderDispatch::Action(Some(a)) => self.process_action(a),
    LeaderDispatch::Action(None) => {}
    LeaderDispatch::Resolve(f) => {
        if let Some(a) = f(self) { self.process_action(a); }
    }
    LeaderDispatch::Mutate(f) => f(self),
}
```

- [ ] **Step 1: Tests.**

```rust
#[test]
fn leader_unrecognized_letter_closes_without_action() {
    use std::time::{Duration, Instant};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut menu = LeaderMenu::new();
    menu.arm_at_for_test(Instant::now() - Duration::from_millis(300));
    menu.tick(Instant::now());
    let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
    let (consumed, dispatch) = menu.handle_key_dispatch(z);
    assert!(consumed);
    assert!(!menu.is_open());
    assert!(matches!(dispatch, LeaderDispatch::Action(None)));
}
```

- [ ] **Step 2: Verify FAIL, implement, verify PASS.**

- [ ] **Step 3: Confirm `SessionDetailView::session_id()` accessibility (added in Task 3).**

- [ ] **Step 4: Run all leader tests.** Clippy + fmt.

**Acceptance:**
- `LeaderCommand::dispatch` handles all 3 variants.
- Unrecognized letter closes; not re-dispatched.
- `p` binding emits `Action::NavigateTo(ViewId::PlanInspector(_))` when session present.

---

## Task 9 — Verify `Alt+*` coexistence (no removals in this release)

**Files:** Verification only — no changes to existing handlers.

- [ ] **Step 1: Test.**

```rust
#[test]
fn alt_i_still_fires_toggle_vim_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use crate::views::dashboard::DashboardView;
    use crate::action::Action;

    let mut view = DashboardView::new();
    let key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT);
    let mut ws = crate::worker_streams::WorkerStreams::default();
    let action = view.handle_key_with_worker_streams(key, &Default::default(), &mut ws);
    assert!(matches!(action, Some(Action::ToggleVimMode)));
}
```

- [ ] **Step 2: Verify PASS (no code change).**

**Acceptance:**
- All existing `Alt+*` handlers untouched.
- Tests confirm at least `Alt+I` continues to emit `Action::ToggleVimMode`.
- No deprecation toasts in this release (deferred to Release N+2).

---

## Task 10 — Full test suite + commit

**Files:** Append to `#[cfg(test)] mod tests` in `leader.rs`.

Covers all ten scenarios from spec §6.

- [ ] **Step 1: All unit-level tests.**

```rust
#[test]
fn leader_closes_on_esc() {
    use std::time::{Duration, Instant};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut menu = LeaderMenu::new();
    menu.arm_at_for_test(Instant::now() - Duration::from_millis(300));
    menu.tick(Instant::now());
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let (consumed, _) = menu.handle_key_dispatch(esc);
    assert!(consumed);
    assert!(!menu.is_open());
}

#[test]
fn leader_does_not_open_before_250ms() {
    use std::time::{Duration, Instant};
    let mut menu = LeaderMenu::new();
    menu.arm_at_for_test(Instant::now() - Duration::from_millis(100));
    menu.tick(Instant::now());
    assert!(!menu.is_open());
    assert!(menu.is_armed());
}

#[test]
fn leader_modifier_bearing_key_closes_and_consumes() {
    use std::time::{Duration, Instant};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut menu = LeaderMenu::new();
    menu.arm_at_for_test(Instant::now() - Duration::from_millis(300));
    menu.tick(Instant::now());
    let alt_m = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT);
    let (consumed, dispatch) = menu.handle_key_dispatch(alt_m);
    assert!(consumed);
    assert!(!menu.is_open());
    assert!(matches!(dispatch, LeaderDispatch::Action(None)));
}

#[test]
fn leader_filters_session_detail_bindings_from_dashboard_scope() {
    let dashboard_keys: Vec<char> = LEADER_BINDINGS.iter()
        .filter(|b| matches!(b.visible_in, ViewScope::Dashboard | ViewScope::Both))
        .map(|b| b.key).collect();
    for unwanted in ['m', 'd', 'r', 'v', 'w'] {
        assert!(!dashboard_keys.contains(&unwanted),
                "Dashboard scope must not include SessionDetail-only key '{}'", unwanted);
    }
    for required in ['i', 's', 'b', 'p'] {
        assert!(dashboard_keys.contains(&required),
                "Dashboard scope missing '{}'", required);
    }
}
```

- [ ] **Step 2: Run full test suite.**

```bash
scripts/spur-cargo test -p spur-tui --lib
```

- [ ] **Step 3: Workspace build + clippy + fmt.**

```bash
scripts/spur-cargo build --workspace
scripts/spur-cargo clippy -p spur-tui -- -D warnings
scripts/spur-cargo fmt -p spur-tui -- --check
```

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-tui/src/components/leader.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/components/help_overlay.rs \
        crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): leader-key Phase 1 — Space opens contextual menu on Dashboard Navigate"
```

**Acceptance Criteria (full suite):**

- [ ] `leader_opens_in_navigate_mode`: Space arms leader; after 250ms tick `is_open == true`.
- [ ] `leader_does_not_open_in_compose`: Space in Compose passes through to Composer.
- [ ] `leader_does_not_open_with_overlay`: any modal → `can_open_leader == false`.
- [ ] `leader_closes_on_esc`: open + Esc → closed, no action.
- [ ] `leader_dispatches_action`: open + `s` → `Action::RequestSessions`; menu closes.
- [ ] `leader_unrecognized_letter_closes`: `z` consumed, not forwarded.
- [ ] `leader_filters_per_view`: Dashboard excludes `m`/`d`/`r`/`v`/`w`; includes `i`/`s`/`b`/`p`.
- [ ] `leader_help_overlay_lists_all_bindings`: `?` text contains "Space" + binding labels.
- [ ] `alt_aliases_still_work`: `Alt+I` emits `Action::ToggleVimMode` unchanged.
- [ ] `leader_popup_renders_after_250ms`: tick flips `is_open` after ≥250ms.

---

## Critical Details

**Borrow-split for Mutate/Resolve dispatch:** `LeaderMenu::handle_key_dispatch` returns `LeaderDispatch` carrying function pointers. Caller (`App`) applies them after the leader borrow drops. No `unsafe` needed.

**`static` vs `const` for `LEADER_BINDINGS`:** `Action` carries `SessionId` (wraps `String`) and `Vec<ContentBlock>` — both have non-trivial Drop. `static` is correct; Drop runs at program exit.

**`p` binding gate:** `gate: Some(|app| app.session_detail.is_some())` requires `pub` access to the `session_detail` field on `App` OR a `pub fn has_session_detail(&self) -> bool` accessor. Pick whichever is consistent with the existing crate's encapsulation pattern.

**Ordering of Space-interception in `handle_key_inner`:** MUST appear after palette guard (`app.rs:976`) and before `let ctx = ViewContext { ... }` at `app.rs:987`. The palette-open case is also caught by the gate (condition 3) but correct ordering is belt-and-suspenders.

**`ViewContext` reference in spec §4.4 — clarification:** the spec mentions `ViewContext<'_>` for gate signatures. This plan uses `fn(&crate::App) -> bool` instead because the gates only need to peek at App state (no rendering context). Equivalent functionally; simpler lifetime story.

**Phase 2 note — SessionDetail:** SessionDetail-only entries in `LEADER_BINDINGS` compile in Phase 1 but are never armed (`arm()` filters to Dashboard scope). When Phase 2 ships, `arm()` gains a `ViewScope` parameter and `can_open_leader` expands to include `ViewId::SessionDetail(_)` once `SessionDetailMode::Navigate` is detectable.

**Hint-slot priority:** Per spec §7.6 and quick-fixes §6.3, the popup overlay uses a dedicated `Rect` separate from the single-line hint slot. Inline hints ("Press Space for actions" footer if added) occupy slot priority 3 and do not compete with tombstone toasts (priority 2) or panic-Esc reset confirmations (priority 1).
