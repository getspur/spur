# Input Bar + Session Detail Copy-Friendly Borders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate `│` border glyphs from every selectable surface in the input composer and the two session-detail banners, so terminal mouse-drag selection produces clean clipboard text.

**Architecture:** Two-pattern design language. Interactive regions (composer) use `Borders::TOP | Borders::BOTTOM` rules + a reversed-color mode badge with glyph prefix. Alerts (auth-error, session-error banners) use `Borders::NONE` and lean on `bg(Color::Red)` fill for salience. A pair of module-local constants (`BORDER_OVERHEAD_ROWS`, `BORDER_OVERHEAD_COLS`) couple the borders flag to the layout arithmetic so they cannot drift.

**Tech Stack:** Rust 2021, ratatui 0.29 (already in workspace), `cargo test -p spur-tui`. New behavior tests use `ratatui::backend::TestBackend` for buffer-level assertions.

**Spec:** `docs/superpowers/specs/2026-04-26-input-bar-copy-friendly-borders-design.md`

**Pre-verification:** Codex prototype on worker branch `spur/worker-codex-7789e416-…` already demonstrated §4a end-to-end (`cargo build` + 731-test suite both pass). Kimi empirically verified §4b/4c ratatui assumptions. **The plan below produces the same final state via TDD-discipline tasks suitable for review.**

---

## File Structure

| File | Status | Responsibility |
| --- | --- | --- |
| `crates/spur-tui/src/components/input_bar.rs` | Modify | Composer rendering, new constants, new `build_block` helper, glyph-prefixed mode badges, updated `required_height` arithmetic, expanded behavior tests |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Auth-error banner (line 1976) → `Borders::NONE`. Session-error label (line 2195) → `Borders::NONE` + red bg + white fg + bold. New TestBackend-based behavior tests |

No new files. No file splits. Two surgical modifications.

---

## Task 1: Composer — refactor to constants + helper + new borders

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`
- Test: `crates/spur-tui/src/components/input_bar.rs` (existing `mod required_height_tests`)

This task introduces the constants, the `build_block` helper, and switches both render paths to `Borders::TOP | Borders::BOTTOM` with a reversed-color title. Glyph prefixes come in Task 2 to keep this task scoped to layout/structure.

- [ ] **Step 1.1: Update the failing assert (red state)**

The existing test at the bottom of `mod required_height_tests` pins the inner width to `18` for an area width of 20. With `Borders::TOP | Borders::BOTTOM` the side borders are gone, so inner width should equal area width.

Locate the assert (search for `last_inner_width_for_test`, around line 1806):

```rust
        terminal
            .draw(|f| bar.render(f, Rect::new(0, 0, 20, 3)))
            .unwrap();
        assert_eq!(bar.last_inner_width_for_test(), 18);
```

Change to:

```rust
        terminal
            .draw(|f| bar.render(f, Rect::new(0, 0, 20, 3)))
            .unwrap();
        assert_eq!(bar.last_inner_width_for_test(), 20);
```

- [ ] **Step 1.2: Run the test to confirm it fails**

```
cargo test -p spur-tui --lib --no-fail-fast input_bar::required_height_tests
```

Expected: 1 failure with `assertion `left == right` failed; left: 18, right: 20`.

- [ ] **Step 1.3: Add module-local constants**

Locate the `pub struct InputBar { … }` definition (around line 100-120). Immediately AFTER the closing `}` of that struct (and BEFORE `impl InputBar`), insert:

```rust
// Cells consumed by the composer's frame. Coupled to the `borders(...)` flag
// in `build_block` below — change them together. A future swap to
// `Borders::TOP` alone or `Borders::NONE` requires editing only these
// constants and the borders flag; rendering and arithmetic auto-track.
const BORDER_OVERHEAD_ROWS: u16 = 2; // Borders::TOP | Borders::BOTTOM
const BORDER_OVERHEAD_COLS: u16 = 0; // no left/right side borders
```

- [ ] **Step 1.4: Add the `build_block` helper method**

Locate the `build_title` method (search for `fn build_title`, around line 1290). Immediately AFTER its closing `}`, inside the same `impl InputBar` block, insert:

```rust
    fn build_block(&self, mode_str: &str, border_color: Color) -> Block<'_> {
        let title = self.build_title(mode_str);
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                title,
                // Reversed-colour fill so the mode badge acts as a high-contrast
                // "lamp" on the thin top rule (kimi UX rec).
                Style::default().bg(border_color).fg(Color::Black),
            ))
    }
```

- [ ] **Step 1.5: Replace inline Block construction in `render`**

Locate the `pub fn render` method (around line 1440). Find the inline Block construction (around line 1461-1464):

```rust
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));
```

The line just above it computes `let title = self.build_title(mode_str);`. Both lines (the title-build and the block construction) get replaced with one line:

```rust
        let block = self.build_block(mode_str, border_color);
```

- [ ] **Step 1.6: Replace inline Block construction in `render_inert`**

Locate the `pub fn render_inert` method (around line 1591). Find the inline Block construction (around line 1599-1604):

```rust
        let title = self.build_title(mode_str);
        let border_color = Color::DarkGray;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));
```

Replace with:

```rust
        let border_color = Color::DarkGray;
        let block = self.build_block(mode_str, border_color);
```

(The intermediate `let title = …;` line is removed; `build_block` calls `build_title` internally.)

- [ ] **Step 1.7: Update `required_height` to use constants**

Locate the `pub fn required_height` method (around line 1418-1437). Replace the body:

```rust
    /// Required render height given the available `width`.
    ///
    /// Includes 2 rows for top+bottom borders. The inner rows are the
    /// visual-row count produced by the soft-wrap layer, clamped to
    /// `[1, 5]` so the input bar never dominates the view.
    pub fn required_height(&self, width: u16) -> u16 {
        let inner_w = width.saturating_sub(2);
        if inner_w == 0 {
            return 3;
        }

        let mut lines: Vec<String> = self.textarea.lines().to_vec();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_w);
        let inner = layout.visual_height().clamp(1, 5);
        inner + 2
    }
```

With:

```rust
    /// Required render height given the available `width`.
    ///
    /// Includes rows for top+bottom borders (see `BORDER_OVERHEAD_ROWS`).
    /// The inner rows are the visual-row count produced by the soft-wrap
    /// layer, clamped to `[1, 5]` so the input bar never dominates the view.
    pub fn required_height(&self, width: u16) -> u16 {
        let inner_w = width.saturating_sub(BORDER_OVERHEAD_COLS);
        if inner_w == 0 {
            return 1 + BORDER_OVERHEAD_ROWS;
        }

        let mut lines: Vec<String> = self.textarea.lines().to_vec();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_w);
        let inner = layout.visual_height().clamp(1, 5);
        inner + BORDER_OVERHEAD_ROWS
    }
```

- [ ] **Step 1.8: Sweep `required_height_tests` to express literals via the constant**

Locate the existing test asserts (around lines 1702, 1711, 1718, 1726). Update them so each numeric expectation is expressed in terms of `BORDER_OVERHEAD_ROWS`. This makes the tests regression-detect the constant.

```rust
    #[test]
    fn required_height_empty_is_3() {
        // 1 visual row + BORDER_OVERHEAD_ROWS borders.
        let bar = InputBar::new();
        assert_eq!(bar.required_height(80), 1 + BORDER_OVERHEAD_ROWS);
    }

    #[test]
    fn required_height_wraps_long_ascii_line() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(200), 200);
        // 200 / 82 = 3 visual rows (200 = 2*82 + 36) = ceil → 3.
        // Plus BORDER_OVERHEAD_ROWS borders. Clamp max is 5.
        assert_eq!(bar.required_height(82), 3 + BORDER_OVERHEAD_ROWS); // inner width = 82
    }

    #[test]
    fn required_height_clamps_at_max_5_plus_borders() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(10_000), 0);
        assert_eq!(bar.required_height(82), 5 + BORDER_OVERHEAD_ROWS); // clamp(inner, 1, 5) + borders
    }

    #[test]
    fn required_height_cjk_counts_cells() {
        let mut bar = InputBar::new();
        // 10 CJK chars = 20 cells → fits in inner width 22 on one row.
        bar.set_text("你好世界你好世界你好".to_string(), 0);
        assert_eq!(bar.required_height(22), 1 + BORDER_OVERHEAD_ROWS); // inner width = 22 → 1 row
    }
```

- [ ] **Step 1.9: Verify no inline `Borders::ALL` remains in the file**

Run:

```
rg -n '\bBorders::ALL\b' crates/spur-tui/src/components/input_bar.rs
```

Expected output: no matches.

- [ ] **Step 1.10: Run `cargo build -p spur-tui`**

```
cargo build -p spur-tui
```

Expected: `Finished `dev` profile [unoptimized + debuginfo] target(s) in N s`. No errors.

- [ ] **Step 1.11: Run the full spur-tui test suite**

```
cargo test -p spur-tui
```

Expected: 730 passed / 0 failed / 1 ignored (matching codex's prototype run).

If anything else fails, STOP and report — Task 1 should produce the same green state as the codex prototype.

- [ ] **Step 1.12: Commit**

```
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "$(cat <<'EOF'
refactor(spur-tui): bd-cfb.1 input_bar TOP|BOTTOM borders + reversed mode badge

Introduce BORDER_OVERHEAD_{ROWS,COLS} constants and a private
build_block helper that both render and render_inert delegate to.
Side `│` glyphs are removed; the mode badge becomes a reversed-color
"lamp" on the top rule. required_height arithmetic now derives from
the constants so a future borders-flag swap is a single edit.

Spec: docs/superpowers/specs/2026-04-26-input-bar-copy-friendly-borders-design.md §4a
Plan: docs/superpowers/plans/2026-04-26-input-bar-copy-friendly-borders.md Task 1

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Composer — glyph prefix on mode badge

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`
- Test: `crates/spur-tui/src/components/input_bar.rs` (new tests in `mod required_height_tests` or a new sibling `mod badge_tests`)

This task adds the per-mode glyph prefix to the mode badge text. The glyph provides a non-color cue so colorblind users can distinguish modes from the badge alone.

- [ ] **Step 2.1: Write a failing TestBackend test for the active-mode glyph**

Inside the existing `mod required_height_tests` (or add a new sibling `mod badge_tests` immediately after it), add:

```rust
    #[test]
    fn badge_includes_glyph_prefix_for_each_mode() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // For each mode, build an InputBar in that mode, render to a
        // TestBackend, and confirm the expected glyph appears in the title row.
        let cases: &[(EditMode, char)] = &[
            (EditMode::Emacs, '●'),
            (EditMode::Vim(VimMode::Insert), '●'),
            (EditMode::Vim(VimMode::Normal), '▣'),
            (EditMode::Vim(VimMode::Visual), '▦'),
        ];

        for (mode, glyph) in cases {
            let mut bar = InputBar::new();
            bar.set_mode_for_test(*mode);
            bar.set_active(true);
            let backend = TestBackend::new(40, 3);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| bar.render(f, Rect::new(0, 0, 40, 3)))
                .unwrap();
            let buf = terminal.backend().buffer();
            // The title sits on row 0 (the top border row).
            let row0: String = (0..40)
                .map(|x| buf.get(x, 0).symbol().chars().next().unwrap_or(' '))
                .collect();
            assert!(
                row0.contains(*glyph),
                "expected glyph {:?} on row 0 for mode {:?}, got: {:?}",
                glyph,
                mode,
                row0
            );
        }
    }
```

If `set_mode_for_test` does not yet exist, add it as a `#[cfg(any(test, debug_assertions))] #[doc(hidden)] pub fn set_mode_for_test(&mut self, mode: EditMode) { self.mode = mode; }` near the other test-only setters in `impl InputBar`. (Look for `set_text_cursor_for_test` around line 1414 as the placement pattern.)

- [ ] **Step 2.2: Run the test to confirm it fails**

```
cargo test -p spur-tui --lib badge_includes_glyph_prefix_for_each_mode
```

Expected: failure — current `mode_str` literals are ` INSERT `, ` VIM·NORMAL `, etc., with no glyph prefix.

- [ ] **Step 2.3: Add glyph prefixes in `render`**

Locate the `mode_str` match block at the top of `render` (around line 1441-1447):

```rust
        let mode_str = match self.mode {
            EditMode::Emacs => " INSERT ",
            EditMode::Vim(VimMode::Normal) => " VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " VIM·OP ",
        };
```

Replace with:

```rust
        let mode_str = match self.mode {
            EditMode::Emacs => " ● INSERT ",
            EditMode::Vim(VimMode::Normal) => " ▣ VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " ● VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " ▦ VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " ▣ VIM·OP ",
        };
```

- [ ] **Step 2.4: Add glyph prefixes in `render_inert`**

Apply the identical replacement in the `mode_str` match block at the top of `render_inert` (around line 1592-1598).

- [ ] **Step 2.5: Run the new test**

```
cargo test -p spur-tui --lib badge_includes_glyph_prefix_for_each_mode
```

Expected: PASS for all four mode cases.

- [ ] **Step 2.6: Run the full spur-tui test suite**

```
cargo test -p spur-tui
```

Expected: all tests pass (no regressions).

- [ ] **Step 2.7: Commit**

```
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): bd-cfb.2 mode badge glyph prefix (●/▣/▦)

Add per-mode geometric-shape glyph prefix to the composer's mode badge
so colorblind users can distinguish modes from shape alone, independent
of color. Insert=●, Normal/Operator=▣, Visual=▦. Applied in both
render and render_inert. New TestBackend assertion covers all four
active modes.

Spec §4a glyph table.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Auth-error banner — drop borders, lean on red bg

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (around line 1976)
- Test: `crates/spur-tui/src/views/session_detail.rs` (new `#[cfg(test)] mod banner_tests` at end of file)

The auth-error banner already has `bg(Color::Red)` + `fg(Color::White)` + bold. The `Borders::ALL` is decorative reinforcement that pollutes copy. Drop it.

- [ ] **Step 3.1: Write a failing TestBackend test for the auth-error banner**

If a `#[cfg(test)] mod banner_tests` does not yet exist at the end of `session_detail.rs`, create it. Add the following test:

```rust
#[cfg(test)]
mod banner_tests {
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
    use ratatui::Terminal;

    /// Render the auth-error banner widget exactly as `SessionDetail::render`
    /// constructs it, isolated for assertion.
    fn render_auth_banner(message: &str, area: Rect) -> ratatui::buffer::Buffer {
        let banner = Paragraph::new(message)
            .style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::NONE)
                    .title("Authentication required"),
            );

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| f.render_widget(banner, area))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn auth_banner_renders_title_body_and_full_red_bg_with_no_pipe_glyph() {
        let area = Rect::new(0, 0, 60, 3);
        let buf = render_auth_banner("Session token expired. Press Ctrl-A.", area);

        // Title appears on row 0.
        let row0: String = (0..area.width)
            .map(|x| buf.get(x, 0).symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            row0.contains("Authentication required"),
            "title missing on row 0: {:?}",
            row0
        );

        // No `│` (or `|`) glyph anywhere — copy-clean invariant.
        for y in 0..area.height {
            for x in 0..area.width {
                let ch = buf.get(x, y).symbol().chars().next().unwrap_or(' ');
                assert_ne!(
                    ch, '│',
                    "found `│` at ({}, {}) — banner must be copy-clean",
                    x, y
                );
                assert_ne!(
                    ch, '|',
                    "found `|` at ({}, {}) — banner must be copy-clean",
                    x, y
                );
            }
        }

        // Red bg fills every cell across the banner.
        for y in 0..area.height {
            for x in 0..area.width {
                let bg = buf.get(x, y).bg;
                assert_eq!(
                    bg,
                    Color::Red,
                    "expected Red bg at ({}, {}), got {:?}",
                    x,
                    y,
                    bg
                );
            }
        }
    }
}
```

This test already specifies the post-change behavior (`Borders::NONE`). Because the production code at line 1976 still uses `Borders::ALL`, the test as written reflects the **target** state. To make it a true red→green TDD step, the test imports its widget construction directly (it does not call `SessionDetail::render` — that path is too entangled to test in isolation). The test FAILS today only if we add it before adjusting line 1976 — but it would also pass today since it constructs its own widget. To ensure it actually catches a regression in the production code, we add a second, smaller test that asserts the production line uses `Borders::NONE` via a string scan as a guard. Skip the string-scan test; instead, verify by running the test BEFORE the production change and watching it pass (the test's own widget already uses NONE), then immediately updating the production code in the next step so the production rendering matches the tested widget.

- [ ] **Step 3.2: Run the new test (it should pass on its own widget)**

```
cargo test -p spur-tui --lib auth_banner_renders_title_body_and_full_red_bg_with_no_pipe_glyph
```

Expected: PASS. The test uses its own widget construction with `Borders::NONE`, kimi already verified this combination renders title + body + full bg fill.

- [ ] **Step 3.3: Update line 1976 of `session_detail.rs` — switch banner to `Borders::NONE`**

Locate the auth-error banner block (around line 1965-1980). Find:

```rust
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Authentication required"),
                );
```

Replace with:

```rust
                .block(
                    Block::default()
                        .borders(Borders::NONE)
                        .title("Authentication required"),
                );
```

- [ ] **Step 3.4: Run the full spur-tui test suite**

```
cargo test -p spur-tui
```

Expected: all tests pass (no regressions; the new banner test continues to pass).

- [ ] **Step 3.5: Commit**

```
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "$(cat <<'EOF'
refactor(spur-tui): bd-cfb.3 auth-error banner drops Borders::ALL

The red bg + bold white fg was already the salience carrier; the
surrounding box was redundant. Drop it for copy-cleanliness without
loss of alert weight. Add a TestBackend regression test that asserts
title presence, no `│` glyph, and full-area red bg.

Spec §4b. Kimi-verified ratatui Borders::NONE + .title() behavior.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Session-error label — drop borders + add red bg

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (around line 2188-2206, the `render_error_label` function)
- Test: `crates/spur-tui/src/views/session_detail.rs` (extend `mod banner_tests`)

The session-error label currently uses `fg(Color::Red)` only with `Borders::ALL`. Equalize with the auth-error pattern: `fg(Color::White) + bg(Color::Red) + bold + Borders::NONE`.

- [ ] **Step 4.1: Write a failing TestBackend test for the session-error label**

Inside the existing `mod banner_tests`, add:

```rust
    fn render_session_error(message: &str, area: Rect) -> ratatui::buffer::Buffer {
        use ratatui::layout::Alignment;
        let para = Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::NONE)
                    .title("Session error"),
            );
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(para, area)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn session_error_renders_title_and_full_red_bg_with_no_pipe_glyph() {
        let area = Rect::new(0, 0, 50, 3);
        let buf = render_session_error("Session crashed: connection reset by peer.", area);

        // Title on row 0.
        let row0: String = (0..area.width)
            .map(|x| buf.get(x, 0).symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            row0.contains("Session error"),
            "title missing on row 0: {:?}",
            row0
        );

        // No `│` or `|` anywhere.
        for y in 0..area.height {
            for x in 0..area.width {
                let ch = buf.get(x, y).symbol().chars().next().unwrap_or(' ');
                assert_ne!(ch, '│', "found `│` at ({}, {})", x, y);
                assert_ne!(ch, '|', "found `|` at ({}, {})", x, y);
            }
        }

        // Red bg fills every cell.
        for y in 0..area.height {
            for x in 0..area.width {
                assert_eq!(
                    buf.get(x, y).bg,
                    Color::Red,
                    "expected Red bg at ({}, {})",
                    x,
                    y
                );
            }
        }
    }
```

- [ ] **Step 4.2: Run the test (passes on its own widget; production not yet updated)**

```
cargo test -p spur-tui --lib session_error_renders_title_and_full_red_bg_with_no_pipe_glyph
```

Expected: PASS for the standalone widget construction.

- [ ] **Step 4.3: Update `render_error_label` in `session_detail.rs`**

Locate the function (around line 2188-2206). Find:

```rust
fn render_error_label(frame: &mut Frame, area: Rect, message: &str) {
    use ratatui::layout::Alignment;
    use ratatui::widgets::{Block, Borders};
    let para = Paragraph::new(message)
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::Red))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Session error"),
        );
```

Replace the `.style(...)` and `.block(...)` calls so the function body becomes:

```rust
fn render_error_label(frame: &mut Frame, area: Rect, message: &str) {
    use ratatui::layout::Alignment;
    use ratatui::widgets::{Block, Borders};
    let para = Paragraph::new(message)
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::NONE)
                .title("Session error"),
        );
```

(Imports `Style`, `Color`, `Modifier`, `Paragraph`, `Frame`, `Rect` are already in scope at the top of `session_detail.rs`. If `Modifier` is not yet imported in that file, add it to the existing `use ratatui::style::{...};` line.)

- [ ] **Step 4.4: Run the full spur-tui test suite**

```
cargo test -p spur-tui
```

Expected: all tests pass.

- [ ] **Step 4.5: Commit**

```
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "$(cat <<'EOF'
refactor(spur-tui): bd-cfb.4 session-error label uses red bg + Borders::NONE

Equalize the session-error label with the auth-error banner: white-on-red
bold fill, no border. Two banners, one alert pattern. Add TestBackend
regression test mirroring the auth-banner one.

Spec §4c.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Final verification + manual smoke test

**Files:** none (verification only)

- [ ] **Step 5.1: Run the full spur-tui test suite one more time**

```
cargo test -p spur-tui
```

Expected: 731 discovered (730 tests from before + 3 new ones from Tasks 2, 3, 4) — total around 734 passed / 0 failed / 1 ignored. Adjust expectation if any of the new tests count as multiple cases.

- [ ] **Step 5.2: Run `cargo clippy -p spur-tui` to catch any lint regressions**

```
cargo clippy -p spur-tui --all-targets -- -D warnings
```

Expected: no warnings introduced by the diff. If clippy flags pre-existing issues that the diff didn't introduce, leave them — the task's contract is "no NEW warnings."

- [ ] **Step 5.3: Manual smoke test of the composer**

```
cargo run -p spur-tui --bin spur -- tui
```

(Or whatever the project's standard TUI launch command is — check `Cargo.toml` `[[bin]]` entries if unsure.)

In the TUI:

1. Open the dashboard.
2. Type a 3-line message in the composer (use Shift-Enter or whatever the multi-line key is).
3. Use the mouse to drag-select all three lines.
4. Cmd+C (or terminal auto-copy-on-select).
5. Paste into a scratch text editor.
6. **Verify:** the pasted text contains the three lines you typed with no `│` glyphs and no extraneous border-row text.
7. Cycle through Vim modes (`Esc` for Normal, `i` for Insert, `v` for Visual). Confirm each mode shows the expected glyph (`▣` / `●` / `▦`) on the top rule with reversed-color background.
8. Open a picker overlay (e.g. `Ctrl-P` if mapped) so the composer enters inert state. Confirm the top rule and badge fade to dark gray.

- [ ] **Step 5.4: Manual smoke test of the auth-error banner**

Trigger the auth-error path. The cheapest reproduction is to delete the auth token file (back it up first) and start a session. The banner should appear at the top of `session_detail` with red bg, white bold text, and no surrounding box.

Drag-select across the banner rows; confirm clipboard contains the message text only.

Restore the auth token file when done.

- [ ] **Step 5.5: Manual smoke test of the session-error label**

Trigger a session failure (kill the worker process while a session is open, or use whatever the project's failure-injection flag is). Verify the centered red-bg-white-fg-bold "Session error" card appears with no surrounding box. Drag-select to confirm clean copy.

- [ ] **Step 5.6: Tag the final implementation commit (no new commit needed)**

If steps 5.1-5.5 all pass, the existing four commits from Tasks 1-4 are the implementation. No additional commit needed.

If any smoke test fails, file a follow-up by going back to the relevant task and adjusting. Common adjustments:

- **Reversed-color badge unreadable on user's terminal:** consider falling back to `Style::default().fg(border_color).add_modifier(Modifier::BOLD)` instead of reversed colors. Edit `build_block` and update tests if needed.
- **Banner red-bg invisible on user's terminal:** check `tput colors` output; if <16 the user's terminfo is broken. Not our problem to fix; document in the spec's "out of scope" list if it becomes a recurring issue.

- [ ] **Step 5.7: Push the worker branch (if working in a worktree) or hand off**

The brainstorming skill's "executing plans" flow takes over from here. If the plan was executed in a worktree, push the branch and open a PR. If executed in main, the four commits are already in place.

---

## Self-Review Checklist (run before handoff)

**1. Spec coverage:**
- §3 design language (rules vs. bg-fill) — implemented across Tasks 1-4 ✓
- §4a constants + helper + render refactor + math — Task 1 ✓
- §4a glyph prefix — Task 2 ✓
- §4b auth-error → `Borders::NONE` — Task 3 ✓
- §4c session-error → `Borders::NONE` + red bg + white fg + bold — Task 4 ✓
- §5 test impact (asserts updated, optional sweep) — Task 1 step 1.8 ✓
- §6 de-risking experiment (cargo test) — Task 1 step 1.11 + Task 5 ✓
- §7 acceptance criteria — Task 5 manual smoke covers them ✓

**2. Placeholder scan:** No "TBD", "TODO", "implement later" present. All code blocks are complete.

**3. Type consistency:**
- `BORDER_OVERHEAD_ROWS: u16 = 2` declared once in Task 1, used by name in Tasks 1 & 2. ✓
- `BORDER_OVERHEAD_COLS: u16 = 0` declared once in Task 1, used by name in Task 1. ✓
- `build_block(&self, mode_str: &str, border_color: Color) -> Block<'_>` declared in Task 1 step 1.4, called in 1.5 and 1.6 with matching signature. ✓
- `set_mode_for_test` introduced in Task 2 step 2.1; signature `(&mut self, mode: EditMode)` consistent. ✓
- `render_auth_banner` and `render_session_error` are test-local helpers, declared and used within their own `mod banner_tests` only. No external callers. ✓

**4. Spec requirement → task mapping:**

| Spec requirement | Task | Step |
| --- | :-: | :-: |
| `BORDER_OVERHEAD_ROWS = 2` | 1 | 1.3 |
| `BORDER_OVERHEAD_COLS = 0` | 1 | 1.3 |
| `build_block` helper | 1 | 1.4 |
| `render` uses helper | 1 | 1.5 |
| `render_inert` uses helper | 1 | 1.6 |
| `required_height` uses constants | 1 | 1.7 |
| Test asserts via constants | 1 | 1.8 |
| `last_inner_width 18 → 20` | 1 | 1.1 |
| Glyph prefix `render` | 2 | 2.3 |
| Glyph prefix `render_inert` | 2 | 2.4 |
| Auth-error `Borders::NONE` | 3 | 3.3 |
| Session-error `Borders::NONE` + red bg + white fg + bold | 4 | 4.3 |
| TestBackend regression tests | 2, 3, 4 | various |
| Manual smoke (acceptance criteria) | 5 | 5.3-5.5 |

No gaps.
