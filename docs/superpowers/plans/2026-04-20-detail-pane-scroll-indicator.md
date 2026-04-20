# DetailPane Scroll Indicator + jump_to_tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `DetailPane`'s two-state `following_indicator` with a state-adaptive `scroll_label` (9 states) and close the Review-jump invariant bypass via a new `jump_to_tab` helper that shares a private `set_tab` with `cycle_tab`.

**Architecture:** Extract a pure `scroll_label(tab, total, visible_h, scroll_offset, is_following, trace_following)` function returning `Cow<'static, str>`. Reorder `render()` so wrapped-line metrics are computed before the block is built, using a shape-equivalent skeleton block for `inner()`. Add a private `set_tab(tab, stream_trace)` helper that encodes the per-tab reset invariants; `cycle_tab` delegates to it and a new `jump_to_tab` sits on top. Downgrade `current_tab` to `pub(crate)` behind a `current_tab()` getter so direct writes from outside the crate become compile errors.

**Tech Stack:** Rust 2024, ratatui 0.29, crossterm. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-20-detail-pane-scroll-indicator-design.md`

---

## File Structure

### Files modified

| File | Responsibility after this change |
|---|---|
| `crates/spur-tui/src/components/detail_pane.rs` | Owns `DetailPane`, `DetailTab`, new private `set_tab`, new public `jump_to_tab`, new pure `scroll_label`, reordered `render()`. `current_tab` field becomes `pub(crate)`; public read access via `current_tab()`. |
| `crates/spur-tui/src/app.rs` | The one direct-write site at `app.rs:1609` migrates to `jump_to_tab(DetailTab::Review, None)`. |
| `crates/spur-tui/tests/review_submission.rs` | The direct-write site at `:108` migrates to `jump_to_tab(...)`. |
| `crates/spur-tui/tests/detail_pane_scroll.rs` | Five read sites (`:87, :160, :199, :234, :241`) migrate from `.current_tab` field access to `.current_tab()` getter. |

### Files NOT modified

| File | Reason |
|---|---|
| `crates/spur-tui/src/views/dashboard.rs:606,824` | Two read sites; both are in-crate so `pub(crate)` field access continues to compile. No migration needed. |
| `crates/spur-tui/src/components/react_trace/*` | Out of scope; Stream-tab delegation is unchanged. |

### Pre-flight check

- [ ] **Step 0.1: Confirm clean working tree on the correct branch.**

  Run: `git status --porcelain && git rev-parse --abbrev-ref HEAD`
  Expected: empty status (or only pre-existing unrelated untracked files), branch name present.

  If not on a feature branch for this work, create one:
  ```bash
  git checkout -b feat/detail-pane-scroll-indicator
  ```

- [ ] **Step 0.2: Baseline build + tests green before changes.**

  Run: `cargo check -p spur-tui`
  Expected: no errors.

  Run: `cargo test -p spur-tui --test detail_pane_scroll`
  Expected: all pass.

---

## Task 1: Extract pure `scroll_label` function + unit tests

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs`

This task adds the pure function and its test matrix WITHOUT changing render. The function is unused by production code after this task; Task 2 wires it in.

- [ ] **Step 1.1: Add the `use std::borrow::Cow;` import at the top of the file.**

  At the top of `crates/spur-tui/src/components/detail_pane.rs`, just below the existing `use` lines (around line 7), add:

  ```rust
  use std::borrow::Cow;
  ```

- [ ] **Step 1.2: Add the pure `scroll_label` function near the top of the `impl DetailPane` block** (before `render`, around line 155).

  Note: this function is a free-standing helper that does NOT take `&self`. Place it OUTSIDE the `impl DetailPane` block, directly above or below it, so it's trivially unit-testable. Recommended placement: just before `impl DetailPane` (around line 52).

  Insert:

  ```rust
  /// Compute the bottom-border scroll label for a DetailPane state.
  ///
  /// Pure function — no ratatui dependencies, no borrow of `DetailPane`.
  /// Exhaustive state coverage per the design spec's state table.
  ///
  /// Arguments:
  ///   - `tab`: current tab
  ///   - `total`: number of wrapped body rows (0 if empty/placeholder)
  ///   - `visible_h`: viewport height in rows
  ///   - `scroll_offset`: current scroll offset in rows
  ///   - `is_following`: pane's own follow flag (authoritative for non-Stream)
  ///   - `stream_trace_following`: Some(trace.is_following()) when on Stream
  ///     with a trace present; None when on Stream placeholder or on a
  ///     non-Stream tab.
  fn scroll_label(
      tab: DetailTab,
      total: usize,
      visible_h: usize,
      scroll_offset: usize,
      is_following: bool,
      stream_trace_following: Option<bool>,
  ) -> Cow<'static, str> {
      // Stream tab — follow flag comes from the trace (if present).
      if matches!(tab, DetailTab::Stream) {
          return match stream_trace_following {
              Some(true) => Cow::Borrowed(" ▼ following "),
              Some(false) => Cow::Borrowed(" ▲ paused "),
              // No trace yet — placeholder path. Default to "following"
              // so the initial render does not look stalled.
              None => Cow::Borrowed(" ▼ following "),
          };
      }

      // Non-Stream tabs — authoritative scroll + follow state on DetailPane.
      if total == 0 {
          return Cow::Borrowed("");
      }
      let max_offset = total.saturating_sub(visible_h);
      if max_offset == 0 {
          // Content fits viewport; nothing to scroll.
          return Cow::Borrowed(" ▼ ");
      }
      if is_following {
          return Cow::Borrowed(" ▼ ");
      }
      if scroll_offset == 0 {
          return Cow::Borrowed(" top ");
      }
      if scroll_offset >= max_offset {
          return Cow::Borrowed(" end ");
      }
      Cow::Owned(format!(" ▲ {} ↑ ", scroll_offset))
  }
  ```

- [ ] **Step 1.3: Run `cargo check -p spur-tui` to confirm the file compiles.**

  Run: `cargo check -p spur-tui`
  Expected: no errors. One warning expected: `scroll_label` is never used (fixed in Task 2).

- [ ] **Step 1.4: Commit the pure function.**

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs
  git commit -m "feat(detail-pane): add pure scroll_label helper

  State-adaptive label for the bottom border. Unused until Task 2
  wires it into render()."
  ```

- [ ] **Step 1.5: Add unit tests inside an inline `#[cfg(test)]` module** at the end of `detail_pane.rs`. If the file already has an inline test module, append to it; otherwise create a new one.

  Append at the very end of the file:

  ```rust
  #[cfg(test)]
  mod scroll_label_tests {
      use super::*;

      #[test]
      fn stream_with_trace_following_shows_following() {
          let s = scroll_label(DetailTab::Stream, 0, 0, 0, false, Some(true));
          assert_eq!(s, Cow::Borrowed(" ▼ following "));
      }

      #[test]
      fn stream_with_trace_paused_shows_paused() {
          let s = scroll_label(DetailTab::Stream, 0, 0, 0, false, Some(false));
          assert_eq!(s, Cow::Borrowed(" ▲ paused "));
      }

      #[test]
      fn stream_without_trace_shows_following() {
          // Placeholder path — no trace yet but pane wants to look live.
          let s = scroll_label(DetailTab::Stream, 1, 10, 0, true, None);
          assert_eq!(s, Cow::Borrowed(" ▼ following "));
      }

      #[test]
      fn non_stream_empty_total_shows_blank() {
          let s = scroll_label(DetailTab::Artifacts, 0, 20, 0, false, None);
          assert_eq!(s, Cow::Borrowed(""));
      }

      #[test]
      fn non_stream_content_fits_viewport_shows_down() {
          // total=10, visible=20 → max_offset=0 → content fits.
          let s = scroll_label(DetailTab::Task, 10, 20, 0, false, None);
          assert_eq!(s, Cow::Borrowed(" ▼ "));
      }

      #[test]
      fn non_stream_at_end_following_shows_down() {
          // total=100, visible=20, offset=80, following → " ▼ ".
          let s = scroll_label(DetailTab::Attempts, 100, 20, 80, true, None);
          assert_eq!(s, Cow::Borrowed(" ▼ "));
      }

      #[test]
      fn non_stream_at_top_shows_top() {
          // total=100, visible=20, offset=0, not following → " top ".
          let s = scroll_label(DetailTab::Review, 100, 20, 0, false, None);
          assert_eq!(s, Cow::Borrowed(" top "));
      }

      #[test]
      fn non_stream_at_end_not_following_shows_end() {
          // total=100, visible=20, offset=80 (= max_offset), not following.
          let s = scroll_label(DetailTab::Artifacts, 100, 20, 80, false, None);
          assert_eq!(s, Cow::Borrowed(" end "));
      }

      #[test]
      fn non_stream_mid_scroll_shows_arrow_count() {
          // total=100, visible=20, offset=42 → " ▲ 42 ↑ ".
          let s = scroll_label(DetailTab::Artifacts, 100, 20, 42, false, None);
          assert_eq!(s, Cow::<'static, str>::Owned(" ▲ 42 ↑ ".to_string()));
      }
  }
  ```

- [ ] **Step 1.6: Run the new tests.**

  Run: `cargo test -p spur-tui --lib scroll_label_tests`
  Expected: **9 passed; 0 failed**.

- [ ] **Step 1.7: Commit the tests.**

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs
  git commit -m "test(detail-pane): exhaustive scroll_label state matrix

  9 tests covering every row of the spec's state table."
  ```

---

## Task 2: Wire `scroll_label` into `render()` via the reorder

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs` — `render()` method (lines 155-280 approx)

This task replaces the two-state `following_indicator` with the `scroll_label` call AND reorders so wrapped-line metrics are computed before the block is built.

- [ ] **Step 2.1: Rewrite `render()` as a single replacement.**

  Replace the entire `render()` method body (from `pub fn render(...)` signature through its closing `}`, currently around lines 155-280) with this implementation. Paste verbatim.

  ```rust
  pub fn render(
      &mut self,
      frame: &mut Frame,
      area: Rect,
      node: &ExecutorNode,
      issue_badge: Option<&str>,
      stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
  ) {
      // ── 1. Compute the trace-follow flag (Stream tab authoritative
      //       source) before any rendering side-effects. ─────────────
      let trace_following: Option<bool> = match self.current_tab {
          DetailTab::Stream => stream_trace.as_deref().map(|t| t.is_following()),
          _ => None,
      };

      // ── 2. Skeleton block — shape-equivalent to the final block so
      //       Block::inner() returns the same rect. ──────────────────
      //
      // Every title POSITION that will appear on the final block must
      // also appear on the skeleton. Content can be placeholder because
      // inner() is a function of borders + title presence, not content.
      let mut skeleton = Block::default()
          .borders(Borders::ALL)
          .title(" ")              // matches final top-left (agent name)
          .title_bottom(" ");      // matches final bottom-left (scroll_label)
      if issue_badge.is_some() {
          skeleton = skeleton
              .title_top(Line::from(" ").alignment(Alignment::Right))   // matches final top-right (badge)
              .title_bottom(Line::from(" ").alignment(Alignment::Right)); // matches final bottom-right ([I]ssue)
      }
      let inner = skeleton.inner(area);
      let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
      let body_area = chunks[1];

      // ── 3. Compute body content + metrics for non-Stream (and Stream
      //       placeholder) paths. For Stream-with-trace, body is owned
      //       by ReactTrace::render_compact; total/visible still
      //       meaningful only for the `scroll_label` derivation. ─────
      let stream_with_trace = matches!(self.current_tab, DetailTab::Stream)
          && stream_trace.is_some();

      // `wrapped` is only populated for paths that render a Paragraph.
      let mut wrapped: Vec<Line<'static>> = Vec::new();
      let visible_h = body_area.height as usize;
      let total: usize;

      if stream_with_trace {
          // ReactTrace owns the body; we do not wrap. `total` is not used
          // for the Stream label (trace_following is authoritative).
          total = 0;
      } else {
          let body_lines = match self.current_tab {
              DetailTab::Stream => {
                  // No trace materialized yet (orphan event or first-load race).
                  vec![Line::from(Span::styled(
                      "(no stream yet)",
                      Style::default().fg(Color::DarkGray),
                  ))]
              }
              DetailTab::Artifacts => self.render_artifacts(node),
              DetailTab::Attempts => self.render_attempts(node),
              DetailTab::Task => self.render_task(node),
              DetailTab::Review => self.render_review(node),
          };
          wrapped = body_lines
              .iter()
              .flat_map(|l| crate::components::line_wrap::wrap_line_to_width(l, body_area.width))
              .collect();
          total = wrapped.len();
      }

      // ── 4. Apply the scroll clamp + re-engage-following BEFORE
      //       deriving the label and rendering the block. This fixes
      //       the one-frame lag where the border used to show stale
      //       "not following" on the frame the user reached bottom. ─
      if !stream_with_trace {
          let max_offset = total.saturating_sub(visible_h);
          if self.is_following {
              self.scroll_offset = max_offset;
          } else {
              self.scroll_offset = self.scroll_offset.min(max_offset);
              if self.scroll_offset >= max_offset && max_offset > 0 {
                  self.is_following = true;
              }
          }
      }

      // ── 5. Derive the scroll label from final post-clamp state. ──
      let scroll_label_text = scroll_label(
          self.current_tab,
          total,
          visible_h,
          self.scroll_offset,
          self.is_following,
          trace_following,
      );

      // ── 6. Build the real block with all titles. ─────────────────
      let mut block = Block::default()
          .borders(Borders::ALL)
          .title(format!(" {} ", node.agent))
          .title_bottom(scroll_label_text.as_ref().to_string());
      if let Some(badge) = issue_badge {
          block = block
              .title_top(Line::from(format!(" {} ", badge)).alignment(Alignment::Right))
              .title_bottom(Line::from(" [I]ssue detail ").alignment(Alignment::Right));
      }
      frame.render_widget(block, area);

      // ── 7. Render tabs. ──────────────────────────────────────────
      let titles: Vec<Line> = DetailTab::all()
          .iter()
          .map(|t| {
              let style = if *t == self.current_tab {
                  Style::default()
                      .fg(Color::Cyan)
                      .add_modifier(Modifier::BOLD)
              } else {
                  Style::default().fg(Color::DarkGray)
              };
              Line::from(Span::styled(t.label(), style))
          })
          .collect();
      let tabs = Tabs::new(titles)
          .select(
              DetailTab::all()
                  .iter()
                  .position(|t| *t == self.current_tab)
                  .unwrap_or(0),
          )
          .divider("│");
      frame.render_widget(tabs, chunks[0]);

      // ── 8. Render body. ──────────────────────────────────────────
      if stream_with_trace {
          let trace = stream_trace.expect("stream_with_trace implies Some");
          trace.render_compact(frame, body_area);
          return;
      }
      let p = Paragraph::new(wrapped).scroll((self.scroll_offset as u16, 0));
      frame.render_widget(p, body_area);
  }
  ```

  Note the critical invariants this preserves:
  - The Stream-with-trace early-return (original L225-235) stays at the end so ReactTrace owns the body.
  - `render_compact` paints only the body (no outer block) — unchanged.
  - Pre-wrap fix (original L254-258) is preserved: `max_offset` is computed from `wrapped.len()`, not unwrapped.
  - Re-engage-following (original L272-274) now fires BEFORE the block renders.

- [ ] **Step 2.2: Compile check.**

  Run: `cargo check -p spur-tui`
  Expected: no errors, no warnings about unused `scroll_label` (it's now referenced).

- [ ] **Step 2.3: Run existing `detail_pane_scroll` integration tests.**

  Run: `cargo test -p spur-tui --test detail_pane_scroll`
  Expected: all pass — the render reorder must not regress anything the existing tests exercised.

  If any test fails, STOP. Do NOT modify the test to match new output. The render reorder must preserve existing observable behavior. Debug the regression.

- [ ] **Step 2.4: Run the full spur-tui test suite to catch any unexpected regression.**

  Run: `cargo test -p spur-tui`
  Expected: all pass.

- [ ] **Step 2.5: Commit.**

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs
  git commit -m "refactor(detail-pane): wire scroll_label + render reorder

  render() now computes wrapped metrics and the is_following re-engage
  BEFORE building the block. Uses a shape-equivalent skeleton block
  for Block::inner(). Fixes a latent one-frame UI lag where the
  follow badge lagged one frame behind the auto-re-engage."
  ```

---

## Task 3: Extract private `set_tab` helper; refactor `cycle_tab` to delegate

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs` — `cycle_tab` and new `set_tab` (around lines 74-99)

- [ ] **Step 3.1: Replace `cycle_tab` with a two-function form.**

  Find the current `cycle_tab` method (it starts around line 74 with the `/// Cycle to the next (or previous) tab.` doc comment and ends around line 99). Replace the entire method with:

  ```rust
  /// Private shared helper. Encodes the per-tab reset invariants so
  /// every entry point (`cycle_tab`, `jump_to_tab`) cannot accidentally
  /// diverge. Opens at top (`scroll_offset = 0`) on every tab; sets
  /// `is_following` based on the destination tab kind; snaps the
  /// Stream trace to bottom when landing on Stream.
  fn set_tab(
      &mut self,
      tab: DetailTab,
      stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
  ) {
      self.current_tab = tab;
      self.scroll_offset = 0;
      match tab {
          DetailTab::Stream => {
              self.is_following = true;
              if let Some(t) = stream_trace {
                  t.scroll_to_bottom();
              }
          }
          _ => {
              self.is_following = false;
          }
      }
  }

  /// Cycle to the next (or previous) tab.
  ///
  /// Per-tab reset invariants (`scroll_offset = 0`, `is_following`
  /// per tab kind) are centralised in [`DetailPane::set_tab`].
  pub fn cycle_tab(
      &mut self,
      forward: bool,
      stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
  ) {
      let all = DetailTab::all();
      let idx = all.iter().position(|t| *t == self.current_tab).unwrap_or(0);
      let next = if forward {
          (idx + 1) % all.len()
      } else {
          (idx + all.len() - 1) % all.len()
      };
      self.set_tab(all[next], stream_trace);
  }
  ```

- [ ] **Step 3.2: Compile check.**

  Run: `cargo check -p spur-tui`
  Expected: no errors.

- [ ] **Step 3.3: Run existing tab-cycling tests.**

  Run: `cargo test -p spur-tui --test detail_pane_scroll`
  Expected: all pass. If any fail, the set_tab extraction regressed behavior.

- [ ] **Step 3.4: Commit.**

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs
  git commit -m "refactor(detail-pane): extract private set_tab helper

  cycle_tab now delegates to set_tab. Prepares the ground for a
  public jump_to_tab that shares the same per-tab reset invariants."
  ```

---

## Task 4: Add public `jump_to_tab` + unit tests

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs` — add `jump_to_tab` and tests

- [ ] **Step 4.1: Write the failing integration tests first** (TDD). Append to the existing inline test module inside `detail_pane.rs`:

  Find `mod scroll_label_tests` (from Task 1) and immediately below it, add a new inline module:

  ```rust
  #[cfg(test)]
  mod jump_to_tab_tests {
      use super::*;

      #[test]
      fn jump_to_review_resets_scroll_and_follow() {
          let mut pane = DetailPane::new();
          // Simulate user having scrolled on a prior tab.
          pane.scroll_offset = 42;
          pane.is_following = true;
          pane.jump_to_tab(DetailTab::Review, None);
          assert_eq!(pane.current_tab, DetailTab::Review);
          assert_eq!(pane.scroll_offset, 0);
          assert!(!pane.is_following);
      }

      #[test]
      fn jump_to_artifacts_opens_at_top_without_following() {
          let mut pane = DetailPane::new();
          pane.scroll_offset = 100;
          pane.is_following = true;
          pane.jump_to_tab(DetailTab::Artifacts, None);
          assert_eq!(pane.current_tab, DetailTab::Artifacts);
          assert_eq!(pane.scroll_offset, 0);
          assert!(!pane.is_following);
      }

      #[test]
      fn jump_to_stream_engages_follow_and_resets_offset() {
          let mut pane = DetailPane::new();
          // Start on a non-Stream tab with a non-zero scroll offset.
          pane.current_tab = DetailTab::Artifacts;
          pane.scroll_offset = 42;
          pane.is_following = false;
          pane.jump_to_tab(DetailTab::Stream, None);
          assert_eq!(pane.current_tab, DetailTab::Stream);
          assert_eq!(pane.scroll_offset, 0);
          assert!(pane.is_following);
      }

      #[test]
      fn jump_is_idempotent_on_same_tab() {
          // Jumping to the tab you are already on still resets.
          let mut pane = DetailPane::new();
          pane.current_tab = DetailTab::Task;
          pane.scroll_offset = 99;
          pane.is_following = true;
          pane.jump_to_tab(DetailTab::Task, None);
          assert_eq!(pane.current_tab, DetailTab::Task);
          assert_eq!(pane.scroll_offset, 0);
          assert!(!pane.is_following);
      }
  }
  ```

- [ ] **Step 4.2: Run the tests to confirm they fail with "method not found: `jump_to_tab`".**

  Run: `cargo test -p spur-tui --lib jump_to_tab_tests`
  Expected: compile error — `jump_to_tab` does not exist yet.

- [ ] **Step 4.3: Add `jump_to_tab` to `impl DetailPane`.**

  Immediately below the `cycle_tab` method (which you edited in Task 3), add:

  ```rust
  /// Jump directly to a specific tab. Applies the same per-tab reset
  /// invariants as [`DetailPane::cycle_tab`] (scroll to top, set
  /// `is_following` per tab kind, snap Stream trace to bottom).
  ///
  /// Use this from outside the pane instead of writing `current_tab`
  /// directly — the field is `pub(crate)` and only readable via
  /// [`DetailPane::current_tab`].
  pub fn jump_to_tab(
      &mut self,
      tab: DetailTab,
      stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
  ) {
      self.set_tab(tab, stream_trace);
  }
  ```

- [ ] **Step 4.4: Run the tests again.**

  Run: `cargo test -p spur-tui --lib jump_to_tab_tests`
  Expected: **4 passed; 0 failed**.

- [ ] **Step 4.5: Commit.**

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs
  git commit -m "feat(detail-pane): add public jump_to_tab helper

  Encodes the per-tab reset invariants (scroll_offset=0,
  is_following per tab kind, Stream trace snap to bottom) via the
  shared private set_tab. Four unit tests cover the four destination
  tab kinds + idempotence on same-tab jump."
  ```

---

## Task 5: Migrate `app.rs:1609` to `jump_to_tab`

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1605-1614` (exact lines confirmed in pre-flight grep)

- [ ] **Step 5.1: Locate the current direct-mutation site.**

  Run: `grep -n "detail_pane_mut()\.current_tab" /Volumes/Projects/spur/crates/spur-tui/src/app.rs`

  Expected: one match at line 1609. If the line number differs from the plan's reference to 1609 (file may have shifted), use the actual line number — the match content is what matters.

- [ ] **Step 5.2: Read the surrounding block** (roughly lines 1605-1614) to understand the context:

  Run: `sed -n '1603,1614p' /Volumes/Projects/spur/crates/spur-tui/src/app.rs`

  Expected output shape (for your reference):
  ```
                  if let Some(id) = next {
                      self.dashboard
                          .agents_tree_mut()
                          .set_selected(Some(id.clone()));
                      self.dashboard.set_focused_node(Some(id));
                      self.dashboard.detail_pane_mut().current_tab =
                          crate::components::detail_pane::DetailTab::Review;
                  }
  ```

- [ ] **Step 5.3: Apply the migration.**

  Replace the `self.dashboard.detail_pane_mut().current_tab = crate::components::detail_pane::DetailTab::Review;` statement (which spans two lines because of line breaking) with a single call to `jump_to_tab`:

  ```rust
                  self.dashboard
                      .detail_pane_mut()
                      .jump_to_tab(crate::components::detail_pane::DetailTab::Review, None);
  ```

  Note the `None` argument: there is no `ReactTrace` to scroll here because we are landing on Review, not Stream. The parameter is an `Option` precisely so non-Stream callers pass `None`.

- [ ] **Step 5.4: Compile check.**

  Run: `cargo check -p spur-tui`
  Expected: no errors.

- [ ] **Step 5.5: Run any integration test that exercises the Review-jump action.**

  Run: `cargo test -p spur-tui --test review_submission`
  Expected: the `r`-jump path compiles and runs.

  If the existing `review_submission` test does its own direct mutation at `:108` and relies on that continuing to compile, the test may currently break. That is expected — Task 6 fixes the test.

- [ ] **Step 5.6: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs
  git commit -m "fix(app): route Review-jump through jump_to_tab

  Closes the invariant bypass at app.rs:1609 where current_tab was
  written directly, skipping the scroll_offset/is_following reset
  logic that cycle_tab (now set_tab) encodes."
  ```

---

## Task 6: Migrate the test-site direct mutation

**Files:**
- Modify: `crates/spur-tui/tests/review_submission.rs:108`

The per-invariant regression coverage (scroll_offset=0, is_following=false after jump to Review) is already in Task 4's inline `jump_to_review_resets_scroll_and_follow` unit test, which can access the private fields from the inline test module. This task just closes the last direct-write site so the migration is complete and Task 7's `pub(crate)` downgrade compiles.

- [ ] **Step 6.1: Locate the direct-mutation site.**

  Run: `grep -n "detail_pane_mut()\.current_tab" /Volumes/Projects/spur/crates/spur-tui/tests/review_submission.rs`

  Expected: one match at or near line 108.

- [ ] **Step 6.2: Apply the migration.**

  At the line (around :108) that reads:

  ```rust
      dashboard.detail_pane_mut().current_tab = DetailTab::Review;
  ```

  Replace with:

  ```rust
      dashboard.detail_pane_mut().jump_to_tab(DetailTab::Review, None);
  ```

  Note: `DetailTab` is already imported at the top of the file for the original line to have compiled. No new import needed.

- [ ] **Step 6.3: Compile check.**

  Run: `cargo check -p spur-tui --tests`
  Expected: no errors.

- [ ] **Step 6.4: Run the test to confirm it still exercises the Review-jump dispatch path.**

  Run: `cargo test -p spur-tui --test review_submission`
  Expected: all tests pass. The test's existing assertions on downstream state (submission payload, action routing, etc.) continue to work because `jump_to_tab(Review, None)` leaves `current_tab == Review` exactly as the direct write did, plus it resets `scroll_offset` and `is_following` — which is the intended behavior correction.

- [ ] **Step 6.5: Commit.**

  ```bash
  git add crates/spur-tui/tests/review_submission.rs
  git commit -m "test(detail-pane): migrate :108 direct write to jump_to_tab

  Closes the last direct-write site on detail_pane.current_tab so
  Task 7's pub(crate) downgrade compiles. Invariant coverage lives
  in the inline jump_to_tab_tests module."
  ```

---

## Task 7: Visibility hardening — `pub(crate) current_tab` + public getter

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs` — struct field + add getter
- Modify: `crates/spur-tui/tests/detail_pane_scroll.rs:87,160,199,234,241` — migrate field reads to getter calls

- [ ] **Step 7.1: Change the struct field visibility.**

  In `crates/spur-tui/src/components/detail_pane.rs`, find the `DetailPane` struct (around line 41):

  ```rust
  pub struct DetailPane {
      pub current_tab: DetailTab,
      scroll_offset: usize,
      is_following: bool,
  }
  ```

  Replace with:

  ```rust
  pub struct DetailPane {
      pub(crate) current_tab: DetailTab,
      scroll_offset: usize,
      is_following: bool,
  }
  ```

- [ ] **Step 7.2: Add the public getter inside `impl DetailPane`.**

  Near the top of `impl DetailPane` (right after `pub fn new() -> Self { … }`, around line 61), add:

  ```rust
  /// Read-only accessor for the current tab. External callers cannot
  /// write `current_tab` directly; use [`DetailPane::jump_to_tab`] or
  /// [`DetailPane::cycle_tab`] to change it.
  pub fn current_tab(&self) -> DetailTab {
      self.current_tab
  }
  ```

- [ ] **Step 7.3: Compile check.**

  Run: `cargo check -p spur-tui`
  Expected: no errors for lib target. Integration tests in `crates/spur-tui/tests/detail_pane_scroll.rs` will show FIVE `field current_tab of struct DetailPane is private` errors — fix in next step.

- [ ] **Step 7.4: Migrate the five read sites in `tests/detail_pane_scroll.rs`.**

  Run: `grep -n "pane\.current_tab" /Volumes/Projects/spur/crates/spur-tui/tests/detail_pane_scroll.rs`

  Expected: five matches at lines 87, 160, 199, 234, 241.

  Migrate each match:
  - `pane.current_tab` → `pane.current_tab()` (in `assert_eq!` / `assert_ne!` / `while` conditions)

  Apply these edits (exact line numbers may drift; match the content):

  - Line 87: `assert_eq!(pane.current_tab, DetailTab::Stream);` → `assert_eq!(pane.current_tab(), DetailTab::Stream);`
  - Line 160: `assert_eq!(pane.current_tab, DetailTab::Task);` → `assert_eq!(pane.current_tab(), DetailTab::Task);`
  - Line 199: `assert_eq!(pane.current_tab, DetailTab::Task);` → `assert_eq!(pane.current_tab(), DetailTab::Task);`
  - Line 234: `assert_ne!(pane.current_tab, DetailTab::Stream);` → `assert_ne!(pane.current_tab(), DetailTab::Stream);`
  - Line 241: `while pane.current_tab != DetailTab::Stream {` → `while pane.current_tab() != DetailTab::Stream {`

- [ ] **Step 7.5: Compile check.**

  Run: `cargo check -p spur-tui --tests`
  Expected: no errors.

- [ ] **Step 7.6: Run the test suite.**

  Run: `cargo test -p spur-tui`
  Expected: all pass.

- [ ] **Step 7.7: Commit.**

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs \
          crates/spur-tui/tests/detail_pane_scroll.rs
  git commit -m "refactor(detail-pane): lock down current_tab to pub(crate)

  Field drops to pub(crate); external reads now go through the
  current_tab() getter. This makes direct writes from outside the
  crate a compile error, structurally preventing future regressions
  of the kind fixed at app.rs:1609."
  ```

---

## Task 8: Regression pass — full tests + clippy + fmt

**Files:** none

- [ ] **Step 8.1: Run the full spur-tui test suite.**

  Run: `cargo test -p spur-tui`
  Expected: all pass, including new `scroll_label_tests`, `jump_to_tab_tests`, and `review_jump_resets_scroll_and_follow_invariants`.

- [ ] **Step 8.2: Run clippy with warnings as errors on the crate.**

  Run: `cargo clippy -p spur-tui --all-targets -- -D warnings`
  Expected: clean.

  If clippy complains about the new code, fix inline. Do not silence warnings with `#[allow(...)]` unless there's a genuine reason — fix the underlying issue.

- [ ] **Step 8.3: Run rustfmt on the touched files.**

  Run:
  ```bash
  cargo fmt -p spur-tui -- \
    crates/spur-tui/src/components/detail_pane.rs \
    crates/spur-tui/src/app.rs \
    crates/spur-tui/tests/review_submission.rs \
    crates/spur-tui/tests/detail_pane_scroll.rs
  ```
  Expected: no output (no diff).

  If fmt produces a diff, inspect to verify the changes are benign (whitespace only, not semantic), then commit:

  ```bash
  git add crates/spur-tui/src/components/detail_pane.rs \
          crates/spur-tui/src/app.rs \
          crates/spur-tui/tests/review_submission.rs \
          crates/spur-tui/tests/detail_pane_scroll.rs
  git commit -m "chore(detail-pane): rustfmt cleanup"
  ```

- [ ] **Step 8.4: Manual smoke check (interactive TUI).**

  Launch the TUI against a session with live stream and at least one non-empty Artifacts or Task tab. Verify:

  - Stream tab: `▼ following` visible at bottom-left.
  - Scroll up on Stream: badge changes to `▲ paused`.
  - Tab over to Artifacts: at top shows ` top ` on bottom-left.
  - Scroll down to middle: shows ` ▲ N ↑ ` where N is a two-to-three-digit number matching scroll_offset.
  - Scroll to bottom: ` end ` OR ` ▼ ` (if auto-re-engage fires).
  - Short content (e.g., Attempts with 1-2 lines): shows ` ▼ `.
  - Press `r` from any tab to jump to Review: Review opens at top (` top ` on bottom), `is_following` is false (no ` ▼ `).

  If any of these behave wrong, stop and debug. Do not mark the task done on visual smell alone.

---

## Task 9: Final spec-coverage review

- [ ] **Step 9.1: Open the spec and cross-check each section against a task.**

  Open: `docs/superpowers/specs/2026-04-20-detail-pane-scroll-indicator-design.md`

  For each section, identify the task step that implements it:

  | Spec section | Implementing task |
  |---|---|
  | Problems R1 / R1a / N1 | T2 (render reorder + scroll_label) / T2 (re-engage moved before render) / T5 (app.rs migration) + T6 (test-site migration) |
  | UX invariants I1-I5 | T2 (I1, I2, I4), T3+T4 (I3), T7 (I5) |
  | State-adaptive `scroll_label` (9 rows) | T1 (pure function) + T2 (wiring) |
  | Render reorder | T2 |
  | `set_tab` / `jump_to_tab` / `cycle_tab` | T3 (set_tab, cycle_tab refactor) + T4 (jump_to_tab) |
  | Visibility hardening (`pub(crate)` + getter) | T7 |
  | Call-site migration (`app.rs`, test) | T5 (app.rs), T6 (review_submission.rs), T7 (detail_pane_scroll.rs reads) |
  | Testing strategy (9 scroll_label + 4 jump_to_tab + existing review_submission test exercising migrated dispatch) | T1 (9), T4 (4), T6 (migration preserves existing test coverage) |
  | Non-goals | No task (correctly excluded) |

  If any spec section has no corresponding task, add a follow-up task step before merging.

- [ ] **Step 9.2: Verify branch is in good shape.**

  Run:
  ```bash
  git log --oneline $(git merge-base HEAD main)..HEAD
  ```
  Expected: a linear sequence of scoped commits (one per task, plus any review-fix commits), each with a meaningful message.

- [ ] **Step 9.3: Offer merge options.**

  Do NOT merge or open a PR automatically. Invoke `superpowers:finishing-a-development-branch` to present the user with structured options.

---

## Summary of deliverables

| Deliverable | Task |
|---|---|
| Pure `scroll_label(...)` function | Task 1 |
| 9 unit tests for `scroll_label` (full state matrix) | Task 1 |
| Render reorder + skeleton-block inner + one-frame-lag fix | Task 2 |
| Private `set_tab` helper | Task 3 |
| `cycle_tab` refactored to delegate | Task 3 |
| Public `jump_to_tab` + 4 unit tests | Task 4 |
| `app.rs:1609` migrated | Task 5 |
| `tests/review_submission.rs:108` migrated | Task 6 |
| `pub(crate) current_tab` + `current_tab()` getter | Task 7 |
| `tests/detail_pane_scroll.rs` 5 read sites migrated | Task 7 |
| Clippy + rustfmt clean; manual smoke verified | Task 8 |
| Spec coverage audit | Task 9 |
