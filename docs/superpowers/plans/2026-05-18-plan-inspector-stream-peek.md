# Plan Inspector — Stream Peek & Dashboard Jump Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In `plan_inspector`, let the user press `s` to open a transient overlay rendering the selected task's worker stream (own scroll/follow state, not shared with Dashboard), and press `S` to jump to Dashboard focused on that worker with the Stream tab selected.

**Architecture:** Add a `Mode { Browse, StreamPeek }` enum to `PlanInspectorView` with its own `StreamPeekState`. Extract the Stream-tab rendering pipeline from `DetailPane` into a free function `render_stream(...)` parameterised over a `StreamViewState` so both `DetailPane` and the peek use the same renderer with independent state. Add `Action::FocusWorkerInDashboard { executor_id, tab }` and route it in `app/action_routing.rs` to call Dashboard's existing focus + `jump_to_tab` APIs. Add a side-door `render_with_worker_streams` / `handle_key_with_worker_streams` on `PlanInspectorView` mirroring the Dashboard pattern.

**Tech Stack:** Rust, ratatui, crossterm. Tests are unit tests in-crate (`cargo test -p spur-tui`). The crate uses standard `#[cfg(test)] mod tests` patterns. Run a single test with `cargo test -p spur-tui <name> -- --nocapture`.

**Spec reference:** `docs/superpowers/specs/2026-05-18-plan-inspector-stream-peek-design.md`.

---

## File Structure

**Modified:**
- `crates/spur-tui/src/components/detail_pane.rs` — extract Stream-tab branch into a free function operating on a `StreamViewState`. Keep `DetailPane`'s public API unchanged.
- `crates/spur-tui/src/views/plan_inspector.rs` — add `Mode`, `StreamPeekState`, peek render, peek key dispatch, side-door methods, auto-close lifecycle.
- `crates/spur-tui/src/action.rs` — add `Action::FocusWorkerInDashboard`.
- `crates/spur-tui/src/app/action_routing.rs` — handle the new action.
- `crates/spur-tui/src/app/mod.rs` — render call site for `PlanInspector` switches to `render_with_worker_streams`.
- `crates/spur-tui/src/app/input.rs` — key-event call site for `PlanInspector` switches to `handle_key_with_worker_streams`.
- `crates/spur-core/src/lineage/projection.rs` — expose `executor_id_for_delegation(&self, delegation_id: &DelegationId) -> Option<ExecutorId>` (currently private as `find_node_by_delegation`).

**Created:**
- `crates/spur-tui/src/components/stream_pane.rs` — the extracted reusable stream renderer (`StreamViewState`, `render_stream(...)`). Owned by Task 1.

---

## Conventions for every task

- TDD: write failing test first, run to confirm fail, implement, run to confirm pass, commit. Each commit is one task.
- Use `cargo check -p spur-tui` to catch type errors quickly between steps.
- Never use `--no-verify` to skip hooks. If a hook fails, fix the underlying issue.
- Commit messages: `feat(spur-tui): <task summary>` or `refactor(spur-tui): <task summary>`.

---

## Task 1: Extract Stream rendering into a reusable function

**Files:**
- Create: `crates/spur-tui/src/components/stream_pane.rs`
- Modify: `crates/spur-tui/src/components/mod.rs` (add `pub mod stream_pane;`)
- Modify: `crates/spur-tui/src/components/detail_pane.rs` (delegate Stream branch)

**Rationale:** The spec mandates "reuse the existing Stream rendering pipeline." Today, the Stream-tab body is rendered inline inside `DetailPane::render` (lines 226–320 of `detail_pane.rs`). Extracting it into a free function on a `StreamViewState` lets both `DetailPane` (which holds its own state) and the peek (which will hold a separate, transient state) share the same renderer.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/src/components/stream_pane.rs` with the public surface and a unit test:

```rust
use std::borrow::Cow;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::components::react_trace::ReactTrace;

/// Scroll + follow state for a Stream view. Owned independently by every
/// caller of [`render_stream`] so two stream views can scroll without
/// interfering with each other.
#[derive(Debug, Clone)]
pub struct StreamViewState {
    pub scroll_offset: usize,
    pub is_following: bool,
}

impl Default for StreamViewState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            is_following: true,
        }
    }
}

impl StreamViewState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scroll_up_by(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.is_following = false;
    }

    pub fn scroll_down_by(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.is_following = true;
    }

    pub fn toggle_follow(&mut self) {
        self.is_following = !self.is_following;
    }
}

/// Pure label helper, identical to `detail_pane::scroll_label` for the
/// Stream tab. Kept here to make the renderer self-contained.
pub(crate) fn stream_scroll_label(
    total: usize,
    visible_h: usize,
    scroll_offset: usize,
    is_following: bool,
) -> Cow<'static, str> {
    if total == 0 {
        return Cow::Borrowed("");
    }
    let max_offset = total.saturating_sub(visible_h);
    if max_offset == 0 {
        return Cow::Borrowed(" ▼ ");
    }
    if is_following {
        return Cow::Borrowed(" ▼ following ");
    }
    if scroll_offset == 0 {
        return Cow::Borrowed(" top ");
    }
    if scroll_offset >= max_offset {
        return Cow::Borrowed(" end ");
    }
    Cow::Owned(format!(" ▲ {} ↑ ", scroll_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_up_disengages_follow() {
        let mut s = StreamViewState::new();
        assert!(s.is_following);
        s.scroll_up_by(1);
        assert!(!s.is_following);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn scroll_to_bottom_reengages_follow() {
        let mut s = StreamViewState { scroll_offset: 42, is_following: false };
        s.scroll_to_bottom();
        assert!(s.is_following);
    }

    #[test]
    fn label_following_shows_following_marker() {
        let l = stream_scroll_label(100, 20, 80, true);
        assert_eq!(l, Cow::Borrowed(" ▼ following "));
    }

    #[test]
    fn label_empty_total_is_blank() {
        let l = stream_scroll_label(0, 20, 0, false);
        assert_eq!(l, Cow::Borrowed(""));
    }
}
```

Add `pub mod stream_pane;` to `crates/spur-tui/src/components/mod.rs` (alphabetically among the existing `pub mod` lines).

- [ ] **Step 2: Run tests to confirm they pass (compile-only barrier first)**

```bash
cargo test -p spur-tui --no-run
cargo test -p spur-tui stream_pane::tests
```

Expected: 4 tests pass.

- [ ] **Step 3: Add `render_stream` function**

Append to `crates/spur-tui/src/components/stream_pane.rs`:

```rust
/// Render a Stream pane into `area`. The caller owns `state`; this function
/// mutates `state.scroll_offset` / `state.is_following` to apply the same
/// clamp + re-engage-following invariants used by `DetailPane`.
///
/// `title_left`: left side of the top border (e.g. `" codex "`).
/// `title_right`: right side of the top border, e.g. badge or attempt label.
/// `bottom_right_hint`: optional right-side bottom-border hint (e.g.
///     `" [esc] close "` for the peek; `None` for `DetailPane`).
///
/// Returns the wrapped total rows + visible viewport height so callers
/// can compose additional title fields (position indicator) on the block.
pub struct StreamRenderInfo {
    pub total_rows: usize,
    pub visible_height: usize,
}

pub fn render_stream(
    frame: &mut Frame,
    area: Rect,
    title_left: &str,
    title_right: Option<&str>,
    bottom_right_hint: Option<&str>,
    trace: Option<&mut ReactTrace>,
    state: &mut StreamViewState,
) -> StreamRenderInfo {
    // 1. Skeleton (shape-equivalent to final block for inner() stability).
    let mut skeleton = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(" ")
        .title_bottom(" ");
    if title_right.is_some() {
        skeleton = skeleton.title_top(Line::from(" ").alignment(Alignment::Right));
    }
    if bottom_right_hint.is_some() {
        skeleton = skeleton.title_bottom(Line::from(" ").alignment(Alignment::Right));
    }
    let inner = skeleton.inner(area);
    let chunks = Layout::vertical([Constraint::Min(1)]).split(inner);
    let body_area = chunks[0];
    let visible_h = body_area.height as usize;

    // 2. Body lines.
    let body_lines: Vec<Line<'static>> = match trace {
        Some(t) => t.build_body_lines(body_area.width),
        None => vec![Line::from(Span::styled(
            "(no stream yet)",
            Style::default().fg(Color::DarkGray),
        ))],
    };
    let wrapped: Vec<Line<'static>> = body_lines
        .iter()
        .flat_map(|l| crate::components::line_wrap::wrap_line_to_width(l, body_area.width))
        .collect();
    let total = wrapped.len();

    // 3. Clamp + re-engage-following.
    let max_offset = total.saturating_sub(visible_h);
    if state.is_following {
        state.scroll_offset = max_offset;
    } else {
        state.scroll_offset = state.scroll_offset.min(max_offset);
        if state.scroll_offset >= max_offset && max_offset > 0 {
            state.is_following = true;
        }
    }

    // 4. Real block.
    let label = stream_scroll_label(total, visible_h, state.scroll_offset, state.is_following);
    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(format!(" {} ", title_left))
        .title_bottom(label.as_ref().to_string());
    if let Some(tr) = title_right {
        block = block.title_top(Line::from(format!(" {} ", tr)).alignment(Alignment::Right));
    }
    if let Some(br) = bottom_right_hint {
        block = block.title_bottom(Line::from(format!(" {} ", br)).alignment(Alignment::Right));
    }
    let _ = Modifier::BOLD; // silence import in case label uses no modifier
    frame.render_widget(block, area);

    // 5. Body paragraph.
    let p = Paragraph::new(wrapped.clone()).scroll((state.scroll_offset as u16, 0));
    frame.render_widget(p, body_area);

    StreamRenderInfo {
        total_rows: total,
        visible_height: visible_h,
    }
}
```

Note: this mirrors the existing pipeline in `detail_pane.rs` lines 198–320. The `Tabs` strip is intentionally NOT part of this function — that remains a `DetailPane` concern.

- [ ] **Step 4: Verify it compiles**

```bash
cargo check -p spur-tui
```

Expected: clean compile (warnings about unused imports inside stream_pane.rs are OK at this step; they'll be exercised in Task 2 onwards).

- [ ] **Step 5: Delegate `DetailPane::Stream` branch through `render_stream`**

In `crates/spur-tui/src/components/detail_pane.rs`, replace the `DetailTab::Stream => { ... }` arm in `render()` (around lines 229–238) with a delegation that constructs a temporary `StreamViewState` from `self.scroll_offset` and `self.is_following`, calls `render_stream`, then writes the mutated state back. This preserves all current behavior — `DetailPane` still owns its own state, the function is just where the work happens now.

Concretely: change the `render` method body so that **on the Stream tab**, instead of computing `body_lines` and going through steps 4–7 inline, it calls `render_stream` with `title_left = &node.agent`, `title_right = issue_badge`, `bottom_right_hint = if issue_badge.is_some() { Some("[I]ssue detail") } else { None }`, and a `&mut StreamViewState` synced from `self`. After the call, copy `state.scroll_offset` and `state.is_following` back to `self`.

For the non-Stream tabs, keep the existing inline rendering (it depends on `DetailPane`'s private `render_*` methods that take `&ExecutorNode`).

Sketch (only the relevant `match` arm changes):

```rust
match self.current_tab {
    DetailTab::Stream => {
        let mut state = crate::components::stream_pane::StreamViewState {
            scroll_offset: self.scroll_offset,
            is_following: self.is_following,
        };
        let bottom_right = if issue_badge.is_some() { Some("[I]ssue detail") } else { None };
        // Render tabs first so the body_area inside render_stream lines up
        // with the existing layout: do the same Layout split here, render
        // the Tabs into chunks[0], pass chunks[1] as area to render_stream.
        // ... (existing tabs rendering preserved) ...
        crate::components::stream_pane::render_stream(
            frame,
            tabs_and_body_area_for_stream, // computed identically to today
            &node.agent,
            issue_badge,
            bottom_right,
            stream_trace,
            &mut state,
        );
        self.scroll_offset = state.scroll_offset;
        self.is_following = state.is_following;
    }
    DetailTab::Artifacts | DetailTab::Attempts | DetailTab::Task | DetailTab::Review => {
        // existing inline body rendering preserved
    }
}
```

**Important:** the existing `DetailPane::render` always renders both the Tabs strip and the body block from one block. The cleanest minimal refactor is to keep the block/tabs construction in `DetailPane::render` and only delegate the **body-line computation + clamp + paragraph render** to `render_stream`. If that turns out cleaner than the sketch above, do it that way and update this task's notes inline before committing.

Existing scroll-label tests in `detail_pane.rs` (`scroll_label_tests`) must still pass unchanged.

- [ ] **Step 6: Run the full test suite**

```bash
cargo test -p spur-tui
```

Expected: all existing tests pass (including the `scroll_label_tests` and `jump_to_tab_tests` modules in `detail_pane.rs`, and the new `stream_pane::tests`).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/stream_pane.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/src/components/detail_pane.rs
git commit -m "refactor(spur-tui): extract Stream pane renderer into stream_pane.rs"
```

---

## Task 2: Expose `executor_id_for_delegation` on `ExecutorLineage`

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`

**Rationale:** Plan_inspector resolves a `TrackedTask` (which carries `delegation_id: Option<String>`) to an executor_id so it can index into `WorkerStreams`. The internal helper `find_node_by_delegation` exists but is private. Expose a thin public accessor returning `Option<ExecutorId>`.

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-core/src/lineage/projection.rs` (within the existing `#[cfg(test)] mod tests` if there is one, otherwise create one):

```rust
#[test]
fn executor_id_for_delegation_returns_node_id_when_present() {
    let mut lineage = ExecutorLineage::new();
    let eid = ExecutorId::new("worker-session-1".into());
    let did = DelegationId("delegation-abc".into());

    // Construct a node and tag it with the delegation_id.
    let mut node = ExecutorNode::default_for_test(eid.clone(), "codex".into());
    node.delegation_id = Some(did.clone());
    lineage.insert_node_for_test(node);

    assert_eq!(
        lineage.executor_id_for_delegation(&did),
        Some(eid),
    );
    assert_eq!(
        lineage.executor_id_for_delegation(&DelegationId("nope".into())),
        None,
    );
}
```

If `ExecutorNode::default_for_test` and `ExecutorLineage::insert_node_for_test` do not exist, use whatever public constructors the existing tests in this file already rely on — match their pattern. The test's purpose is to lock in the public accessor signature; adapt the setup to the test infrastructure already present.

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p spur-core executor_id_for_delegation_returns_node_id_when_present
```

Expected: FAIL with "no method named `executor_id_for_delegation`".

- [ ] **Step 3: Add the public method**

In `crates/spur-core/src/lineage/projection.rs`, immediately after the existing private `find_node_by_delegation` definition (around line 611):

```rust
/// Public accessor: resolve a delegation_id to the owning executor_id, if any.
/// Used by TUI plan_inspector to look up the worker's stream trace from a
/// `TrackedTask.delegation_id`.
pub fn executor_id_for_delegation(&self, delegation_id: &DelegationId) -> Option<ExecutorId> {
    self.find_node_by_delegation(delegation_id)
        .map(|node| node.id.clone())
}
```

If `ExecutorNode` does not expose `.id` publicly, add `pub fn id(&self) -> &ExecutorId` first, or use whatever the existing field name is — check the struct definition in the same crate before adapting.

- [ ] **Step 4: Run test to confirm it passes**

```bash
cargo test -p spur-core executor_id_for_delegation_returns_node_id_when_present
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs
git commit -m "feat(spur-core): expose executor_id_for_delegation on ExecutorLineage"
```

---

## Task 3: Add `Action::FocusWorkerInDashboard`

**Files:**
- Modify: `crates/spur-tui/src/action.rs`

**Rationale:** The `S` keybind in plan_inspector emits this action; `app/action_routing.rs` will translate it to "navigate to Dashboard + focus that executor in AgentsTree + jump_to_tab(Stream)" in Task 8.

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/spur-tui/src/action.rs`:

```rust
#[cfg(test)]
mod focus_worker_action_tests {
    use super::*;
    use crate::components::detail_pane::DetailTab;

    #[test]
    fn focus_worker_in_dashboard_action_constructs() {
        let a = Action::FocusWorkerInDashboard {
            executor_id: "worker-session-42".into(),
            tab: DetailTab::Stream,
        };
        match a {
            Action::FocusWorkerInDashboard { executor_id, tab } => {
                assert_eq!(executor_id, "worker-session-42");
                assert_eq!(tab, DetailTab::Stream);
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p spur-tui focus_worker_action_tests
```

Expected: FAIL with `no variant named FocusWorkerInDashboard`.

- [ ] **Step 3: Add the variant**

In `crates/spur-tui/src/action.rs`, add inside the `Action` enum (e.g., right after `InspectWorkers`):

```rust
/// Navigate to Dashboard, focus the executor with this id in `AgentsTree`,
/// and call `DetailPane::jump_to_tab(tab)`. Emitted by PlanInspector's `S`
/// key when the selected task has a resolved worker executor.
FocusWorkerInDashboard {
    executor_id: String,
    tab: crate::components::detail_pane::DetailTab,
},
```

`DetailTab` already derives `Debug, Clone, Copy, PartialEq, Eq` so the action's existing `#[derive(Debug, Clone)]` continues to apply.

- [ ] **Step 4: Run test to confirm it passes**

```bash
cargo test -p spur-tui focus_worker_action_tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/action.rs
git commit -m "feat(spur-tui): add Action::FocusWorkerInDashboard"
```

---

## Task 4: Add `Mode` + `StreamPeekState` to `PlanInspectorView`

**Files:**
- Modify: `crates/spur-tui/src/views/plan_inspector.rs`

**Rationale:** All peek state lives in the view. `Mode` makes key dispatch unambiguous; the `StreamViewState` from Task 1 holds scroll/follow independently from Dashboard.

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-tui/src/views/plan_inspector.rs` in the existing test module at the bottom:

```rust
#[test]
fn new_view_starts_in_browse_mode() {
    let v = PlanInspectorView::new(SessionId("s".into()));
    assert!(matches!(v.mode(), PlanInspectorMode::Browse));
}

#[test]
fn enter_stream_peek_sets_mode_and_initial_state() {
    let mut v = PlanInspectorView::new(SessionId("s".into()));
    v.enter_stream_peek("worker-session-1".into(), "t-12".into());
    match v.mode() {
        PlanInspectorMode::StreamPeek { executor_id, task_id, state } => {
            assert_eq!(executor_id, "worker-session-1");
            assert_eq!(task_id, "t-12");
            assert!(state.is_following);
            assert_eq!(state.scroll_offset, 0);
        }
        _ => panic!("expected StreamPeek"),
    }
}

#[test]
fn leave_stream_peek_returns_to_browse() {
    let mut v = PlanInspectorView::new(SessionId("s".into()));
    v.enter_stream_peek("w".into(), "t".into());
    v.leave_stream_peek();
    assert!(matches!(v.mode(), PlanInspectorMode::Browse));
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p spur-tui plan_inspector::tests::new_view_starts_in_browse_mode
```

Expected: FAIL with `no method named mode` / `no enum PlanInspectorMode`.

- [ ] **Step 3: Add the types and methods**

In `plan_inspector.rs`, near the top with the other types:

```rust
#[derive(Debug)]
pub enum PlanInspectorMode {
    Browse,
    StreamPeek {
        executor_id: String,
        task_id: String,
        state: crate::components::stream_pane::StreamViewState,
    },
}
```

Add a `mode: PlanInspectorMode` field to `PlanInspectorView`. Initialise it to `PlanInspectorMode::Browse` in both `new` and `new_for_plan`.

Add methods:

```rust
impl PlanInspectorView {
    pub fn mode(&self) -> &PlanInspectorMode {
        &self.mode
    }

    pub fn enter_stream_peek(&mut self, executor_id: String, task_id: String) {
        self.mode = PlanInspectorMode::StreamPeek {
            executor_id,
            task_id,
            state: crate::components::stream_pane::StreamViewState::new(),
        };
    }

    pub fn leave_stream_peek(&mut self) {
        self.mode = PlanInspectorMode::Browse;
    }

    fn peek_state_mut(&mut self) -> Option<&mut crate::components::stream_pane::StreamViewState> {
        if let PlanInspectorMode::StreamPeek { state, .. } = &mut self.mode {
            Some(state)
        } else {
            None
        }
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p spur-tui plan_inspector
```

Expected: PASS (all three new tests + all existing plan_inspector tests still pass).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/plan_inspector.rs
git commit -m "feat(spur-tui): add Mode + StreamPeekState to PlanInspectorView"
```

---

## Task 5: Add side-door methods `render_with_worker_streams` + `handle_key_with_worker_streams`

**Files:**
- Modify: `crates/spur-tui/src/views/plan_inspector.rs`

**Rationale:** `ViewContext` deliberately does not expose `WorkerStreams` (per `views/mod.rs` line 132 doc comment). Dashboard solves this with `render_with_lineage` + `handle_key_with_worker_streams`. Plan_inspector adopts the same pattern.

- [ ] **Step 1: Write the failing test**

Add to the plan_inspector test module (these tests do not require a real frame; they verify the methods exist with the expected signatures and that they delegate sensibly):

```rust
#[test]
fn render_with_worker_streams_does_not_panic_when_no_streams() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic(&session_id);
    let lineage = spur_core::lineage::projection::ExecutorLineage::new();
    let ctx_plan_projection = &projection;
    // Use a minimal in-memory backend; the existing test helpers should
    // already give us a `ViewContext`. Reuse them.
    let ctx = view_context_for_tests(&lineage, ctx_plan_projection);

    let mut ws = crate::worker_streams::WorkerStreams::new();
    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());

    // Build a 80x24 test backend frame.
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| {
        view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
    }).unwrap();
    // Nothing else to assert beyond "did not panic"; the rendering paths
    // are exercised in higher-level integration coverage.
}
```

If `view_context_for_tests` doesn't exist, use the existing test setup pattern visible in the same file's existing tests (e.g., the tests around lines 1003–1090) — they already construct a `ViewContext` via `ViewContext::test_ctx`. Copy that approach.

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p spur-tui render_with_worker_streams_does_not_panic_when_no_streams
```

Expected: FAIL with `no method named render_with_worker_streams`.

- [ ] **Step 3: Implement the side-door methods**

Add an `impl PlanInspectorView` block (separate from the `impl View for PlanInspectorView` block, mirroring Dashboard's pattern):

```rust
impl PlanInspectorView {
    pub fn render_with_worker_streams(
        &mut self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
        ctx: &super::ViewContext,
    ) {
        // 1. Render the existing plan_inspector content first.
        <Self as super::View>::render(self, frame, area, ctx);

        // 2. If in StreamPeek mode, render the overlay on top.
        if let PlanInspectorMode::StreamPeek { executor_id, task_id, state } = &mut self.mode {
            let trace = worker_streams.get_mut(executor_id);
            Self::render_peek_overlay(frame, area, executor_id, task_id, trace, state);
        }
    }

    pub fn handle_key_with_worker_streams(
        &mut self,
        key: crossterm::event::KeyEvent,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
        ctx: &super::ViewContext,
    ) -> Option<crate::action::Action> {
        let _ = worker_streams; // peek key handling consumes only scroll/follow flags;
                                // worker_streams is reserved for future read-back uses.
        // If peek is open, dispatch to peek handler first.
        if let PlanInspectorMode::StreamPeek { .. } = &self.mode {
            if let Some(action) = self.handle_peek_key(key) {
                return Some(action);
            }
            // Even if the peek didn't produce an Action, swallow the key.
            return None;
        }
        // Otherwise: try the open-peek keys (`s`, `S`) before delegating
        // to the existing handle_key. These keys are inactive when there
        // is no plan or no selected task.
        if let Some(action) = self.maybe_handle_open_peek(key, ctx) {
            return Some(action);
        }
        <Self as super::View>::handle_key(self, key, ctx)
    }

    // Stubs filled in by Tasks 6 and 7.
    fn render_peek_overlay(
        _frame: &mut ratatui::Frame,
        _area: ratatui::layout::Rect,
        _executor_id: &str,
        _task_id: &str,
        _trace: Option<&mut crate::components::react_trace::ReactTrace>,
        _state: &mut crate::components::stream_pane::StreamViewState,
    ) {
        // Implemented in Task 6.
    }

    fn handle_peek_key(&mut self, _key: crossterm::event::KeyEvent) -> Option<crate::action::Action> {
        // Implemented in Task 7.
        None
    }

    fn maybe_handle_open_peek(
        &mut self,
        _key: crossterm::event::KeyEvent,
        _ctx: &super::ViewContext,
    ) -> Option<crate::action::Action> {
        // Implemented in Task 7.
        None
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p spur-tui plan_inspector
```

Expected: PASS — the no-panic test passes, no existing tests regress.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/plan_inspector.rs
git commit -m "feat(spur-tui): add plan_inspector side-door methods for WorkerStreams"
```

---

## Task 6: Render the peek overlay

**Files:**
- Modify: `crates/spur-tui/src/views/plan_inspector.rs`

- [ ] **Step 1: Write the failing test**

Add to the plan_inspector test module:

```rust
#[test]
fn peek_overlay_renders_in_60_col_terminal_without_panic() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic(&session_id);
    let lineage = spur_core::lineage::projection::ExecutorLineage::new();
    let ctx = ViewContext::test_ctx(&lineage); // adapt to existing test_ctx signature

    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.enter_stream_peek("worker-session-1".into(), "t-12".into());

    let mut ws = crate::worker_streams::WorkerStreams::new();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| {
        view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
    }).unwrap();

    // Verify the peek title text appears somewhere in the buffer.
    let buf = terminal.backend().buffer().clone();
    let dump = format!("{:?}", buf);
    assert!(dump.contains("stream:"), "expected 'stream:' title in buffer");
    assert!(dump.contains("t-12"), "expected task id in buffer");
}

#[test]
fn peek_overlay_falls_back_to_fullscreen_below_60_cols() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic(&session_id);
    let lineage = spur_core::lineage::projection::ExecutorLineage::new();
    let ctx = ViewContext::test_ctx(&lineage);

    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.enter_stream_peek("worker-session-1".into(), "t-12".into());

    let mut ws = crate::worker_streams::WorkerStreams::new();
    let backend = TestBackend::new(40, 20); // narrow → full-screen
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| {
        view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
    }).unwrap();
    // No assertion beyond "did not panic" — exact glyph layout is unstable.
}
```

Note: `projection` is computed but unused in the second test; suppress with `let _ = projection;` or drop it.

- [ ] **Step 2: Run to confirm they fail**

```bash
cargo test -p spur-tui peek_overlay
```

Expected: FAIL — overlay renders nothing yet (Task 5 stub).

- [ ] **Step 3: Implement `render_peek_overlay`**

Replace the stub in `plan_inspector.rs`:

```rust
fn render_peek_overlay(
    frame: &mut ratatui::Frame,
    parent_area: ratatui::layout::Rect,
    executor_id: &str,
    task_id: &str,
    trace: Option<&mut crate::components::react_trace::ReactTrace>,
    state: &mut crate::components::stream_pane::StreamViewState,
) {
    use ratatui::layout::Rect;
    use ratatui::widgets::Clear;

    // Pick area: ≥60 cols → centered popover at 80% × 60%; else full-screen.
    let area: Rect = if parent_area.width >= 60 {
        let w = (parent_area.width as u32 * 80 / 100).max(60) as u16;
        let h = (parent_area.height as u32 * 60 / 100).max(8) as u16;
        let x = parent_area.x + (parent_area.width.saturating_sub(w)) / 2;
        let y = parent_area.y + (parent_area.height.saturating_sub(h)) / 2;
        Rect::new(x, y, w, h)
    } else {
        parent_area
    };

    // Clear the underlying widget cells so the overlay is opaque.
    frame.render_widget(Clear, area);

    // Title text: "stream: <agent_or_executor> (<task_id>)" plus optional [completed].
    // We don't have the worker's friendly agent name here; the executor_id is fine
    // for now (callers can extend by passing an explicit display name later).
    let title_left = format!("stream: {} ({})", executor_id, task_id);
    let title_right_completed = trace
        .as_ref()
        .map(|t| t.is_terminal_for_peek())
        .unwrap_or(false);
    let title_right = if title_right_completed { Some("[completed]") } else { None };

    let bottom_hint = "[esc] close · [j/k] scroll · [f] follow";

    crate::components::stream_pane::render_stream(
        frame,
        area,
        &title_left,
        title_right,
        Some(bottom_hint),
        trace,
        state,
    );
}
```

`ReactTrace::is_terminal_for_peek()` does not exist yet. Two choices:
1. Add a trivial accessor `pub fn is_terminal_for_peek(&self) -> bool { /* derive from the trace's last entry status */ }` — search the existing `ReactTrace` API for a similar predicate (`is_done`, `is_idle`, etc.) and reuse if present.
2. If no such signal exists, drop the `[completed]` title decoration in this task and revisit in a follow-up. The plan does NOT require this feature to ship Task 6; it's a polish item that becomes a `// TODO(peek-completed-badge)` if no signal is available.

Pick whichever fits the existing `ReactTrace` API. If you punt, remove the `title_right_completed` block and pass `title_right = None`.

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p spur-tui peek_overlay
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/plan_inspector.rs
git commit -m "feat(spur-tui): render plan_inspector StreamPeek overlay"
```

---

## Task 7: Wire keybinds (`s`, `S`, and peek-internal keys)

**Files:**
- Modify: `crates/spur-tui/src/views/plan_inspector.rs`

- [ ] **Step 1: Write the failing tests**

Add to the plan_inspector test module:

```rust
fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
fn key_shift(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
}
fn key_code(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

#[test]
fn lowercase_s_enters_peek_when_task_has_worker() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic_and_worker(&session_id); // helper below
    let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");

    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();
    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.set_selected_task_id_for_tests(Some("t-12".into())); // expose if not already

    let action = view.handle_key_with_worker_streams(key('s'), &mut ws, &ctx);

    assert!(matches!(view.mode(), PlanInspectorMode::StreamPeek { .. }));
    // No Action emitted on local open.
    assert!(action.is_none());
}

#[test]
fn lowercase_s_flashes_hint_when_task_has_no_worker() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic(&session_id); // task without delegation_id
    let lineage = spur_core::lineage::projection::ExecutorLineage::new();

    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();
    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.set_selected_task_id_for_tests(Some("t-12".into()));

    let action = view.handle_key_with_worker_streams(key('s'), &mut ws, &ctx);

    assert!(matches!(view.mode(), PlanInspectorMode::Browse));
    assert!(matches!(action, Some(Action::FlashHint { .. })));
}

#[test]
fn shift_s_emits_focus_worker_action() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic_and_worker(&session_id);
    let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");

    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();
    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.set_selected_task_id_for_tests(Some("t-12".into()));

    let action = view.handle_key_with_worker_streams(key_shift('S'), &mut ws, &ctx);

    match action {
        Some(Action::FocusWorkerInDashboard { executor_id, tab }) => {
            assert_eq!(executor_id, "worker-session-1");
            assert_eq!(tab, crate::components::detail_pane::DetailTab::Stream);
        }
        other => panic!("expected FocusWorkerInDashboard, got {:?}", other),
    }
}

#[test]
fn esc_leaves_peek() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic_and_worker(&session_id);
    let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();

    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.enter_stream_peek("worker-session-1".into(), "t-12".into());

    let action = view.handle_key_with_worker_streams(key_code(KeyCode::Esc), &mut ws, &ctx);

    assert!(matches!(view.mode(), PlanInspectorMode::Browse));
    assert!(action.is_none());
}

#[test]
fn j_scrolls_peek_without_leaking_to_task_list() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic_and_worker(&session_id);
    let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();

    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    let initial_selection = view.selected_task_id_for_tests();
    view.enter_stream_peek("worker-session-1".into(), "t-12".into());

    // Set a non-zero scroll, then press 'k' to go up.
    if let Some(state) = view.peek_state_mut_for_tests() {
        state.scroll_offset = 10;
        state.is_following = false;
    }

    let _ = view.handle_key_with_worker_streams(key('k'), &mut ws, &ctx);

    // Peek scroll moved up by 1.
    if let PlanInspectorMode::StreamPeek { state, .. } = view.mode() {
        assert_eq!(state.scroll_offset, 9);
    }
    // Task selection unchanged.
    assert_eq!(view.selected_task_id_for_tests(), initial_selection);
}

#[test]
fn f_toggles_follow_in_peek() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_epic_and_worker(&session_id);
    let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();

    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.enter_stream_peek("worker-session-1".into(), "t-12".into());

    let initial = match view.mode() {
        PlanInspectorMode::StreamPeek { state, .. } => state.is_following,
        _ => panic!(),
    };

    let _ = view.handle_key_with_worker_streams(key('f'), &mut ws, &ctx);

    if let PlanInspectorMode::StreamPeek { state, .. } = view.mode() {
        assert_eq!(state.is_following, !initial);
    }
}
```

Add test helpers `projection_with_epic_and_worker`, `lineage_with_worker_for_task`, `view_context_for_tests`, `set_selected_task_id_for_tests`, `selected_task_id_for_tests`, `peek_state_mut_for_tests` next to the existing test helpers. These should mirror the patterns already used in tests at the bottom of `plan_inspector.rs` lines 863–1090.

For the lineage helper, the worker node needs `delegation_id = Some(...)` matching what `projection_with_epic_and_worker` sets on `TrackedTask`. Use the same `DelegationId` value (e.g. `"deleg-12"`) on both sides so `executor_id_for_delegation` resolves.

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p spur-tui plan_inspector
```

Expected: FAIL — multiple test failures, all in the new tests, because `maybe_handle_open_peek` and `handle_peek_key` are stubs from Task 5.

- [ ] **Step 3: Implement `maybe_handle_open_peek`**

Replace the stub:

```rust
fn maybe_handle_open_peek(
    &mut self,
    key: crossterm::event::KeyEvent,
    ctx: &super::ViewContext,
) -> Option<crate::action::Action> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let plan = self.active_plan(ctx)?;
    let task = self.selected_task(plan)?;
    let tab = crate::components::detail_pane::DetailTab::Stream;

    let no_worker_hint = || crate::action::Action::FlashHint {
        message: format!("Task {} has no active worker yet", task.task_id),
    };

    // Resolve worker executor_id from delegation_id, if any.
    let executor_id = task.delegation_id.as_ref().and_then(|did| {
        ctx.lineage
            .executor_id_for_delegation(&spur_core::lineage::types::DelegationId(did.clone()))
            .map(|eid| eid.0.clone())
    });

    match (key.code, key.modifiers) {
        (KeyCode::Char('s'), m) if m == KeyModifiers::NONE => match executor_id {
            Some(eid) => {
                self.enter_stream_peek(eid, task.task_id.clone());
                None
            }
            None => Some(no_worker_hint()),
        },
        (KeyCode::Char('S'), m) if m == KeyModifiers::NONE || m == KeyModifiers::SHIFT => {
            match executor_id {
                Some(eid) => Some(crate::action::Action::FocusWorkerInDashboard {
                    executor_id: eid,
                    tab,
                }),
                None => Some(no_worker_hint()),
            }
        }
        _ => None,
    }
}
```

Replace the path `spur_core::lineage::types::DelegationId` with whichever path the type actually lives at — check the import line in `lineage/adapter.rs:179` shown above for the canonical use.

- [ ] **Step 4: Implement `handle_peek_key`**

```rust
fn handle_peek_key(&mut self, key: crossterm::event::KeyEvent) -> Option<crate::action::Action> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let state = self.peek_state_mut()?;
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            self.leave_stream_peek();
            None
        }
        (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            state.scroll_down_by(1);
            None
        }
        (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            state.scroll_up_by(1);
            None
        }
        (KeyCode::Char('g'), KeyModifiers::NONE) => {
            state.scroll_to_top();
            None
        }
        (KeyCode::Char('G'), _) => {
            state.scroll_to_bottom();
            None
        }
        (KeyCode::Char('f'), KeyModifiers::NONE) => {
            state.toggle_follow();
            None
        }
        // `S` while peek is open: treat as "open it for real" — close peek
        // and emit the jump. We need executor_id; fall back: we already
        // captured it when entering peek. Pull it out before leaving.
        (KeyCode::Char('S'), m) if m == KeyModifiers::SHIFT || m == KeyModifiers::NONE => {
            let executor_id = match &self.mode {
                PlanInspectorMode::StreamPeek { executor_id, .. } => executor_id.clone(),
                _ => return None,
            };
            self.leave_stream_peek();
            Some(crate::action::Action::FocusWorkerInDashboard {
                executor_id,
                tab: crate::components::detail_pane::DetailTab::Stream,
            })
        }
        _ => None, // swallow everything else
    }
}
```

- [ ] **Step 5: Run tests to confirm they pass**

```bash
cargo test -p spur-tui plan_inspector
```

Expected: all peek-key tests + existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/plan_inspector.rs
git commit -m "feat(spur-tui): wire plan_inspector peek keybindings"
```

---

## Task 8: Auto-close peek on task or plan change

**Files:**
- Modify: `crates/spur-tui/src/views/plan_inspector.rs`

**Rationale:** Spec lifecycle rule: peek auto-closes if the selected task changes, the active plan changes, or the session is rebound underneath.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn peek_auto_closes_when_selected_task_changes() {
    let session_id = SessionId("brain-1".into());
    let projection = projection_with_two_running_tasks(&session_id); // new helper
    let lineage = lineage_with_two_workers(&session_id);             // new helper
    let ctx = view_context_for_tests(&lineage, &projection);
    let mut ws = crate::worker_streams::WorkerStreams::new();

    let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
    view.set_selected_task_id_for_tests(Some("t-12".into()));
    view.enter_stream_peek("worker-session-1".into(), "t-12".into());

    // Press 'j' to move to the next task in the lane.
    let _ = view.handle_key_with_worker_streams(key('j'), &mut ws, &ctx);

    // Selection must have advanced and peek auto-closed.
    assert_ne!(view.selected_task_id_for_tests(), Some("t-12".into()));
    assert!(matches!(view.mode(), PlanInspectorMode::Browse));
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test -p spur-tui peek_auto_closes_when_selected_task_changes
```

Expected: FAIL — peek currently stays open across selection changes.

- [ ] **Step 3: Implement auto-close**

In `set_selected_task_id_inner` (around line 88 of `plan_inspector.rs`), add at the top:

```rust
fn set_selected_task_id_inner(&mut self, task_id: Option<String>) {
    // Lifecycle: any selected-task change auto-closes a live StreamPeek.
    if matches!(self.mode, PlanInspectorMode::StreamPeek { .. }) {
        self.mode = PlanInspectorMode::Browse;
    }
    if self.selected_task_id.as_deref() != task_id.as_deref() {
        self.open_issue_id = None;
        self.task_detail_scroll = 0;
    }
    self.selected_task_id = task_id;
}
```

Also: in any place that switches `pinned_plan_id` or rebinds `session_id` (none today, but future-proof), the same auto-close applies. For now we only guard the `selected_task_id` path; a follow-up task can add a centralized lifecycle gate if more entry points emerge.

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p spur-tui plan_inspector
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/plan_inspector.rs
git commit -m "feat(spur-tui): auto-close StreamPeek when selected task changes"
```

---

## Task 9: App-level wiring (route the side-door + handle the new Action)

**Files:**
- Modify: `crates/spur-tui/src/app/mod.rs`
- Modify: `crates/spur-tui/src/app/input.rs`
- Modify: `crates/spur-tui/src/app/action_routing.rs`

- [ ] **Step 1: Write the failing test**

In `app/action_routing.rs`, add (or extend an existing) test module:

```rust
#[cfg(test)]
mod focus_worker_routing_tests {
    use super::*;
    use crate::action::Action;
    use crate::components::detail_pane::DetailTab;

    #[test]
    fn focus_worker_action_navigates_to_dashboard_and_targets_executor() {
        // Use whatever test harness this module already establishes for
        // routing tests. The assertion: after routing the action, the
        // app's current_view == ViewId::Dashboard, the Dashboard's
        // focused agent matches the executor_id, and DetailPane's
        // current_tab == DetailTab::Stream.
        //
        // If no harness exists, create a minimal one inline that holds an
        // `App` and exposes the relevant fields for assertion.
        let mut app = crate::app::test_support::new_app_with_dashboard();
        // Seed Dashboard with an executor named "worker-session-1".
        app.seed_executor_for_tests("worker-session-1");

        let action = Action::FocusWorkerInDashboard {
            executor_id: "worker-session-1".into(),
            tab: DetailTab::Stream,
        };
        app.route_action_for_tests(action);

        assert_eq!(app.current_view_for_tests(), crate::action::ViewId::Dashboard);
        assert_eq!(app.dashboard_focused_executor_for_tests(), Some("worker-session-1".into()));
        assert_eq!(app.dashboard_detail_tab_for_tests(), DetailTab::Stream);
    }
}
```

If `test_support::new_app_with_dashboard` does not exist, the implementer should add the smallest test seam needed. Look at existing tests in `app/action_routing.rs` (or the surrounding `app/*.rs` modules) to see how they assert routing behavior today; mirror that pattern. If routing tests are currently absent or fully integration-style, accept that this test may need to be an in-crate integration-style test using a real `App` constructed via existing public helpers.

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo test -p spur-tui focus_worker_routing_tests
```

Expected: FAIL — the action is currently unhandled.

- [ ] **Step 3: Wire the render call site**

In `crates/spur-tui/src/app/mod.rs` around line 638 (`ViewId::PlanInspector(_) =>` branch), change:

```rust
ViewId::PlanInspector(_) => {
    if let Some(ref mut view) = self.plan_inspector {
        view.render(frame, view_area, &ctx);
    }
}
```

to:

```rust
ViewId::PlanInspector(_) => {
    if let Some(ref mut view) = self.plan_inspector {
        view.render_with_worker_streams(frame, view_area, &mut self.worker_streams, &ctx);
    }
}
```

- [ ] **Step 4: Wire the key-event call site**

In `crates/spur-tui/src/app/input.rs` around line 209 (the `match self.current_view` arm in the key handler), find the `ViewId::PlanInspector(...) =>` branch (if absent, find where plan_inspector key events are dispatched and modify there). Replace the call to `view.handle_key(...)` with:

```rust
view.handle_key_with_worker_streams(key, &mut self.worker_streams, &ctx)
```

If the surrounding code differs (e.g., the dispatch uses a different binding name for the `ViewContext`), preserve that — only the method name + the extra `&mut self.worker_streams` argument changes.

- [ ] **Step 5: Handle the new Action in `action_routing.rs`**

Find the routing site that handles `Action::InspectWorkers` (it's the closest analogue: it switches to Dashboard and focuses a worker). Add a sibling arm:

```rust
Action::FocusWorkerInDashboard { executor_id, tab } => {
    self.current_view = ViewId::Dashboard;
    // Tell Dashboard to focus that executor and jump its DetailPane tab.
    self.dashboard.focus_executor_in_agents_tree(&executor_id);
    self.dashboard.detail_pane_mut().jump_to_tab(tab);
}
```

`focus_executor_in_agents_tree` may not exist verbatim — check the Dashboard public surface for the closest existing API (something like `agents_tree.focus(...)` or `select_executor`). If a public helper does not exist, add a small one that does the equivalent. Keep the wrapper narrow.

- [ ] **Step 6: Run tests to confirm they pass**

```bash
cargo test -p spur-tui
```

Expected: all tests pass (new routing test + everything previously green).

- [ ] **Step 7: Manual smoke test**

```bash
cargo build -p spur-tui
# Run the TUI against an existing session that has at least one running
# worker on a plan task. From the plan_inspector view:
#   - press 's' on the running task → overlay should appear with stream
#   - press 'j'/'k' → only the overlay scrolls
#   - press 'f' → toggles follow indicator on overlay bottom border
#   - press 'esc' → overlay closes, task list still selected
#   - press 'S' → jumps to Dashboard, that worker focused, Stream tab live
#   - press 's' on a task with no worker → status hint flashes, no overlay
#   - press 'j' while overlay open from previous task → overlay closes
#     and selection moves
```

If any manual case misbehaves, stop and file an inline fix — do not commit a broken UX.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/app/mod.rs \
        crates/spur-tui/src/app/input.rs \
        crates/spur-tui/src/app/action_routing.rs \
        crates/spur-tui/src/views/dashboard.rs # only if a new helper was added
git commit -m "feat(spur-tui): route FocusWorkerInDashboard + plug plan_inspector side-door"
```

---

## Task 10: Final verification

- [ ] **Step 1: Full test run**

```bash
cargo test --workspace
```

Expected: green across the workspace.

- [ ] **Step 2: Clippy + fmt**

```bash
cargo clippy -p spur-tui -p spur-core --no-deps -- -D warnings
cargo fmt --all -- --check
```

Expected: no warnings, no diffs.

- [ ] **Step 3: Manual end-to-end against a live brain session**

Reproduce the smoke test from Task 9 against the local dev brain. Confirm the listed behaviors observationally. If any anomaly appears, file it as a follow-up issue rather than amending the plan tasks (the plan is complete; observed defects are bug-fix work).

- [ ] **Step 4: Tag the implementation in beads**

Optional but standard for SPUR work — open a beads issue tracking the merge of this branch with the spec path:

```
docs/superpowers/specs/2026-05-18-plan-inspector-stream-peek-design.md
docs/superpowers/plans/2026-05-18-plan-inspector-stream-peek.md
```

---

## Spec Coverage Check

| Spec requirement | Task |
|---|---|
| `Mode { Browse, StreamPeek }` enum on plan_inspector | Task 4 |
| Reuse Stream renderer (no parallel impl) | Task 1 |
| Transient `StreamViewState`, not shared with Dashboard | Task 1, Task 4 |
| `s` opens peek; flash hint when no worker | Task 7 |
| `S` jumps to Dashboard + focuses worker + jump_to_tab(Stream) | Task 3 + Task 7 + Task 9 |
| `Esc`/`q` close; `j/k/g/G/f` scroll inside; other keys swallowed | Task 7 |
| Centered overlay ≥60 cols; full-screen fallback below | Task 6 |
| `[completed]` title decoration when worker finishes | Task 6 (polish; may be deferred if no `ReactTrace` predicate exists) |
| Auto-close peek when selected task changes | Task 8 |
| Action routed via app-level routing (not plan_inspector reaching into Dashboard) | Task 3 + Task 9 |
| Side-door methods mirroring Dashboard's pattern | Task 5 |

All requirements have a task.
