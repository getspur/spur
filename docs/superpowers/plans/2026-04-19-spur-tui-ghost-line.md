# spur-tui Intent Preview (Ghost-Line) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a 1-line dim-gray "ghost-line" under the input bar that previews what `Enter` will do, so the UI teaches itself. Supports five states: send-to-brain · send-as-new-session · interrupt-current-turn · delegation-hint · thinking-timer.

**Architecture:** A new pure module `components::ghost_line` owns (a) a `GhostLineState` enum, (b) a pure `derive()` function mapping relevant state → `Option<GhostLineState>`, (c) a `GhostLineView` ratatui widget, and (d) a `DelegationHeuristic` that fires the "may delegate" hint on conjunction/enumeration patterns and self-disables after 3 false positives per session. Rendered below the input bar in both `SessionDetailView` and `DashboardView`. Thinking-timer uses `BrainStatus::Thinking` plus a timestamp tracked on `App`. No new dependencies.

**Tech Stack:** Rust, `ratatui`, `crossterm` (already in the crate), std::time::Instant.

**Spec:** `docs/superpowers/specs/2026-04-19-spur-tui-ux-best-approach.md` §4.4, §5.2.

**Out of scope (per spec §5.2):** Intent-preview accuracy telemetry (deferred to Phase F2); state transitions beyond the five MVP variants.

---

## File Structure

**New files:**

- `crates/spur-tui/src/components/ghost_line.rs` — `GhostLineState` enum, pure `derive()`, `GhostLineView` widget.
- `crates/spur-tui/src/components/delegation_heuristic.rs` — conjunction detector + false-positive counter (owned by `App`).
- `crates/spur-tui/tests/ghost_line_states.rs` — pure state-derivation tests.
- `crates/spur-tui/tests/ghost_line_heuristic.rs` — delegation-heuristic tests.
- `crates/spur-tui/tests/ghost_line_render.rs` — widget render tests.

**Modified files:**

- `crates/spur-tui/src/components/mod.rs` — add `pub mod ghost_line; pub mod delegation_heuristic;`
- `crates/spur-tui/src/app.rs` — track `thinking_since: Option<Instant>` on `App`; update on `BrainStatus` transitions; own the `DelegationHeuristic` and mutate it on send-and-observe.
- `crates/spur-tui/src/views/session_detail.rs` — render `GhostLineView` below the input bar (1-row chunk).
- `crates/spur-tui/src/views/dashboard.rs` — render `GhostLineView` below the input bar when input non-empty.

---

## Type Reference (committed signatures — used across tasks)

```rust
// components/ghost_line.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhostLineState {
    SendToBrain,
    SendAsNewSession,
    InterruptCurrentTurn,
    DelegationHint,
    ThinkingTimer { secs: u64 },
}

/// Pure derivation — no I/O, no time lookups. All inputs explicit.
pub fn derive(ctx: GhostContext<'_>) -> Option<GhostLineState> { ... }

#[derive(Debug, Clone, Copy)]
pub struct GhostContext<'a> {
    /// Current input-bar text.
    pub input_text: &'a str,
    /// Whether any brain session is attached (false → Dashboard empty-state).
    pub brain_attached: bool,
    /// Some(secs) when brain is Thinking and has been for this many seconds.
    /// None otherwise.
    pub thinking_for_secs: Option<u64>,
    /// Result of the delegation heuristic. True when the input looks like
    /// enumerated/conjunction work and brain is idle.
    pub delegation_hint_active: bool,
    /// If true (SPUR_GHOST=0), derive always returns None.
    pub killed: bool,
}
```

```rust
// components/delegation_heuristic.rs
#[derive(Debug, Default)]
pub struct DelegationHeuristic {
    /// Number of times we hinted "may delegate" but the brain did NOT delegate
    /// on the next turn. Incremented by App::observe_turn_outcome.
    false_positives: u32,
    /// Set to true after 3 false positives; ghost-line will never emit
    /// DelegationHint again this session.
    disabled: bool,
}

impl DelegationHeuristic {
    pub fn evaluate(&self, input_text: &str) -> bool;
    pub fn record_hint_fired(&mut self);
    pub fn record_outcome(&mut self, brain_actually_delegated: bool);
    pub fn is_disabled(&self) -> bool;
}
```

---

## Task 1: Scaffold module + `GhostLineState` enum

**Files:**
- Create: `crates/spur-tui/src/components/ghost_line.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Test: `crates/spur-tui/tests/ghost_line_states.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/ghost_line_states.rs`:

```rust
use spur_tui::components::ghost_line::{derive, GhostContext, GhostLineState};

fn ctx(input: &str, brain_attached: bool) -> GhostContext<'_> {
    GhostContext {
        input_text: input,
        brain_attached,
        thinking_for_secs: None,
        delegation_hint_active: false,
        killed: false,
    }
}

#[test]
fn empty_input_yields_none() {
    assert_eq!(derive(ctx("", true)), None);
    assert_eq!(derive(ctx("   ", true)), None); // whitespace-only is empty
}

#[test]
fn killed_always_yields_none() {
    let mut c = ctx("hello", true);
    c.killed = true;
    assert_eq!(derive(c), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test ghost_line_states`
Expected: FAIL — unresolved import `spur_tui::components::ghost_line`.

- [ ] **Step 3: Create the module and enum**

Create `crates/spur-tui/src/components/ghost_line.rs`:

```rust
//! Intent Preview — the ghost-line under the input bar.
//!
//! A pure derivation of "what will Enter do?" based on explicit context.
//! No I/O, no time reads — all time values are passed in as `thinking_for_secs`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhostLineState {
    SendToBrain,
    SendAsNewSession,
    InterruptCurrentTurn,
    DelegationHint,
    ThinkingTimer { secs: u64 },
}

#[derive(Debug, Clone, Copy)]
pub struct GhostContext<'a> {
    pub input_text: &'a str,
    pub brain_attached: bool,
    pub thinking_for_secs: Option<u64>,
    pub delegation_hint_active: bool,
    pub killed: bool,
}

/// Pure derivation: `ctx` → `Option<GhostLineState>`.
///
/// Priority order:
/// 1. `killed` (SPUR_GHOST=0) — always `None`.
/// 2. Empty/whitespace input + no thinking timer — `None`.
/// 3. `thinking_for_secs >= 2` — `ThinkingTimer`.
/// 4. Input starts with `!` — `InterruptCurrentTurn`.
/// 5. `brain_attached == false` — `SendAsNewSession`.
/// 6. `delegation_hint_active == true` — `DelegationHint`.
/// 7. Otherwise — `SendToBrain`.
pub fn derive(ctx: GhostContext<'_>) -> Option<GhostLineState> {
    if ctx.killed {
        return None;
    }
    // Empty-input fallback only when no thinking timer — otherwise we still
    // want to show the timer even with empty input.
    let empty = ctx.input_text.trim().is_empty();
    if empty && ctx.thinking_for_secs.unwrap_or(0) < 2 {
        return None;
    }
    if let Some(secs) = ctx.thinking_for_secs {
        if secs >= 2 {
            return Some(GhostLineState::ThinkingTimer { secs });
        }
    }
    if ctx.input_text.starts_with('!') {
        return Some(GhostLineState::InterruptCurrentTurn);
    }
    if !ctx.brain_attached {
        return Some(GhostLineState::SendAsNewSession);
    }
    if ctx.delegation_hint_active {
        return Some(GhostLineState::DelegationHint);
    }
    Some(GhostLineState::SendToBrain)
}
```

Modify `crates/spur-tui/src/components/mod.rs` — add alongside existing module declarations:

```rust
pub mod ghost_line;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test ghost_line_states`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/ghost_line.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/tests/ghost_line_states.rs
git commit -m "feat(spur-tui): scaffold ghost-line module with state enum and pure derive"
```

---

## Task 2: `derive` covers states A/B/C/E (straight-forward inputs)

**Files:**
- Modify: `crates/spur-tui/tests/ghost_line_states.rs`

The `derive` implementation already produced in Task 1 covers all four non-heuristic states. This task LOCKS them in with tests.

- [ ] **Step 1: Append failing tests**

Append to `crates/spur-tui/tests/ghost_line_states.rs`:

```rust
#[test]
fn state_a_idle_brain_plain_text() {
    let r = derive(ctx("fix the login redirect bug", true));
    assert_eq!(r, Some(GhostLineState::SendToBrain));
}

#[test]
fn state_b_no_brain_attached_means_new_session() {
    let r = derive(ctx("build a CLI for log parsing", false));
    assert_eq!(r, Some(GhostLineState::SendAsNewSession));
}

#[test]
fn state_c_bang_prefix_means_interrupt() {
    let r = derive(ctx("!wait that's wrong", true));
    assert_eq!(r, Some(GhostLineState::InterruptCurrentTurn));
}

#[test]
fn state_c_interrupt_beats_new_session() {
    // Even when no brain attached, `!` prefix wins (user expressed clear intent).
    let r = derive(ctx("!hold on", false));
    assert_eq!(r, Some(GhostLineState::InterruptCurrentTurn));
}

#[test]
fn state_e_thinking_timer_fires_at_2_seconds_and_above() {
    let mut c = ctx("", true);
    c.thinking_for_secs = Some(1);
    assert_eq!(derive(c), None);

    c.thinking_for_secs = Some(2);
    assert_eq!(derive(c), Some(GhostLineState::ThinkingTimer { secs: 2 }));

    c.thinking_for_secs = Some(30);
    assert_eq!(derive(c), Some(GhostLineState::ThinkingTimer { secs: 30 }));
}

#[test]
fn state_e_thinking_beats_input_prefixes() {
    // When brain is thinking, timer takes priority over new-session / interrupt.
    let mut c = ctx("!hold on", true);
    c.thinking_for_secs = Some(5);
    assert_eq!(derive(c), Some(GhostLineState::ThinkingTimer { secs: 5 }));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test ghost_line_states`
Expected: PASS (8 tests total with the two from Task 1).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/tests/ghost_line_states.rs
git commit -m "test(spur-tui): lock ghost-line states A/B/C/E via derive tests"
```

---

## Task 3: `DelegationHeuristic` — conjunction detector + self-disable

**Files:**
- Create: `crates/spur-tui/src/components/delegation_heuristic.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Test: `crates/spur-tui/tests/ghost_line_heuristic.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/ghost_line_heuristic.rs`:

```rust
use spur_tui::components::delegation_heuristic::DelegationHeuristic;

#[test]
fn fresh_heuristic_is_enabled_and_quiet() {
    let h = DelegationHeuristic::default();
    assert!(!h.is_disabled());
}

#[test]
fn single_sentence_does_not_fire() {
    let h = DelegationHeuristic::default();
    assert!(!h.evaluate("fix the login redirect bug"));
}

#[test]
fn and_conjunction_with_two_items_does_not_fire() {
    // "A and B" — too few items for delegation to be obviously preferred.
    let h = DelegationHeuristic::default();
    assert!(!h.evaluate("refactor auth and add tests"));
}

#[test]
fn and_conjunction_with_three_items_fires() {
    let h = DelegationHeuristic::default();
    assert!(h.evaluate("refactor auth, add tests, and benchmark the endpoints"));
}

#[test]
fn then_chained_sequence_fires() {
    let h = DelegationHeuristic::default();
    assert!(h.evaluate("read the config, then parse the schema, then write migrations"));
}

#[test]
fn numbered_list_of_three_fires() {
    let h = DelegationHeuristic::default();
    assert!(h.evaluate("1. refactor auth  2. add tests  3. benchmark"));
}

#[test]
fn self_disables_after_three_false_positives() {
    let mut h = DelegationHeuristic::default();
    let query = "refactor auth, add tests, and benchmark the endpoints";
    assert!(h.evaluate(query));

    // Simulate three turns where we hinted but brain didn't delegate.
    h.record_hint_fired();
    h.record_outcome(false);
    assert!(h.evaluate(query)); // still enabled

    h.record_hint_fired();
    h.record_outcome(false);
    assert!(h.evaluate(query)); // still enabled (2 FPs)

    h.record_hint_fired();
    h.record_outcome(false);
    assert!(h.is_disabled(), "should disable after 3 false positives");
    assert!(!h.evaluate(query), "evaluate must return false once disabled");
}

#[test]
fn correct_delegations_do_not_count_as_false_positives() {
    let mut h = DelegationHeuristic::default();
    h.record_hint_fired();
    h.record_outcome(true); // brain DID delegate — no FP bump
    h.record_hint_fired();
    h.record_outcome(true);
    h.record_hint_fired();
    h.record_outcome(true);
    assert!(!h.is_disabled());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test ghost_line_heuristic`
Expected: FAIL — unresolved import `DelegationHeuristic`.

- [ ] **Step 3: Implement the heuristic**

Create `crates/spur-tui/src/components/delegation_heuristic.rs`:

```rust
//! Delegation-hint heuristic for the ghost-line.
//!
//! Fires when input looks like enumerated/conjunction work. Self-disables
//! after 3 false positives per session (brain hinted to delegate but didn't).

#[derive(Debug, Default)]
pub struct DelegationHeuristic {
    false_positives: u32,
    disabled: bool,
}

impl DelegationHeuristic {
    /// True when `input_text` looks like multi-step work AND the heuristic
    /// has not self-disabled. Conservative: requires >= 3 enumerated items
    /// or a `then`-chained sequence of >= 3 steps.
    pub fn evaluate(&self, input_text: &str) -> bool {
        if self.disabled {
            return false;
        }
        count_items(input_text) >= 3
    }

    /// Call immediately after showing the hint to the user. Used only to
    /// pair with `record_outcome` in the App's turn-observer.
    pub fn record_hint_fired(&mut self) {
        // No-op in MVP; reserved for Phase F2 telemetry (hint_show_count).
    }

    /// Called when the turn that followed a hinted send has resolved.
    /// `brain_actually_delegated == false` counts as a false positive.
    /// After 3 FPs, `self.disabled` latches true for the session.
    pub fn record_outcome(&mut self, brain_actually_delegated: bool) {
        if !brain_actually_delegated {
            self.false_positives += 1;
            if self.false_positives >= 3 {
                self.disabled = true;
            }
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }
}

/// Count distinct work items in `text`. Counts:
///  - comma-separated clauses (≥ 3 commas-or-semicolons at top level)
///  - `then` occurrences (case-insensitive word-boundary) as chain links
///  - numbered-list markers (`1.`, `2.`, `3.`) as items
///
/// Returns the MAX of the three signals.
fn count_items(text: &str) -> usize {
    let lower = text.to_lowercase();

    // Signal 1: comma/semicolon clauses. Trailing punctuation is stripped.
    let clause_count = text
        .split(|c: char| c == ',' || c == ';')
        .filter(|s| !s.trim().is_empty())
        .count();

    // Signal 2: `then` word occurrences. Simple word-boundary check.
    let then_count = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| *w == "then")
        .count()
        + 1; // N `then`s = N+1 items

    // Signal 3: numbered-list markers `1. 2. 3.`. Must be followed by space.
    let mut num_count = 0;
    for n in 1..=9 {
        let marker = format!("{}. ", n);
        if text.contains(&marker) {
            num_count += 1;
        }
    }

    [clause_count, then_count, num_count].into_iter().max().unwrap_or(0)
}
```

Modify `crates/spur-tui/src/components/mod.rs` — add alongside existing declarations:

```rust
pub mod delegation_heuristic;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test ghost_line_heuristic`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/delegation_heuristic.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/tests/ghost_line_heuristic.rs
git commit -m "feat(spur-tui): DelegationHeuristic with conjunction detector and 3-FP self-disable"
```

---

## Task 4: `GhostLineView` render widget

**Files:**
- Modify: `crates/spur-tui/src/components/ghost_line.rs`
- Test: `crates/spur-tui/tests/ghost_line_render.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/ghost_line_render.rs`:

```rust
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::ghost_line::{GhostLineState, GhostLineView};

fn render_line(state: GhostLineState, width: u16) -> String {
    let backend = TestBackend::new(width, 1);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width, height: 1 };
        let view = GhostLineView::new(state);
        f.render_widget(view, area);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.width).map(|x| buf[(x, 0)].symbol()).collect::<String>()
}

#[test]
fn send_to_brain_renders_expected_label() {
    let s = render_line(GhostLineState::SendToBrain, 60);
    assert!(s.contains("↵ send to brain") || s.contains("send to brain"));
}

#[test]
fn send_as_new_session() {
    let s = render_line(GhostLineState::SendAsNewSession, 60);
    assert!(s.contains("send as new session"));
}

#[test]
fn interrupt() {
    let s = render_line(GhostLineState::InterruptCurrentTurn, 60);
    assert!(s.contains("interrupt"));
}

#[test]
fn delegation_hint_is_hedged() {
    let s = render_line(GhostLineState::DelegationHint, 80);
    assert!(s.contains("may delegate"), "hint must use soft language, got: {s}");
}

#[test]
fn thinking_timer_includes_seconds() {
    let s = render_line(GhostLineState::ThinkingTimer { secs: 4 }, 60);
    assert!(s.contains("thinking"));
    assert!(s.contains("4"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test ghost_line_render`
Expected: FAIL — unresolved `GhostLineView`.

- [ ] **Step 3: Add `GhostLineView`**

Append to `crates/spur-tui/src/components/ghost_line.rs`:

```rust
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

pub struct GhostLineView {
    state: GhostLineState,
}

impl GhostLineView {
    pub fn new(state: GhostLineState) -> Self {
        Self { state }
    }
}

fn label_for(state: &GhostLineState) -> String {
    match state {
        GhostLineState::SendToBrain => "↵ send to brain".into(),
        GhostLineState::SendAsNewSession => "↵ send as new session".into(),
        GhostLineState::InterruptCurrentTurn => "↵ interrupt current turn".into(),
        GhostLineState::DelegationHint => "↵ send · brain may delegate (hint)".into(),
        GhostLineState::ThinkingTimer { secs } => {
            format!("↵ enter to queue · brain is thinking ({}s)", secs)
        }
    }
}

impl Widget for GhostLineView {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let style = match self.state {
            GhostLineState::InterruptCurrentTurn => Style::default().fg(Color::LightRed),
            GhostLineState::DelegationHint => Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            _ => Style::default().fg(Color::DarkGray),
        };
        let line = Line::from(Span::styled(label_for(&self.state), style));
        Paragraph::new(line).render(area, buf);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test ghost_line_render`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/ghost_line.rs \
        crates/spur-tui/tests/ghost_line_render.rs
git commit -m "feat(spur-tui): GhostLineView widget renders per-state labels in dim gray"
```

---

## Task 5: `SPUR_GHOST=0` env kill-switch

**Files:**
- Modify: `crates/spur-tui/src/components/ghost_line.rs`
- Test: `crates/spur-tui/tests/ghost_line_states.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/ghost_line_states.rs`:

```rust
use spur_tui::components::ghost_line::ghost_killed_by_env;

#[test]
fn ghost_killed_by_env_reads_spur_ghost_variable() {
    std::env::set_var("SPUR_GHOST", "0");
    assert!(ghost_killed_by_env());
    std::env::set_var("SPUR_GHOST", "1");
    assert!(!ghost_killed_by_env());
    std::env::remove_var("SPUR_GHOST");
    assert!(!ghost_killed_by_env());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test ghost_line_states`
Expected: FAIL — unresolved `ghost_killed_by_env`.

- [ ] **Step 3: Add the env reader**

Append to `crates/spur-tui/src/components/ghost_line.rs`:

```rust
/// Read SPUR_GHOST env var — `"0"` disables the ghost-line entirely.
///
/// Callers should pass the result into `GhostContext::killed`. Exposed as a
/// top-level helper so the session/dashboard views share one read site.
pub fn ghost_killed_by_env() -> bool {
    std::env::var("SPUR_GHOST").map(|v| v == "0").unwrap_or(false)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test ghost_line_states -- --test-threads=1`

(Single-threaded because env-var mutation is process-global — avoids interference with parallel tests. The `--test-threads=1` flag ensures sequential execution for this test file only.)

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/ghost_line.rs \
        crates/spur-tui/tests/ghost_line_states.rs
git commit -m "feat(spur-tui): SPUR_GHOST=0 env kill-switch for ghost-line"
```

---

## Task 6: Thinking-since timestamp on `App`

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Test: `crates/spur-tui/tests/ghost_line_integration.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/ghost_line_integration.rs`:

```rust
use spur_tui::app::BrainStatus;
use spur_tui::test_support::new_app;

#[test]
fn thinking_since_set_on_transition_to_thinking() {
    let mut app = new_app();
    assert!(app.thinking_since_for_test().is_none());

    app.set_brain_status_for_test(BrainStatus::Thinking);
    assert!(app.thinking_since_for_test().is_some(), "expected timestamp once thinking");
}

#[test]
fn thinking_since_cleared_on_transition_away() {
    let mut app = new_app();
    app.set_brain_status_for_test(BrainStatus::Thinking);
    app.set_brain_status_for_test(BrainStatus::Ready);
    assert!(app.thinking_since_for_test().is_none());
}

#[test]
fn thinking_since_preserved_across_identical_transitions() {
    let mut app = new_app();
    app.set_brain_status_for_test(BrainStatus::Thinking);
    let t1 = app.thinking_since_for_test().unwrap();
    app.set_brain_status_for_test(BrainStatus::Thinking);
    let t2 = app.thinking_since_for_test().unwrap();
    assert_eq!(t1, t2, "same-state re-set must not reset the timestamp");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test ghost_line_integration`
Expected: FAIL — unresolved `thinking_since_for_test` / `set_brain_status_for_test`.

- [ ] **Step 3: Add the field and test hooks**

In `crates/spur-tui/src/app.rs`, inside `pub struct App { ... }` (near `brain_status: BrainStatus`), add:

```rust
    /// When brain transitioned to Thinking. None when brain is not thinking.
    thinking_since: Option<std::time::Instant>,
```

In `build_with_license_state` inside the `Self { ... }` literal, add:

```rust
            thinking_since: None,
```

Locate the existing assignment to `self.brain_status`. Wrap all assignments through a new helper so thinking_since stays in sync. Search for lines like `self.brain_status = BrainStatus::...` and replace them with `self.set_brain_status(...)`. Add this helper method:

```rust
    fn set_brain_status(&mut self, next: BrainStatus) {
        let was_thinking = matches!(self.brain_status, BrainStatus::Thinking);
        let now_thinking = matches!(next, BrainStatus::Thinking);
        match (was_thinking, now_thinking) {
            (false, true) => { self.thinking_since = Some(std::time::Instant::now()); }
            (true, false) => { self.thinking_since = None; }
            _ => {}
        }
        self.brain_status = next;
    }
```

Add the test-only accessors:

```rust
    #[cfg(any(test, debug_assertions))]
    pub fn thinking_since_for_test(&self) -> Option<std::time::Instant> {
        self.thinking_since
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_brain_status_for_test(&mut self, s: BrainStatus) {
        self.set_brain_status(s);
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test ghost_line_integration`
Expected: PASS (3 tests).

Also run the full suite to catch any regression from the brain_status call-site rewrites:

```bash
cargo test -p spur-tui
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/ghost_line_integration.rs
git commit -m "feat(spur-tui): track thinking_since on App for ghost-line timer"
```

---

## Task 7: App owns `DelegationHeuristic` + send-and-observe hook

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/ghost_line_integration.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/ghost_line_integration.rs`:

```rust
#[test]
fn heuristic_records_hint_on_send_with_conjunction_input() {
    let mut app = new_app();
    // Simulate a user sending text that would have triggered a hint.
    app.record_turn_started_for_test("refactor A, add tests for B, and benchmark C");
    // Now simulate brain turn completing WITHOUT delegation.
    app.record_turn_ended_for_test(false);
    app.record_turn_started_for_test("refactor A, add tests for B, and benchmark C");
    app.record_turn_ended_for_test(false);
    app.record_turn_started_for_test("refactor A, add tests for B, and benchmark C");
    app.record_turn_ended_for_test(false);

    assert!(app.heuristic_for_test().is_disabled(), "after 3 FPs, heuristic disables");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test ghost_line_integration`
Expected: FAIL — unresolved `record_turn_started_for_test` etc.

- [ ] **Step 3: Add the heuristic field and hooks to `App`**

In `crates/spur-tui/src/app.rs`, inside `pub struct App { ... }` add:

```rust
    delegation_heuristic: crate::components::delegation_heuristic::DelegationHeuristic,
    /// Set to true at `record_turn_started` when the user input would have
    /// triggered a delegation hint. Flipped to the actual outcome at
    /// `record_turn_ended` via heuristic.record_outcome.
    last_turn_hinted: bool,
```

In the `Self { ... }` literal inside `build_with_license_state`:

```rust
            delegation_heuristic: crate::components::delegation_heuristic::DelegationHeuristic::default(),
            last_turn_hinted: false,
```

Add these methods on `impl App`:

```rust
    /// Called when the user sends a message. Evaluates the heuristic against
    /// the input text; records whether a hint would have fired for this turn.
    pub fn record_turn_started(&mut self, input_text: &str) {
        self.last_turn_hinted = self.delegation_heuristic.evaluate(input_text);
        if self.last_turn_hinted {
            self.delegation_heuristic.record_hint_fired();
        }
    }

    /// Called when the brain's turn completes. If a hint was shown for this
    /// turn but the brain did not delegate, this counts as a false positive.
    pub fn record_turn_ended(&mut self, brain_delegated: bool) {
        if self.last_turn_hinted {
            self.delegation_heuristic.record_outcome(brain_delegated);
        }
        self.last_turn_hinted = false;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn record_turn_started_for_test(&mut self, input: &str) {
        self.record_turn_started(input);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn record_turn_ended_for_test(&mut self, delegated: bool) {
        self.record_turn_ended(delegated);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn heuristic_for_test(&self) -> &crate::components::delegation_heuristic::DelegationHeuristic {
        &self.delegation_heuristic
    }
```

Locate the existing `process_action` arm for `Action::SendMessage` / `Action::NewSessionWithMessage` and add a call to `self.record_turn_started(&text)` where `text` is the concatenation of all text blocks. For MVP, call it from the SendMessage arm only:

```rust
Action::SendMessage { session, blocks, interrupt } => {
    // NEW: seed the delegation heuristic with the outgoing text.
    let text = blocks.iter().filter_map(|b| {
        if let spur_acp::ContentBlock::Text(t) = b { Some(t.clone()) } else { None }
    }).collect::<Vec<_>>().join(" ");
    self.record_turn_started(&text);
    // ... existing dispatch ...
}
```

(Adjust `ContentBlock::Text` to the real variant name — `rg -n "enum ContentBlock" crates/spur-acp/src/` will confirm.)

Add a call to `self.record_turn_ended(delegated)` wherever a turn resolves. Use `SpurEventBody::TurnComplete` or equivalent as the hook; observe whether the turn produced a `DelegationRequested` by tracking a per-turn flag:

```rust
// In handle_spur_event, for DelegationRequested:
SpurEventBody::DelegationRequested { .. } => {
    self.this_turn_delegated = true;
    // ... existing handling ...
}
// On TurnComplete:
SpurEventBody::TurnComplete { .. } => {
    self.record_turn_ended(self.this_turn_delegated);
    self.this_turn_delegated = false;
    // ... existing handling ...
}
```

Add `this_turn_delegated: bool` to the struct and `false` to the initializer.

(If `TurnComplete` is not a variant, use the actual "turn ended" event. The audit confirmed `force_flush_all` is called on TurnComplete in `session_detail.rs:1409` — search for that site to find the exact event name.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test ghost_line_integration`
Expected: PASS.

Full suite: `cargo test -p spur-tui` — PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/ghost_line_integration.rs
git commit -m "feat(spur-tui): App tracks delegation heuristic outcomes per turn"
```

---

## Task 8: Wire ghost-line into `SessionDetailView`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/app.rs` (call-site)

- [ ] **Step 1: Read the current layout**

The audit documented 5 vertical chunks at `session_detail.rs:1666-1673`: header · trace · workers · input · status. We need to add a 1-row ghost-line chunk between input and status.

Run: `rg -n "Constraint::" crates/spur-tui/src/views/session_detail.rs | head -30`
Confirm the existing layout uses `ratatui::layout::Layout` with Constraint::Length and Constraint::Min. Count the constraints to understand the current split.

- [ ] **Step 2: Add the ghost-line chunk and render**

In `SessionDetailView::render` (or wherever the layout is built), modify the constraint list to insert a 1-row chunk just below the input bar:

```rust
// BEFORE (illustrative — match the real code):
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(1),    // header
        Constraint::Min(4),       // trace
        Constraint::Length(workers_h),
        Constraint::Length(input_height),
        Constraint::Length(1),    // status
    ])
    .split(area);

// AFTER:
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(1),    // header
        Constraint::Min(4),       // trace
        Constraint::Length(workers_h),
        Constraint::Length(input_height),
        Constraint::Length(1),    // ghost-line (new)
        Constraint::Length(1),    // status
    ])
    .split(area);
```

Render the ghost-line into the new chunk. The view needs access to: input text (already has it via `input_bar.text()`), brain_status snapshot (passed via `ViewContext` — confirm), thinking_since (needs to flow in).

Plumb `thinking_for_secs: Option<u64>` through `ViewContext` in `crates/spur-tui/src/views/mod.rs`:

```rust
pub struct ViewContext<'a> {
    pub lineage: &'a spur_core::lineage::projection::ExecutorLineage,
    pub brain_status: &'a crate::app::BrainStatus,
    pub license_badge: Option<&'a crate::components::status_bar::LicenseBadge>,
    // NEW:
    pub thinking_for_secs: Option<u64>,
    pub delegation_hint_active: bool,
    pub ghost_killed: bool,
}
```

Update `test_support::test_view_ctx` and the lib-level `new_app` helpers to set the three new fields to defaults (`None`, `false`, `false`).

In `App::render`, before calling the view's render, compute and pass:

```rust
let thinking_for_secs = self.thinking_since.map(|t| t.elapsed().as_secs());
let delegation_hint_active = self
    .input_bar_text_or_empty() // add this helper — returns session_detail's input text
    .map(|t| self.delegation_heuristic.evaluate(t))
    .unwrap_or(false);
let ghost_killed = crate::components::ghost_line::ghost_killed_by_env();
let ctx = ViewContext { /* ... */, thinking_for_secs, delegation_hint_active, ghost_killed };
```

Add `fn input_bar_text_or_empty(&self) -> Option<&str>` on `App` returning `self.session_detail.as_ref().map(|v| v.input_text())` — and add the `input_text()` accessor on `SessionDetailView` returning `self.input_bar.text()`.

Finally, inside `SessionDetailView::render`, render the ghost-line into its chunk:

```rust
let ghost_ctx = crate::components::ghost_line::GhostContext {
    input_text: self.input_bar.text(),
    brain_attached: true, // always true inside SessionDetailView
    thinking_for_secs: ctx.thinking_for_secs,
    delegation_hint_active: ctx.delegation_hint_active,
    killed: ctx.ghost_killed,
};
if let Some(state) = crate::components::ghost_line::derive(ghost_ctx) {
    let view = crate::components::ghost_line::GhostLineView::new(state);
    f.render_widget(view, ghost_chunk);
}
```

(Adjust `self.input_bar.text()` if the accessor is named differently; `rg -n "pub fn text" crates/spur-tui/src/components/input_bar.rs` will confirm.)

- [ ] **Step 3: Build and verify visually**

Run: `cargo build -p spur-tui`
Expected: SUCCESS.

Run the TUI (however the dev binary is invoked — `cargo run -p spur-tui --example react_trace_bench_sim` or the standard entry point) and verify:
- Ghost-line visible under input bar on SessionDetail view.
- Typing plain text shows `↵ send to brain` in dim gray.
- Typing `!hold on` shows `↵ interrupt current turn` in light red.
- Typing `refactor A, add tests for B, and benchmark C` shows the delegation hint.

- [ ] **Step 4: Run full suite**

Run: `cargo test -p spur-tui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/views/mod.rs \
        crates/spur-tui/src/components/input_bar.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/lib.rs
git commit -m "feat(spur-tui): render ghost-line below input bar in SessionDetail"
```

---

## Task 9: Wire ghost-line into `DashboardView`

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Read the dashboard layout**

The audit documented the populated dashboard as four vertical bands + input + status. For empty-state, only input + status. In both, add a 1-row ghost-line chunk above the status bar.

Run: `rg -n "Constraint::|Layout::default" crates/spur-tui/src/views/dashboard.rs | head -30`

- [ ] **Step 2: Add the ghost-line chunk and render**

In both the empty-state and populated render paths (search for `dashboard.rs:363` for empty-state, `dashboard.rs:431` for populated per audit), insert a 1-row chunk:

```rust
.constraints([
    /* existing constraints */
    Constraint::Length(1), // ghost-line (new)
    Constraint::Length(1), // status
])
```

Render similarly to Task 8. Dashboard sets `brain_attached: false` whenever `lineage.nodes().count() == 0` (empty state) — or always follow the same rule as SessionDetail when populated. For MVP, the dashboard rule is simple:

```rust
let brain_attached = /* whatever signal the dashboard uses to know brain is live.
                       In MVP, conservative: `false` whenever lineage is empty, otherwise `true`. */;
let ghost_ctx = GhostContext {
    input_text: self.input_bar.text(),
    brain_attached,
    thinking_for_secs: ctx.thinking_for_secs,
    delegation_hint_active: ctx.delegation_hint_active,
    killed: ctx.ghost_killed,
};
if let Some(state) = derive(ghost_ctx) {
    let view = GhostLineView::new(state);
    f.render_widget(view, ghost_chunk);
}
```

(The dashboard does not store its own input text if the input bar is shared — look up the real path.)

- [ ] **Step 3: Build and verify visually**

Run: `cargo build -p spur-tui`

Launch TUI and verify:
- Ghost-line visible on Dashboard.
- Empty input → ghost-line hidden (None from derive).
- Typing "build a CLI for log parsing" on empty dashboard → shows `↵ send as new session`.

- [ ] **Step 4: Run full suite**

Run: `cargo test -p spur-tui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): render ghost-line below input bar on Dashboard"
```

---

## Post-Plan Verification

After all tasks complete:

```bash
cargo test -p spur-tui
cargo clippy -p spur-tui --all-targets -- -D warnings
cargo fmt -p spur-tui -- --check
```

Manual smoke checklist:

- [ ] Ghost-line hidden when input empty and brain not thinking
- [ ] Dashboard (no session): typing text → "send as new session"
- [ ] SessionDetail idle: typing text → "send to brain"
- [ ] SessionDetail streaming: brain thinking ≥ 2s → "brain is thinking (Ns)" even with empty input
- [ ] Typing `!stop` → "interrupt current turn" in red
- [ ] Typing multi-item conjunction → "brain may delegate (hint)" in italic
- [ ] `SPUR_GHOST=0 cargo run …` → ghost-line never appears
- [ ] After 3 non-delegating turns with hint shown, heuristic stops firing for the session

---

_End of plan. See `2026-04-19-spur-tui-teachable-moments.md` for Plan 3._
