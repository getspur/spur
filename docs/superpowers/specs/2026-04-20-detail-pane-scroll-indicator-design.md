# DetailPane Scroll Indicator + jump_to_tab Design

Date: 2026-04-20
Status: Approved (brainstorm + codex-acp review complete, ready for plan)
Area: `spur-tui` — `components/detail_pane.rs`, one site in `app.rs`, one test site

## Problems

**R1 — No scroll-position indicator on non-Stream tabs.** Current `following_indicator` at `detail_pane.rs:179` renders `" ▼ following "` when at bottom+following and `""` otherwise. When the user scrolls up, the bottom border goes blank — there is no textual cue that (a) the user is scrolled up, (b) how far above the viewport they are, or (c) where they are in the content. Confirmed by codex-acp first-principles review.

**R1a — One-frame UI lag (latent).** At `detail_pane.rs:191` the block is rendered with `frame.render_widget`, which writes immediately to the buffer. The `is_following` re-engage branch at L272-274 fires AFTER. On the frame where the user reaches the bottom, the border still shows "not following"; state transitions only for the next frame. Codex confirmed `Frame::render_widget` is synchronous to the buffer.

**N1 — Review-jump invariant bypass.** `app.rs:1562-1563` mutates `detail_pane.current_tab = DetailTab::Review` directly, skipping the `cycle_tab` reset logic. Review inherits whatever `scroll_offset` and `is_following` the previous tab had, instead of opening at top with `is_following = false`. The test at `tests/review_submission.rs:108` does the same. The root cause is that `current_tab` is `pub` — nothing structurally prevents direct writes.

## Goal

Replace the two-state `following_indicator` with a **state-adaptive `scroll_label`** that covers every pane state, extracted as a pure function that is unit-testable without a ratatui frame. Introduce a **shared `set_tab` helper** that encodes the per-tab reset invariants, used by both `cycle_tab` and a new **`jump_to_tab`**. Downgrade `current_tab` to `pub(crate)` to prevent future direct-write bugs.

## UX Invariants

- **I1 — Legible scroll state.** On every tab at every moment, the bottom border shows a label that tells the user whether they are at top, mid-scroll, at end, following, or paused.
- **I2 — Stream vs. non-Stream semantics are distinct.** Stream = tailing a live event stream. Non-Stream tabs = pinned to the bottom of static content. The label shape differs to match.
- **I3 — Tab state is reset on every transition.** Whether via `cycle_tab`, `jump_to_tab`, or any other entry point, landing on a new tab always resets `scroll_offset = 0` and sets `is_following` per the tab kind.
- **I4 — No one-frame lags.** The label rendered on frame N reflects the scroll state for that frame (including any auto-re-engage side effect of that frame's metrics).
- **I5 — Direct writes to `current_tab` from outside the crate are a compile error.** Enforced by visibility.

## Design

### 1. State-adaptive `scroll_label`

Pure function with no ratatui dependencies:

```rust
use std::borrow::Cow;

fn scroll_label(
    tab: DetailTab,
    total: usize,
    visible_h: usize,
    scroll_offset: usize,
    is_following: bool,
    stream_trace_following: Option<bool>,
) -> Cow<'static, str> {
    // implementation per the state table below
}
```

**State table** (exhaustive):

| Tab | Trace | `total` | `max_offset` | `is_following` / `trace_following` | Scroll position | Label |
|---|---|---|---|---|---|---|
| Stream | present | any | any | trace_following = true | — | `" ▼ following "` |
| Stream | present | any | any | trace_following = false | — | `" ▲ paused "` |
| Stream | absent (placeholder) | any | any | — | — | `" ▼ following "` |
| Non-Stream | — | 0 | 0 | — | — | `""` |
| Non-Stream | — | > 0 | 0 (fits) | — | — | `" ▼ "` |
| Non-Stream | — | > 0 | > 0 | is_following = true | offset = max_offset | `" ▼ "` |
| Non-Stream | — | > 0 | > 0 | is_following = false | offset = 0 | `" top "` |
| Non-Stream | — | > 0 | > 0 | is_following = false | offset = max_offset | `" end "` |
| Non-Stream | — | > 0 | > 0 | is_following = false | 0 < offset < max_offset | `" ▲ {offset} ↑ "` |

`max_offset` is derived as `total.saturating_sub(visible_h)`; the caller passes `total` and `visible_h` already computed.

### 2. Render reorder

Current `render()` sequence (detail_pane.rs:155-280):

1. compute `following`
2. build `block` with titles
3. `block.inner(area)` + `frame.render_widget(block, area)`
4. split inner → tab row + body
5. render tabs
6. Stream-tab early return: `trace.render_compact(body_area); return;`
7. compute `body_lines`, pre-wrap, `max_offset`, clamp, re-engage following
8. render body `Paragraph`

New sequence:

1. compute `following` (Stream-tab helper for trace state)
2. **Build a skeleton block** with `Borders::ALL` and placeholder titles matching the final block's *title positions* (one top-left, optionally top-right, one bottom-left, optionally bottom-right). Values can be `" "` — what matters is the count + position + alignment so that `Block::inner` returns the same rect as the final block.
3. `inner = skeleton.inner(area)`; split into tab + body chunks
4. For Stream-with-trace: we don't need wrapping; go straight to step 7. For placeholder Stream or non-Stream: compute `body_lines`, pre-wrap, `total = wrapped.len()`, `max_offset = total.saturating_sub(body_area.height as usize)`.
5. Apply scroll clamp + re-engage-following logic (moves up from L266-275)
6. Derive `scroll_label = scroll_label(tab, total, visible_h, scroll_offset, is_following, trace_following)` — NOW `is_following` already reflects the re-engage side effect
7. Build the **real block** with final titles (including `scroll_label` on bottom-left) and render it
8. Render tabs
9. For Stream-with-trace: `trace.render_compact(body_area)` and return
10. Otherwise: render `Paragraph::new(wrapped).scroll(...)` into body_area

This eliminates the one-frame lag: the scroll label derives from the post-clamp, post-re-engage state, *then* the block is rendered.

**Why a skeleton block rather than computing inner from borders alone:** `Block::inner` reserves border cells AND title rows. `title_bottom("")` still occupies a title slot even if the text is empty. The skeleton must match the *shape* of the final block — same borders, same number of title positions — so that `inner` returns the same rect.

### 3. `set_tab` / `jump_to_tab` / `cycle_tab`

```rust
impl DetailPane {
    /// Private shared helper. Encodes the per-tab reset invariants so every
    /// entry point (cycle_tab, jump_to_tab) cannot accidentally diverge.
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

    /// Cycle to the next/previous tab. Public API unchanged.
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

    /// Jump directly to a specific tab, applying the same per-tab reset
    /// invariants as cycle_tab. Use this from outside the pane; direct
    /// writes to `current_tab` are disallowed by visibility.
    pub fn jump_to_tab(
        &mut self,
        tab: DetailTab,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
        self.set_tab(tab, stream_trace);
    }
}
```

### 4. Visibility hardening

Downgrade `current_tab` from `pub` to `pub(crate)` at `detail_pane.rs:41`. Add a read-only getter if any caller outside the module needs to read it:

```rust
pub struct DetailPane {
    pub(crate) current_tab: DetailTab,
    // ...
}

impl DetailPane {
    pub fn current_tab(&self) -> DetailTab { self.current_tab }
}
```

Direct writes from outside the crate become compile errors. Inside the crate, `app.rs` and tests must use `jump_to_tab`.

### 5. Call-site migration

| Site | Before | After |
|---|---|---|
| `app.rs:1562-1563` | `self.dashboard.detail_pane_mut().current_tab = DetailTab::Review;` | `self.dashboard.detail_pane_mut().jump_to_tab(DetailTab::Review, None);` |
| `tests/review_submission.rs:108` | (direct mutation — verify with grep at plan time) | `detail_pane.jump_to_tab(DetailTab::Review, None);` |

### 6. Testing strategy

**Unit tests for `scroll_label`** — exhaustive state-table coverage, one test per row (9 tests). Pure function, no ratatui frame needed:

```rust
#[test]
fn stream_with_trace_following() {
    assert_eq!(
        scroll_label(DetailTab::Stream, 0, 0, 0, false, Some(true)),
        Cow::Borrowed(" ▼ following ")
    );
}

#[test]
fn non_stream_mid_scroll_shows_count() {
    assert_eq!(
        scroll_label(DetailTab::Artifacts, 100, 20, 42, false, None),
        Cow::<'static, str>::Owned(" ▲ 42 ↑ ".to_string())
    );
}
// ... 7 more for the remaining rows
```

**Unit tests for `jump_to_tab`** — verify per-tab invariants:

```rust
#[test]
fn jump_to_review_resets_scroll_and_follow() {
    let mut pane = DetailPane::new();
    pane.scroll_offset = 42;      // test hook: crate-private write
    pane.is_following = true;
    pane.jump_to_tab(DetailTab::Review, None);
    assert_eq!(pane.current_tab(), DetailTab::Review);
    assert_eq!(pane.scroll_offset, 0);
    assert!(!pane.is_following);
}

#[test]
fn jump_to_stream_engages_follow_and_scrolls_trace_to_bottom() {
    let mut pane = DetailPane::new();
    pane.current_tab = DetailTab::Artifacts;
    pane.scroll_offset = 42;
    let mut trace = /* build a ReactTrace */;
    pane.jump_to_tab(DetailTab::Stream, Some(&mut trace));
    assert!(pane.is_following);
    assert_eq!(pane.scroll_offset, 0);
    // trace.is_following() → true (verified via its existing API)
}
```

**Integration test** — after Review-jump, the fresh-tab invariants hold:

```rust
// tests/review_submission.rs (or similar)
#[test]
fn r_key_jump_to_review_opens_at_top() {
    // setup a session with Review content and scroll-down on Artifacts
    // press `r` → dispatch Action handled at app.rs:1562
    // assert current_tab == Review, scroll_offset == 0, is_following == false
}
```

### 7. Non-goals

- Scrollbar widget on right edge. MCTS rejected (J4 Stream/ReactTrace collision).
- Wrap-result cache for non-Stream tabs (audit's R2). Separate follow-up.
- Narrow-terminal tab slicing (audit's minor #3). Separate follow-up.
- Focus/blur affordance for DetailPane (codex's N2). Separate follow-up.
- Refining the "▼ following" semantic overload (audit's #4). Codex argued both badges mean "stick to newest content"; accept that framing.
- Changes to `ReactTrace::render_compact`, `is_following`, or any other react_trace internals.

## Rationale vs. rejected alternatives

- **Option B (scrollbar widget)** — rejected. `ReactTrace::render_compact` owns `body_area` on the Stream tab; a scrollbar painting inside body collides with that ownership, forcing either an asymmetric UI (no scrollbar on Stream) or body-width shrink for all tabs. MCTS scored 52/70 vs Option A's 66/70.
- **Option C (both scrollbar + text)** — rejected. Inherits B's collision plus adds redundancy on short-content states (J1).
- **Loop `cycle_tab` to reach Review** — rejected during codex review. Would visit intermediate tabs and spuriously call `trace.scroll_to_bottom()` on the Stream intermediate hop.
- **Embed `is_following` re-engage inline with block build** — rejected because it tangles state mutation with rendering. The reorder approach cleanly separates "compute state" → "render state".
- **Keep `current_tab` `pub`** — rejected. Nothing else enforces I5. `pub(crate)` is the minimal hardening that preserves internal-crate access (tests, dashboard) while blocking external direct writes.

## Compatibility

- Public API additions: `DetailPane::jump_to_tab`, `DetailPane::current_tab()` getter.
- Public API changes: `DetailPane::current_tab` field becomes `pub(crate)`. Any external consumer of the field becomes a compile error. Codex's grep found none outside the crate; in-crate consumers (app.rs, tests) migrate to `jump_to_tab` or the getter.
- No behavioral changes on the Stream tab live path — `trace.is_following()` still drives the Stream-tab label.
