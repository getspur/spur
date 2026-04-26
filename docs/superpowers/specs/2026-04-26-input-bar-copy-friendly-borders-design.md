# Copy-friendly TUI borders for the input composer and session-detail banners

**Status:** approved design (post-MCTS UX synthesis)
**Date:** 2026-04-26
**Scope:** `crates/spur-tui/src/components/input_bar.rs`, `crates/spur-tui/src/views/session_detail.rs`
**Brainstorming brief:** `docs/superpowers/dashboard-borders-copy-friendly-proposal.md`
**Reviewers:** worker:codex (Rust ergonomics + diff), worker:kimi (TUI UX), worker:gemini (architecture); L9 UX MCTS resynthesis on top.

---

## 1. Problem

ratatui's `Block::default().borders(Borders::ALL)` paints `│` glyphs on column 0 and column N-1 of every row inside a panel. Terminal selection is rectangular and character-cell-based, so dragging across multi-line content captures those border glyphs into the clipboard. The user-reported friction is concentrated in:

1. **The input composer (`input_bar.rs`)** — the primary multi-line copy source. Two near-duplicate border construction sites: `render` (line 1461-1464) and `render_inert` (line 1601-1604).
2. **The two session-detail banners (`session_detail.rs`)** — auth-error (line 1976) and session-error label (line 2195). Occasional copy targets when users share errors for bug reports or support.

## 2. First-principles framing

Terminal selection is rectangular and character-cell-based. Whatever glyph occupies a selected cell is the clipboard payload. Borders are *one* carrier of visual salience (boundary definition, alert weight). They are not the only carrier:

- **Rules** (`─`, `│`) carry **boundaries** between regions. Side rules pollute multi-line copy; horizontal rules cost at most one stray decorative row.
- **Background fill** (`Color::Red` bg, etc.) carries **alert weight** without any glyph cost — the bg is invisible to the clipboard.
- **Reversed-color spans** (`bg(X).fg(black)`) carry **focus/state indication** at a single localized point.
- **Glyph prefixes** (`●`, `▣`, `▦`, `○`) carry **mode/state recognition** independent of color, surviving colorblindness.

The right design question is therefore not "boxed vs. unboxed" but "**what carrier gives this surface its salience, and is that carrier copy-clean?**"

The auth-error banner today already has `bg(Color::Red).fg(Color::White).add_modifier(BOLD)`. The red background is the dominant salience carrier; the surrounding `Borders::ALL` is decorative reinforcement that *competes with* the bg fill for attention. Removing the redundant border concentrates the alert and gains copy-cleanliness — both for the same edit.

## 3. The design language

Two patterns, applied uniformly:

> **Regions use horizontal rules. Alerts use background fill. Side `│` columns are forbidden anywhere a user might select text.**

| Surface type | Treatment | Salience carrier |
| --- | --- | --- |
| Interactive region (composer, future panels) | `Borders::TOP \| Borders::BOTTOM` + reversed-color title badge with glyph prefix | colored top rule + reversed badge + symbol |
| Alert / banner (auth-error, session-error) | `Borders::NONE` + colored `bg` fill on the inner area | background color + bold/contrast text |

This produces zero `│` columns anywhere in the rendered view, eliminating the user's "which region copies clean?" cognitive load.

## 4. Change set

### 4a. `input_bar.rs` — composer

#### Module-local constants (new)

Add near the top of the `impl InputBar` region:

```rust
// Cells consumed by the composer's frame. Coupled to the `borders(...)` flag
// in `build_block` below — change them together. A future swap to
// `Borders::TOP` alone or `Borders::NONE` requires editing only these
// constants and the borders flag; rendering and arithmetic auto-track.
const BORDER_OVERHEAD_ROWS: u16 = 2;  // Borders::TOP | Borders::BOTTOM
const BORDER_OVERHEAD_COLS: u16 = 0;  // no left/right side borders
```

#### Shared block builder (new private method)

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

The `mode_str` `match` blocks at the top of both `render` (line 1441-1447) and `render_inert` (line 1592-1598) gain a glyph prefix per mode:

| Mode | Glyph | New `mode_str` literal | Meaning |
| --- | :-: | --- | --- |
| `EditMode::Emacs` | `●` | ` ● INSERT ` | live, taking input |
| `EditMode::Vim(VimMode::Insert)` | `●` | ` ● VIM·INSERT ` | live, taking input |
| `EditMode::Vim(VimMode::Normal)` | `▣` | ` ▣ VIM·NORMAL ` | command mode (full-box symbol) |
| `EditMode::Vim(VimMode::Visual)` | `▦` | ` ▦ VIM·VISUAL ` | selection mode (hatched) |
| `EditMode::Vim(VimMode::Operator(_))` | `▣` | ` ▣ VIM·OP ` | command operator |

`render_inert` uses the same glyph as the active mode but with `border_color = Color::DarkGray`, which is the existing inert cue — the gray reversed badge naturally reads as a paused ` ○ ` look without needing a separate glyph table. (The `○` glyph in §3's overview stands for the perceived effect; the implementation reuses the active-mode glyph dimmed.)

The glyph prefix provides a non-color cue for colorblind users and faster mode recognition. `build_title` itself is unchanged — it continues to accept the prepared `mode_str` and compose the title.

#### `render` and `render_inert` collapse

Both `render` (line 1440-1584) and `render_inert` (line 1591-…) lose their inline `Block::default()` construction and use the helper:

```rust
let block = self.build_block(mode_str, border_color);
let inner = block.inner(area);
self.last_inner_width.set(inner.width);
frame.render_widget(block, area);
```

The `border_color` derivation (Vim-mode-aware in `render`, fixed `Color::DarkGray` in `render_inert`) stays at the call sites; the helper does not need to know about `EditMode`.

#### `required_height` math

```rust
pub fn required_height(&self, width: u16) -> u16 {
    let inner_w = width.saturating_sub(BORDER_OVERHEAD_COLS);  // was - 2
    if inner_w == 0 {
        return 1 + BORDER_OVERHEAD_ROWS;                       // was return 3
    }

    let mut lines: Vec<String> = self.textarea.lines().to_vec();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let layout = crate::components::input_bar_wrap::wrap(&lines, inner_w);
    let inner = layout.visual_height().clamp(1, 5);
    inner + BORDER_OVERHEAD_ROWS                                // was inner + 2
}
```

With `BORDER_OVERHEAD_ROWS = 2` and `BORDER_OVERHEAD_COLS = 0`, the runtime values are identical to today's `+ 2` body — only the `inner_w` computation collapses to a no-op `saturating_sub(0)`. **A future swap to `Borders::TOP` alone or `Borders::NONE` requires editing only the two constants and the `borders(...)` flag in `build_block`. Rendering and arithmetic cannot drift.**

`render` and `render_inert` already access inner geometry via `let inner = block.inner(area);` — ratatui auto-adjusts `inner.width` (gains the 2 reclaimed cells) and `inner.height` (still `area.height - 2`).

`input_bar_wrap::wrap` is safe at `inner_w = 1` (asserts only `width > 0`, has a width-1 test), so no extra guard is needed.

### 4b. `session_detail.rs` — auth-error banner (line 1976)

```rust
let banner = Paragraph::new(msg.as_str())
    .style(
        Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD),
    )
    .wrap(Wrap { trim: false })
    .block(
        Block::default()
            .borders(Borders::NONE)        // was Borders::ALL
            .title("Authentication required"),
    );
```

The red bg fill alone carries the alert. The title sits on the first inner row (ratatui still renders titles on `Borders::NONE` blocks at the top of the area). The 3-row banner geometry is unchanged.

### 4c. `session_detail.rs` — session-error label (line 2195)

```rust
let para = Paragraph::new(message)
    .alignment(Alignment::Center)
    .style(
        Style::default()
            .fg(Color::White)              // was Color::Red — swapped for contrast on red bg
            .bg(Color::Red)                // new: matches auth-error pattern
            .add_modifier(Modifier::BOLD), // new: matches auth-error pattern
    )
    .block(
        Block::default()
            .borders(Borders::NONE)        // was Borders::ALL
            .title("Session error"),
    );
```

Equalizes the two banners' salience pattern. Both alerts now use red-bg-fill as the carrier.

## 5. Test impact

### `input_bar.rs` (file-local)

Because vertical border count is unchanged (still 2 rows) and the `+ 2` literal becomes `+ BORDER_OVERHEAD_ROWS` (= 2), all `required_height` asserts that pin vertical math stay correct. Only `inner_w` shifts from `width − 2` to `width`:

| Line | Today | After | Notes |
| --- | --- | --- | --- |
| 1702 | `required_height(80) == 3` | **3** unchanged | 1 inner + 2 borders |
| 1711 | `required_height(82) == 5` | **5** unchanged | inner_w 82 now (was 80); 200/82→3 rows still. Comment on line 1711 updates from `// inner width = 80` to `// inner width = 82`. |
| 1715 | fn name `..._plus_borders` | unchanged | still 2 borders |
| 1718 | `required_height(82) == 7` | **7** unchanged | clamp 5 + 2 |
| 1726 | `required_height(22) == 3` | **3** unchanged | inner_w 22 now (was 20); still 1 row |
| ~1806 | `last_inner_width_for_test() == 18` | **20** | **only assert that changes** |

Recommended sweep: re-express literal `+ 2` test asserts in terms of `BORDER_OVERHEAD_ROWS` (e.g. `assert_eq!(bar.required_height(80), 1 + BORDER_OVERHEAD_ROWS);`) so the tests become regression detectors for the constants themselves.

### `session_detail.rs`

No unit tests pin the banner border state. Visual smoke test only.

## 6. De-risking experiment

Before promoting to a plan, run a local prototype: edit `input_bar.rs` only, then `cargo test -p spur-tui`. Expected blast radius is exactly the one assert in the table above (`last_inner_width 18 → 20`). Anything beyond that is a signal that other tests are coupled to the side-border footprint and need attention.

## 7. Acceptance criteria

- Drag-select 5 lines in the active composer → clipboard contains zero `│` characters and no leading/trailing whitespace beyond what the user typed.
- Drag-select 3 lines of the auth-error banner → clipboard contains the message text only, no `│`.
- Drag-select the session-error label → clipboard contains the message text only, no `│`.
- Active / inactive / inert focus colour cues remain visually obvious in the composer.
- Vim mode is recognizable both by color (Green / Yellow / LightYellow / DarkGray) and by glyph (`●` / `▣` / `▦` / `○`).
- All `input_bar.rs` tests pass after the documented assert update.
- Existing `session_detail.rs` and `dashboard.rs` integration tests pass without changes.

## 8. Risk audit

| # | Risk | Likelihood | Impact | Composite | Mitigation |
| --- | --- | :-: | :-: | :-: | --- |
| 1 | Test failure on missed assert (`last_inner_width 18→20`) | High | Low | **Low** | Update one line; documented |
| 2 | Snapshot/golden test elsewhere pins `│` glyph in narrow scope | Low | Med | **Low** | De-risking step catches it |
| 3 | Reversed-color badge unreadable on niche terminals | Low | Low | **Low** | Theme override is a 1-line change |
| 4 | Vim users disoriented by lost full-box mode glow | Low | Low | **Low** | Reversed badge + glyph prefix compensate |
| 5 | `BORDER_OVERHEAD_*` constants drift from `borders(...)` flag in future edits | Med | Med | **Med** | Module-level coupling comment; tests assert via the constants |
| 6 | Layout off-by-one in `dashboard.rs` / `session_detail.rs` consumers | Low | Med | **Low** | Consumers call `required_height` opaquely |
| 7 | Banner bg-fill renders poorly on legacy terminals | Low | Med | **Low** | `Color::Red` bg already required by current auth-error; no new ground |
| 8 | Need to retrofit a crate-wide helper later | High | Low | **Low** | When react_trace lands, splits cleanly into `panel_block` (TOP\|BOTTOM) + `alert_block` (NONE+bg) |
| 9 | Session-error gains a bg fill — slight visual change | Med | Low | **Low** | Matches auth-error pattern; net consistency win |
| 10 | Mode-badge glyph rendering on minimal terminals | Low | Low | **Low** | Title text still readable as fallback |

**Aggregate: LOW.** Highest residual is constants drift (#5), mitigated by a module-level comment and tests anchored on the constants.

## 9. Out of scope (filed as follow-ups)

- `crates/spur-tui/src/components/react_trace/render.rs` (lines 288, 403). Primary copy source for trace bodies. Same `TOP | BOTTOM` rules + reversed-badge + glyph-prefix treatment will apply. **When this lands, it is the natural moment to extract a crate-wide `panel_block(focused, title, glyph) -> Block` helper and a parallel `alert_block(bg_color, title) -> Block` helper.** Two data points are sufficient to settle the helper signatures; doing it speculatively from one site is premature.
- Other dashboard panels: `activity_log.rs`, `detail_pane.rs`, `agents_tree.rs`, `plan_*.rs`. Track in a single follow-up issue.
- Runtime `copy_mode` toggle (the rejected Option B from brainstorming): no ambient need once the design language above is in place; revisit only if user feedback demands a global "stripped chrome" mode.
- Subtle background tint on composer interior. Adds nice containment but interacts with non-truecolor terminals; revisit if the bottom rule alone proves insufficient.
- Dimmed placeholder text inside an empty inert composer. Useful but scope-creep; defer until inert-empty state is observed to be confusing.

## 10. Reviewer convergence summary

| Reviewer | Position | How v1 addresses it |
| --- | --- | --- |
| codex (Rust) | Ship with adjustments; caught two extra test sites; verified `wrap` safe at width=1. | Test table above includes the missed sites; constants make the math single-sourced. |
| kimi (UX) | Ship with UX additions; insisted on `TOP \| BOTTOM` to preserve status-bar separator; recommended reversed-color badge to compensate for lost mode glow. | Both bundled; glyph prefix added on top for colorblind users. |
| gemini (architecture) | Ship with architectural changes; insisted on local helper now, `BORDER_OVERHEAD` constants to prevent drift, and challenged the "users don't copy errors" exception. | Local helper bundled; constants bundled; banners now drop borders entirely (gemini's consistency win) but lean on bg-fill (kimi's salience requirement) — both reviewers' core concerns satisfied simultaneously. |
| L9 UX MCTS resynthesis | The original "TOP\|BOTTOM everywhere" answer was conflating two different surface types. Alerts and regions need different carriers. | Two-pattern design language documented in §3. Final design eliminates `│` from every selectable surface. |
