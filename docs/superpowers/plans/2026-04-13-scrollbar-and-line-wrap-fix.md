# Scrollbar visibility + row-exact line wrap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the TUI session-trace scrollbar so the thumb reflects the true proportion of hidden content, and fix row-exact scrolling so no tail content is unreachable under auto-follow.

**Architecture:** Two localized changes in `crates/spur-tui`. (1) Add a pre-wrap helper `wrap_line_to_width` in a new module `line_wrap.rs` that splits `ratatui::text::Line`s at word boundaries while preserving span styles and unicode-width correctness. (2) In `react_trace.rs`, pre-wrap the generated `Vec<Line>` before rendering, remove ratatui's `.wrap()` (so Paragraph scroll becomes row-exact), and set `.viewport_content_length(visible_height)` on the `ScrollbarState` so the thumb is proportional.

**Tech Stack:** Rust, ratatui 0.29, `unicode-width` (new direct dep).

**Spec:** [`docs/superpowers/specs/2026-04-13-scrollbar-and-line-wrap-fix-design.md`](../specs/2026-04-13-scrollbar-and-line-wrap-fix-design.md)

---

## File Map

| File | Change | Responsibility |
|---|---|---|
| `crates/spur-tui/Cargo.toml` | Modify | Add `unicode-width` direct dep |
| `crates/spur-tui/src/components/line_wrap.rs` | Create | Owns the `wrap_line_to_width` helper + its unit tests |
| `crates/spur-tui/src/components/mod.rs` | Modify | Register `pub mod line_wrap;` |
| `crates/spur-tui/src/components/react_trace.rs` | Modify | Consume the helper in `render()`; drop `.wrap(Wrap {..})`; add `viewport_content_length` to scrollbar |

Four tasks, each with its own commit. `line_wrap.rs` ships with comprehensive unit tests that form the acceptance gate for the helper's correctness (the user-facing symptom is visual — unit tests are the reliable bar).

---

### Task 1: Add `unicode-width` dependency

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`

`unicode-width` is already transitive via `ratatui`, but importing it directly pins the version and makes its use explicit.

- [ ] **Step 1: Edit Cargo.toml**

Open `/Volumes/Projects/spur/crates/spur-tui/Cargo.toml`. Under `[dependencies]`, add a line for `unicode-width` immediately after the `tracing` line so deps stay roughly alphabetical/grouped. The file currently ends its `[dependencies]` block at `tracing = { workspace = true }`. Change:

```toml
tracing = { workspace = true }
```

to:

```toml
tracing = { workspace = true }
unicode-width = "0.1"
```

- [ ] **Step 2: Build**

Run: `cd /Volumes/Projects/spur && cargo build -p spur-tui`
Expected: compiles cleanly. Cargo may download `unicode-width` the first time — that's fine.

- [ ] **Step 3: Commit**

```bash
cd /Volumes/Projects/spur
git add crates/spur-tui/Cargo.toml crates/spur-tui/Cargo.lock 2>/dev/null; git add Cargo.lock 2>/dev/null
git commit -m "$(cat <<'EOF'
deps(spur-tui): add direct unicode-width dependency

Already available transitively through ratatui. Adding it as a direct
dep pins the version and makes its use explicit for the upcoming
line_wrap helper, which needs per-character display-width measurement
for correct word-wrap across emoji and wide CJK characters.
EOF
)"
```

> **Note:** `Cargo.lock` may be at the workspace root. `git add Cargo.lock` from the repo root covers either case; the `|| true` fallback avoids failing if the file didn't change.

---

### Task 2: Create `line_wrap` module with helper and unit tests

**Files:**
- Create: `crates/spur-tui/src/components/line_wrap.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

The helper is the core of Fix C. It takes a single `Line` plus a target width and returns one or more `Line`s, each with width ≤ target, such that concatenation reproduces the source content. Word-break at whitespace when possible; char-break when a single word exceeds width. Styles preserved across splits.

- [ ] **Step 1: Register the new module**

Open `/Volumes/Projects/spur/crates/spur-tui/src/components/mod.rs`. Current content:

```rust
pub mod activity_log;
pub mod agents_tree;
pub mod help_overlay;
pub mod input_bar;
pub mod react_trace;
pub mod status_bar;
```

Insert `pub mod line_wrap;` in alphabetical order (between `input_bar` and `react_trace`):

```rust
pub mod activity_log;
pub mod agents_tree;
pub mod help_overlay;
pub mod input_bar;
pub mod line_wrap;
pub mod react_trace;
pub mod status_bar;
```

- [ ] **Step 2: Create `line_wrap.rs` with the full helper and tests**

Create the file at `/Volumes/Projects/spur/crates/spur-tui/src/components/line_wrap.rs` with this exact content:

```rust
//! Row-exact line wrapping for the session trace.
//!
//! `wrap_line_to_width` splits a single `ratatui::text::Line` at word
//! boundaries such that each returned `Line` has display width `<= width`.
//! Span styles are preserved across splits (a mid-span break produces two
//! spans with the same `Style`). Whitespace at a break point is dropped
//! from the end of the first output line; leading whitespace on the source
//! line is preserved.
//!
//! This exists because `ratatui::widgets::Paragraph` with `.wrap(...)` uses
//! word-wrap internally, but `Paragraph::scroll((y, _))` counts visual rows
//! post-wrap — and consumers cannot ask how many rows the Paragraph will
//! produce. To make scroll and scrollbar state row-exact, we pre-wrap here
//! and render the Paragraph without its own wrap.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Wrap a single `Line` to the given display width.
///
/// Returns one or more `Line`s each with `width() <= width`. Concatenating
/// the returned lines reproduces the source content (text order and span
/// styles preserved).
///
/// - If the source already fits in `width`, returns `vec![line.clone()]`.
/// - If `width == 0`, returns `vec![line.clone()]` (degenerate — caller
///   must ensure width > 0 for correct behavior).
pub fn wrap_line_to_width(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line_to_owned(line)];
    }

    // Flatten to (Style, char) pairs so we can walk character-by-character
    // with stable per-char styles.
    let flat: Vec<(Style, char)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |c| (style, c))
        })
        .collect();

    // Early-out for trivial cases.
    if flat.is_empty() {
        return vec![Line::from("")];
    }
    let total_width: u16 = flat
        .iter()
        .map(|(_, c)| char_width(*c))
        .sum::<u32>()
        .min(u32::from(u16::MAX)) as u16;
    if total_width <= width {
        return vec![line_to_owned(line)];
    }

    // Greedy word-wrap:
    //   - `cur_start` = index into `flat` where the current output line begins.
    //   - `i` = current cursor.
    //   - `last_ws_end_exclusive` = first index AFTER the last whitespace run
    //     since `cur_start` (i.e., where the continuation line would begin
    //     if we break there). None if no whitespace was seen yet.
    //   - `cur_width` = display width of `flat[cur_start..i]`.
    //
    // When appending `flat[i]` would overflow `width`:
    //   - If `last_ws_end_exclusive` is Some(k), emit `flat[cur_start..k]`
    //     with trailing whitespace trimmed (we use the last non-whitespace
    //     boundary for emission), and restart from `k`.
    //   - Else (no whitespace seen), emit `flat[cur_start..i]` as-is
    //     (char-break fallback) and restart from `i`.
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur_start: usize = 0;
    let mut i: usize = 0;
    let mut cur_width: u16 = 0;
    // The index one past the last NON-whitespace char that we'd keep in the
    // emitted line if we broke at the most recent whitespace run.
    let mut break_end_exclusive: Option<usize> = None;
    // The index where the next line would start after the most recent
    // whitespace run (first non-ws char after the run).
    let mut break_continuation_start: Option<usize> = None;
    // Track whether we're currently inside a whitespace run.
    let mut in_ws: bool = false;
    // Track, during a whitespace run, the index just before the run started —
    // used to set `break_end_exclusive` when we transition ws->non-ws.
    let mut ws_run_pre_start: usize = 0;

    while i < flat.len() {
        let (_, c) = flat[i];
        let cw = char_width(c) as u16;
        let is_ws = is_wrap_whitespace(c);

        // Detect whitespace-run transitions BEFORE committing `c`.
        if is_ws && !in_ws {
            ws_run_pre_start = i;
            in_ws = true;
        } else if !is_ws && in_ws {
            // Ended the whitespace run at `i` — record the break points:
            // emit line up through ws_run_pre_start (exclusive), continue from i.
            break_end_exclusive = Some(ws_run_pre_start);
            break_continuation_start = Some(i);
            in_ws = false;
        }

        // Can we fit this character?
        if cur_width.saturating_add(cw) > width && i > cur_start {
            // Must break.
            let (emit_end, next_start) = match (break_end_exclusive, break_continuation_start) {
                (Some(end), Some(cont)) if end > cur_start && cont > cur_start => (end, cont),
                _ => {
                    // No usable word break since `cur_start`. Char-break fallback.
                    (i, i)
                }
            };
            out.push(build_line(&flat[cur_start..emit_end]));
            cur_start = next_start;
            cur_width = flat[cur_start..i]
                .iter()
                .map(|(_, c)| char_width(*c))
                .sum::<u32>()
                .min(u32::from(u16::MAX)) as u16;
            // Invalidate stale break markers — they belong to the previous line.
            break_end_exclusive = None;
            break_continuation_start = None;
            in_ws = false;
            // Do not advance `i`; re-evaluate `c` at the new cur_start.
            continue;
        }

        cur_width = cur_width.saturating_add(cw);
        i += 1;
    }

    // Flush remainder.
    if cur_start < flat.len() {
        out.push(build_line(&flat[cur_start..]));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }

    out
}

/// Display width of a single char. Control chars count as 0; width-unknown
/// chars default to 1 to avoid undercount.
fn char_width(c: char) -> u32 {
    UnicodeWidthChar::width(c).unwrap_or(1) as u32
}

/// A character is a "wrap whitespace" if it's an ASCII space or tab. We
/// intentionally exclude other whitespace categories (e.g. non-breaking
/// space) to keep behavior predictable; newlines shouldn't appear in Line
/// input because callers split on '\n' upstream.
fn is_wrap_whitespace(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Build a `Line<'static>` from a slice of (Style, char) pairs, merging
/// consecutive chars with the same Style into one Span.
fn build_line(chars: &[(Style, char)]) -> Line<'static> {
    if chars.is_empty() {
        return Line::from("");
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_style: Style = chars[0].0;
    let mut cur_buf = String::new();
    for (style, c) in chars {
        if *style == cur_style {
            cur_buf.push(*c);
        } else {
            if !cur_buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut cur_buf), cur_style));
            }
            cur_style = *style;
            cur_buf.push(*c);
        }
    }
    if !cur_buf.is_empty() {
        spans.push(Span::styled(cur_buf, cur_style));
    }
    Line::from(spans)
}

/// Clone a borrowed `Line` into a `'static`-lifetimed owned `Line`. Needed
/// because the wrap helper returns owned Lines but input may borrow.
fn line_to_owned(line: &Line<'_>) -> Line<'static> {
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.to_string(), s.style))
        .collect();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn w(line: &Line<'_>) -> u16 {
        line.width() as u16
    }

    fn s(text: &str) -> Line<'static> {
        Line::from(text.to_string())
    }

    #[test]
    fn width_zero_returns_clone() {
        let line = s("hello world");
        let out = wrap_line_to_width(&line, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].width(), 11);
    }

    #[test]
    fn empty_line_returns_single_empty() {
        let line = Line::from("");
        let out = wrap_line_to_width(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(w(&out[0]), 0);
    }

    #[test]
    fn fits_within_width_returns_clone() {
        let line = s("hello");
        let out = wrap_line_to_width(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(w(&out[0]), 5);
    }

    #[test]
    fn plain_word_break_at_whitespace() {
        // "hello world" width=11. Target width=5. Break at space.
        let line = s("hello world");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to_string(), "hello");
        assert_eq!(out[1].to_string(), "world");
        assert!(w(&out[0]) <= 5);
        assert!(w(&out[1]) <= 5);
    }

    #[test]
    fn multiple_breaks() {
        // Three 5-char words forced to wrap at width=5.
        let line = s("aaaaa bbbbb ccccc");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].to_string(), "aaaaa");
        assert_eq!(out[1].to_string(), "bbbbb");
        assert_eq!(out[2].to_string(), "ccccc");
    }

    #[test]
    fn long_word_char_break_fallback() {
        // One 15-char word at width=5 → char-break into 3 pieces.
        let line = s("abcdefghijklmno");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 3);
        for part in &out {
            assert!(w(part) <= 5, "part {:?} exceeds width", part.to_string());
        }
        // Concatenation reproduces original.
        let joined: String = out.iter().map(|l| l.to_string()).collect();
        assert_eq!(joined, "abcdefghijklmno");
    }

    #[test]
    fn multi_span_preserved_across_break() {
        // Two styled spans; break falls inside the second.
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let line = Line::from(vec![
            Span::styled("red_", red),
            Span::styled("blue words here", blue),
        ]);
        // width=7 forces break within the blue span.
        let out = wrap_line_to_width(&line, 7);
        assert!(out.len() >= 2);
        for part in &out {
            assert!(w(part) <= 7);
        }
        // First line must start with the "red_" span in Red style.
        let first_spans = &out[0].spans;
        assert_eq!(first_spans[0].content, "red_");
        assert_eq!(first_spans[0].style, red);
        // All blue characters in the output must carry the Blue style.
        let mut found_blue = false;
        for part in &out {
            for span in &part.spans {
                if span.content.chars().any(|c| c == 'b' || c == 'w' || c == 'h') {
                    assert_eq!(span.style, blue);
                    found_blue = true;
                }
            }
        }
        assert!(found_blue);
    }

    #[test]
    fn wide_emoji_accounted() {
        // "✉" is width 1 or 2 depending on Unicode tables; treat any wide
        // char as occupying up to 2 columns. Pick an ideograph which is
        // definitely width 2: "字".
        // Line is "字字字字字" (5 ideographs → 10 columns). Width=5 should
        // split every 2 chars (pair of 2-col chars exceeds 5 by 4+2=... let's
        // compute: width 5 fits 2 ideographs (2+2=4, +1 more=6 exceeds) so
        // split into pieces of 2 ideographs, last is 1.
        let line = s("字字字字字");
        let out = wrap_line_to_width(&line, 5);
        for part in &out {
            assert!(w(part) <= 5, "part {:?} has width {}", part.to_string(), w(part));
        }
        // Joined string must reproduce source.
        let joined: String = out.iter().map(|l| l.to_string()).collect();
        assert_eq!(joined, "字字字字字");
    }

    #[test]
    fn leading_whitespace_preserved_on_first_line() {
        // First line preserves its leading indent; only trailing whitespace
        // at a break is dropped. "   hello world" width=5: first emits
        // "   he" (char-break since the first 3 chars are the indent and
        // there's no word-break yet inside the first 5 columns)...
        // Actually, with greedy word-wrap: we see space, space, space, h, e,
        // l, l, o, space, w, o, r, l, d. The first whitespace run is chars
        // 0..=2. Break-end-exclusive = 0, continuation-start = 3. At i=5
        // (l), cur_width = 5, would overflow. We break: emit chars[0..0]
        // (empty) and restart from 3. Now cur_start=3, cur_width reset,
        // i=3 (h). Walk hello → fits until i=8 (o). Then i=9 (space).
        // is_ws=true. Continue. Width still 5. i=10 (w). in_ws→false,
        // set break-end=9, cont=10. cur_width=5+1? wait space is ws too,
        // cw=1... Let me just assert the invariant and reasonable output.
        let line = s("   hello world");
        let out = wrap_line_to_width(&line, 10);
        // width=10 fits "   hello" (8 chars) and " world" (6) would exceed
        // 8+6=14? No, continuation starts at 'w' so next line is "world".
        // Actually: "   hello world" width=14 total, width=10 target.
        // Walk: "   hello" fits (width 8). At space (i=8), in_ws starts.
        // At 'w' (i=9), in_ws ends → break-end=8, cont=9. At 'd' (i=13),
        // cur_width=8+1(space)+5(world)=14>10. Emit flat[0..8]="   hello",
        // continue from 9. So out = ["   hello", "world"].
        for part in &out {
            assert!(w(part) <= 10);
        }
        assert!(out[0].to_string().starts_with("   "));
        assert!(out.iter().any(|l| l.to_string() == "world"));
    }

    #[test]
    fn trailing_whitespace_dropped_at_break() {
        // "aaa  bbb" (two spaces in the middle). width=5 → "aaa" + "bbb".
        // The two spaces should NOT appear at the end of "aaa".
        let line = s("aaa  bbb");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to_string(), "aaa");
        assert_eq!(out[1].to_string(), "bbb");
    }

    #[test]
    fn invariant_all_outputs_within_width() {
        // Property-style: many inputs, various widths, assert invariant.
        let cases: Vec<(&str, u16)> = vec![
            ("", 10),
            ("short", 10),
            ("one two three four five", 7),
            ("one two three four five", 10),
            ("one two three four five", 100),
            ("abcdefghijklmnopqrstuvwxyz", 5),
            ("   indented text with   weird  spacing", 10),
            ("a b c d e f g h i j k l m n o p", 3),
            ("字字字 字字字字 字", 4),
        ];
        for (input, width) in cases {
            let line = s(input);
            let out = wrap_line_to_width(&line, width);
            for part in &out {
                assert!(
                    w(part) <= width,
                    "input {:?} width {}: output {:?} has width {}",
                    input,
                    width,
                    part.to_string(),
                    w(part)
                );
            }
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-tui --lib line_wrap -- --nocapture`
Expected: all 11 tests pass. If any fail, read the diagnostic, adjust the helper, re-run. Do not proceed to Task 3 until the test suite is green.

- [ ] **Step 4: Build**

Run: `cd /Volumes/Projects/spur && cargo build -p spur-tui`
Expected: clean build, no new warnings.

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Projects/spur
git add crates/spur-tui/src/components/mod.rs crates/spur-tui/src/components/line_wrap.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): add line_wrap helper for row-exact pre-wrapping

Introduces wrap_line_to_width(line, width) which splits a ratatui Line
at word boundaries into one or more Lines each with display width <=
width. Whitespace at break points is dropped; leading whitespace on
the source line is preserved. Word-break fall back to char-break for
words longer than width. Span styles preserved across splits — a
mid-span break produces two spans with the same Style. Display width
uses unicode_width::UnicodeWidthChar so emoji and CJK ideographs
account correctly.

Ships with 11 unit tests covering: width=0, empty line, fits,
multi-word break, multiple breaks, long-word char-break fallback,
multi-span preservation, wide-char accounting, leading whitespace
preserved, trailing whitespace dropped, and an all-outputs-within-width
invariant across varied inputs.

Preparation for the scrollbar/line-wrap fix in react_trace.rs — the
Paragraph there will use this helper to pre-wrap before rendering so
scroll becomes row-exact.
EOF
)"
```

---

### Task 3: Apply Fix A + Fix C in `react_trace.rs`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs` (`render()` method, lines ~482–521)

This replaces the char-wrap estimate with a row-exact pre-wrap and removes `Paragraph`'s own wrap so scroll is exact. Also sets `viewport_content_length` on the `ScrollbarState` so the thumb is proportional.

- [ ] **Step 1: Remove `Wrap` import**

In `/Volumes/Projects/spur/crates/spur-tui/src/components/react_trace.rs`, find the ratatui import block at the top:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};
```

Remove `Wrap` from the `widgets::{...}` list (it's no longer used):

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};
```

- [ ] **Step 2: Add import of the wrap helper**

Below the ratatui import, add (or extend an existing `use super::` line):

```rust
use super::line_wrap::wrap_line_to_width;
```

If a `use super::...` line already exists (for `MAX_LOG_ENTRIES`), place the new line directly below it.

- [ ] **Step 3: Replace the metrics + render block**

Locate the existing block at the bottom of `render()`, approximately lines 482–521:

```rust
        // Compute rendered-line metrics for accurate scrolling.
        let inner = block.inner(area);
        let effective_width = inner.width;
        let visible_height = inner.height as usize;
        let total_lines: usize = lines
            .iter()
            .map(|line| {
                let w = line.width() as u16;
                if w == 0 || effective_width == 0 {
                    1usize
                } else {
                    ((w + effective_width - 1) / effective_width) as usize
                }
            })
            .sum();

        self.last_total_lines.set(total_lines);
        self.last_visible_height.set(visible_height);

        // Clamp or pin scroll offset.
        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((offset as u16, 0));

        frame.render_widget(paragraph, area);

        // Scrollbar for position indicator.
        if total_lines > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total_lines).position(offset);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
```

Replace with this version:

```rust
        // Pre-wrap every built Line to the inner width so the Paragraph
        // renders row-exact and scroll offsets are exact visual rows.
        let inner = block.inner(area);
        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        let wrapped: Vec<Line> = lines
            .into_iter()
            .flat_map(|l| wrap_line_to_width(&l, effective_width))
            .collect();

        let total_lines = wrapped.len();
        self.last_total_lines.set(total_lines);
        self.last_visible_height.set(visible_height);

        // Clamp or pin scroll offset.
        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };

        // Paragraph renders each pre-wrapped Line as one visual row. No
        // `.wrap()` — we already sized every Line to `effective_width`.
        let paragraph = Paragraph::new(wrapped)
            .block(block)
            .scroll((offset as u16, 0));

        frame.render_widget(paragraph, area);

        // Scrollbar: proportional thumb via viewport_content_length.
        if total_lines > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .position(offset)
                .viewport_content_length(visible_height);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
```

- [ ] **Step 4: Build**

Run: `cd /Volumes/Projects/spur && cargo build -p spur-tui`
Expected: clean build, no warnings. The `Wrap` import removal must be complete — if a stray `Wrap { trim: false }` reference remains, the build will fail with "cannot find Wrap in scope."

- [ ] **Step 5: Run unit tests (ensure nothing regressed)**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-tui --lib`
Expected: all tests pass, including the 11 from Task 2.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/Projects/spur
git add crates/spur-tui/src/components/react_trace.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): row-exact session trace scrolling + proportional scrollbar

Two compounding bugs fixed in the trace panel rendering:

1. ScrollbarState was built via ScrollbarState::new(total_lines).position(offset)
   without a viewport_content_length. ratatui renders the thumb at its
   minimum size when viewport_content_length = 0, so the thumb conveyed
   no information about the proportion of hidden content. Users saw a
   tiny dot regardless of how much scrolled content was hidden. Now set
   viewport_content_length = visible_height so the thumb is proportional.

2. total_lines was approximated by ceil(line_width / effective_width) per
   logical line (character-wrap), but Paragraph::wrap(Wrap{..}) does
   word-wrap internally, and its row count can diverge from ours in
   either direction. The consequence: max_offset was wrong, scroll_offset
   under follow pinned to our wrong value, and if ratatui produced more
   visual rows than we counted, the tail of the content was permanently
   unreachable — matching the reported "many lines hidden" symptom.

   Fix: pre-wrap every built Line through line_wrap::wrap_line_to_width
   (added in the previous commit) and render Paragraph without its own
   .wrap(). Each pre-wrapped Line already fits effective_width, so the
   Paragraph renders one visual row per Line and scroll operates on
   exact rows. total_lines = wrapped.len() is now the exact count; the
   tail is always reachable under follow.

Removes the now-unused ratatui::widgets::Wrap import.
EOF
)"
```

---

### Task 4: Full workspace build + unit tests + manual smoke

**Files:** none (verification only)

Acceptance gate for the spec.

- [ ] **Step 1: Full workspace build**

Run: `cd /Volumes/Projects/spur && cargo build --workspace 2>&1 | tail -5`
Expected: `Finished dev profile [unoptimized + debuginfo] target(s) in …` with no errors and no new warnings.

- [ ] **Step 2: Full workspace test run**

Run: `cd /Volumes/Projects/spur && cargo test --workspace 2>&1 | tail -20`
Expected: all tests pass. In particular, the 11 tests in `components::line_wrap::tests` should run and pass. Read the tail for any FAIL line.

- [ ] **Step 3: Manual smoke test**

Run in a terminal:

```bash
cd /Volumes/Projects/spur
SPUR_LOG=debug cargo run -p spur-cli -- run 2> /tmp/spur-wrapfix.log
```

In the TUI:
1. Send a prompt to the kiro brain that produces a reasonably long markdown response, e.g. *"List 15 ideas for using DuckDB in real applications, with 2-3 sentences each."*
2. Wait for the full response to render in the session detail view.
3. Observe the scrollbar on the right edge of the trace panel:
   - The thumb should be **proportional** — if you can see roughly a third of the content, the thumb should occupy roughly a third of the scrollbar track.
   - The thumb should sit at the **bottom** of the track while the auto-follow pin is active.
4. Press `k` or arrow-up to scroll up manually. The thumb should move upward proportionally.
5. Press `G` (or scroll all the way down) to return to the end. Verify that **the last line of the last assistant response is visible at the bottom of the panel** — not cut off, not hidden below the viewport.
6. Scroll through the whole response with `k`/`j`. Compare the content you see by scrolling against the content you saw live during streaming. Nothing should be missing.
7. Press `q` or `Ctrl+C` to exit.

Expected visual result: scrollbar thumb reflects the proportion of hidden content (small thumb for long content, large thumb for short content). No content is hidden below the true bottom.

- [ ] **Step 4: Report**

If steps 1–3 pass, report DONE to the parent conversation. If the manual smoke shows any hidden tail or a non-proportional thumb, capture the exact symptom (which keys, which content) and surface it — the cause is likely in the wrap helper and one of the unit-test cases missed a real-world input.

- [ ] **Step 5: Optional Cargo.lock commit**

If `cargo build` refreshed `Cargo.lock`, a small follow-up may be needed:

```bash
cd /Volumes/Projects/spur
git status
# If only Cargo.lock changed:
git add Cargo.lock && git commit -m "chore: refresh Cargo.lock after unicode-width dep" 2>/dev/null || true
```

---

## Self-Review Notes

- **Spec coverage:** Fix A (scrollbar viewport_content_length) lives in Task 3 Step 3. Fix C (pre-wrap helper + disable Paragraph::wrap) lives in Tasks 2 (helper) and 3 (wiring). Spec non-goals (no turn grouping, no caching, no cosmetic scrollbar changes) are respected.
- **Placeholder scan:** No TBDs. Every step has actual code or a concrete command with expected output.
- **Type consistency:** `wrap_line_to_width(line: &Line<'_>, width: u16) -> Vec<Line<'static>>` in Task 2 matches the call site `lines.into_iter().flat_map(|l| wrap_line_to_width(&l, effective_width))` in Task 3. `effective_width` is `u16` (from `Rect::width`), matches the parameter type. `total_lines = wrapped.len()` is `usize`, matches `ScrollbarState::new(usize)`'s expected type per ratatui 0.29 docs.
- **Lifetime note:** The helper returns `Line<'static>` because it copies the input span content. Callers pass `Line<'_>` (usually `Line<'static>` already from previous builds), so the conversion is a no-op type-wise but forces an owned copy — acceptable for our frequency (one re-wrap per frame, bounded content).
- **Test sufficiency:** 11 tests cover the correctness invariants. The final `invariant_all_outputs_within_width` case acts as a light property test — any future wrap bug that violates the width invariant will surface there regardless of the specific input shape.
- **Degenerate inputs:** `width = 0` is a pass-through (returns clone). `effective_width` from ratatui is the inner rect width, which is only 0 for a 2-col-wide or narrower panel — an edge case that already rendered badly before, and our behavior degrades gracefully.
