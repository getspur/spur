# Copy-friendly TUI borders — narrowed scope: `input_bar` + `session_detail`

**Status:** brainstorming brief, pre-spec
**Author:** brain (Opus 4.7, L9 staff/data-eng framing)
**Reviewers requested:** worker:codex, worker:kimi, worker:gemini
**Scope (narrowed by user):** `crates/spur-tui/src/components/input_bar.rs`, `crates/spur-tui/src/views/session_detail.rs`

> Earlier draft surveyed the entire `spur-tui` crate. User pointed out the **input_bar is the actual copy-paste source** and `session_detail` is the actual copy *destination*. Other panels (`activity_log`, `detail_pane`, `react_trace`, `plan_*`) are out of scope here and filed for follow-up.

---

## 1. Problem (re-stated for the narrowed scope)

ratatui's `Block::default().borders(Borders::ALL)` paints `│` on column 0 and column N-1 of every inner row. Terminal mouse-selection is rectangular and character-cell based, so dragging across multi-line content captures those `│` glyphs into the clipboard.

The user-reported friction is concentrated in two places:

1. **`input_bar.rs`** — where the user types multi-line drafts and frequently re-selects them to copy/cut into another tool. Two near-duplicate `Borders::ALL` sites:
   - `render` (active composer) — line 1461-1464.
   - `render_inert` (overlay-active state) — line 1601-1604.
2. **`session_detail.rs`** — the view that hosts the input_bar plus the react-trace body. Three `Block`s in this file:
   - `1976-1978` auth-error banner (red, full-screen-width, height=3 modal-feeling card).
   - `2176` already `Borders::NONE` (centered load label) — no change.
   - `2195-2197` session-error label ("Session error" title), centered, height=3.

## 2. First-principles framing

Terminal selection is rectangular and character-cell based. Whatever glyph sits in selected cells *is* the clipboard payload.

- **Side borders** (`│` on every row) → catastrophic copy pollution.
- **Top border only** (`─` on one row, with the title sitting on it) → at most one stray decorative row in the clipboard, easy to ignore.
- **Modal/banner cards** (auth-error, session-error) → user does *not* drag-select these; they are short, ephemeral, and the visual box helps draw attention. Copy-friendliness is not the goal here.

Therefore the narrowed proposal targets **only the input composer** for the border swap, and explicitly **keeps the two error banners boxed**.

## 3. Concrete change set

### 3a. `input_bar.rs` — switch to `Borders::TOP | Borders::BOTTOM`, factor a local helper, anchor on `BORDER_OVERHEAD_*` constants

> **Revised twice.** First after kimi UX review (TOP-only → TOP|BOTTOM to keep the status-bar separator). Second after gemini architecture review: extract a small local helper inside `input_bar.rs` (two near-duplicate construction sites in `render` and `render_inert` already justify it), and replace the `+ 2` / `- 2` magic numbers with module-local constants so rendering and arithmetic share one source of truth.

Add to the top of the `impl InputBar` region (or as module-private `const`s):

```rust
// Vertical / horizontal cells consumed by the composer's frame.
// Coupled to the `borders(...)` flags below — change them together.
const BORDER_OVERHEAD_ROWS: u16 = 2;  // Borders::TOP | Borders::BOTTOM
const BORDER_OVERHEAD_COLS: u16 = 0;  // no left/right side borders
```

Add a small private helper that both render paths share:

```rust
fn build_block(&self, mode_str: &str, border_color: Color) -> Block<'_> {
    let title = self.build_title(mode_str);
    Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            // Reversed-colour fill so the mode badge acts as a high-contrast
            // "lamp" on the thin top rule (kimi UX rec #2).
            Style::default().bg(border_color).fg(Color::Black),
        ))
}
```

Both `render` and `render_inert` collapse from a 4-line block construction to:

```rust
let block = self.build_block(mode_str, border_color);
let inner = block.inner(area);
self.last_inner_width.set(inner.width);
frame.render_widget(block, area);
```

The `border_color` derivation (Vim-mode-aware in `render`, fixed `DarkGray` in `render_inert`) stays at the call sites, so the helper does not need to know about `EditMode`.

The `border_color` logic stays unchanged:

| State | Color |
| --- | --- |
| Active, Emacs / Vim-Insert | `Color::Green` |
| Active, Vim-Normal | `Color::Yellow` |
| Active, Vim-Visual | `Color::LightYellow` |
| Inactive | `Color::DarkGray` |
| Inert (overlay owns cursor) | `Color::DarkGray` |

The mode badge (` INSERT `, ` VIM·NORMAL `, etc.) renders as the title on the top rule — same chrome, less clutter, no side glyphs.

#### Math impact — anchored on the new constants (gemini rec #3, #5)

`required_height` (line 1418-1437) becomes:

```rust
pub fn required_height(&self, width: u16) -> u16 {
    let inner_w = width.saturating_sub(BORDER_OVERHEAD_COLS);  // was - 2
    if inner_w == 0 {
        return 1 + BORDER_OVERHEAD_ROWS;                       // was return 3
    }
    …
    inner + BORDER_OVERHEAD_ROWS                                // was inner + 2
}
```

With `BORDER_OVERHEAD_ROWS = 2` and `BORDER_OVERHEAD_COLS = 0`, the runtime values are unchanged from today's `+ 2` / `if inner_w == 0 { return 3 }` body — only the `width.saturating_sub(0)` is a no-op pass-through. **This means a future swap to `Borders::TOP` alone, or `Borders::NONE`, requires editing only the two constants and the `borders(...)` flag in `build_block`, with rendering and arithmetic auto-tracking.** Eliminates the desync risk gemini flagged.

Inside `render` and `render_inert`, all access to inner geometry already goes through `let inner = block.inner(area);` (lines 1466, 1605) — ratatui auto-adjusts `inner.width` (gains the 2 reclaimed cells, since side borders are gone) and keeps `inner.height` at `area.height - 2`. **No other math change is needed inside the render paths.**

#### Test impact (file-local) — minimal after `TOP | BOTTOM` + constants

Because **vertical** border count is unchanged (still 2 rows) and the `+ 2` literal becomes `+ BORDER_OVERHEAD_ROWS` (which equals 2), all five `required_height` asserts that pin the vertical math stay correct. Only `inner_w` shifts (from `width − 2` to `width`):

| Line | Today | After | Notes |
| --- | --- | --- | --- |
| 1702 | `required_height(80) == 3` | **3** (unchanged) | 1 inner + 2 borders |
| 1711 | `required_height(82) == 5` (inner_w 80, 200/80→3 rows) | **5** (unchanged) | inner_w 82 now, 200/82→3 rows still. Comment update: `inner width = 80` → `inner width = 82` |
| 1715 | fn name `..._plus_borders` | unchanged | still 2 borders, name still accurate |
| 1718 | `required_height(82) == 7` (clamp 5 + 2) | **7** (unchanged) | |
| 1726 | `required_height(22) == 3` (CJK, inner_w 20) | **3** (unchanged) | inner_w 22 now, still fits 1 row |
| ~1806 | `last_inner_width_for_test() == 18` (area w=20 − 2 sides) | **20** | **only assert that changes** |

Per gemini #5, asserts where the literal couples to the border footprint (`5`, `7`, `3`) should be re-expressed in terms of the constant: `assert_eq!(bar.required_height(80), 1 + BORDER_OVERHEAD_ROWS);` etc. This makes the tests a regression detector for the constants themselves.

So the test delta is **1 changed assertion** (line ~1806: 18 → 20), **1 changed comment** (line 1711), and an **optional sweep** that re-expresses the magic numbers in terms of `BORDER_OVERHEAD_ROWS`. Substantially safer refactor than `Borders::TOP` alone.

**Resolved (codex):** `input_bar_wrap::wrap` is safe at `width = 1` (only asserts `width > 0`, has a width-1 test) — no guard needed in `required_height`.

#### Layout impact at consumers

`session_detail.rs:2006` and `dashboard.rs:514` allocate `Constraint::Length(input_height)` from `input_bar.required_height(content_area.width)`. Because the function returns one fewer row, the input bar shrinks by 1 row and the surrounding flexible area (`react_trace`, `activity_log`) gains 1 row automatically. **No layout call-site changes needed.**

### 3b. `session_detail.rs` — banner border treatment (REVIEWER DISAGREEMENT, needs user call)

The two reviewer recommendations diverge here:

- **Kimi (UX):** Keep `Borders::ALL`. Error banners are *transient, high-priority alerts* — the box provides visual weight that signals "stop and read." Users don't drag-select error banners with anything close to the frequency they drag-select composer drafts, so the copy-friendliness argument doesn't apply. Boxed banners gain *more* contrast against rule-only composer chrome, which is appropriate for alerts. Unboxing them for aesthetic consistency reduces alert salience.
- **Gemini (architecture):** Drop them too. Argues users *do* copy auth/session errors — for bug reports, support tickets, and forum posts. Leaving them on `Borders::ALL` undermines the primary copy-friendly goal and creates a jarring inconsistency that will rot ("why does *this* panel still have side `│`?").

Both arguments are real. **This is a UX/UX disagreement, not a Rust correctness question.** It is left as an open question for the user.

The two banner sites:

- **Line 1976** auth-error banner: red-bg / white-fg / bold, full-width, 3 rows. ephemeral modal alert.
- **Line 2195** "Session error" centered label, 3 rows tall. ephemeral modal alert.
- **Line 2176** already `Borders::NONE` — no change either way.

**Three resolutions for the user to choose from:**

1. **Keep `Borders::ALL`** (kimi's recommendation, brief's original position) — alerts retain visual weight; banner sites are unchanged.
2. **Switch to `Borders::TOP | Borders::BOTTOM`** (gemini's recommendation) — consistency with the composer; users copying error text get clean lines.
3. **Hybrid: `Borders::LEFT | Borders::TOP | Borders::BOTTOM`** (kimi's compromise rec) — sidebar-alert look à la modern web alerts; preserves visual distinctiveness but the *right* edge is open so multi-line copy from a long error message still works for the right side.

If unstated, default is **(1) keep ALL** because that matches the brief's original design and is the safer default. The brief proceeds assuming (1); if the user picks (2) or (3), the change to `session_detail.rs` is two near-identical 1-line edits to the `borders(...)` flag at lines 1976 and 2195.

## 4. Options considered

| Option | Description | Verdict |
| --- | --- | --- |
| **A′** (chosen, post-kimi) | `input_bar.rs` → `Borders::TOP \| Borders::BOTTOM` + reversed-colour mode badge; `session_detail.rs` banners untouched. | **Ship.** |
| A | `input_bar.rs` → `Borders::TOP` only. | Rejected — bottom row of composer fuses with status bar at max height. |
| B | Same as A′ plus runtime `copy_mode` toggle (key `^Y`) that strips borders globally. | Defer — no user demand yet, adds state machine. |
| C | Replace `│` with styled space column. | Reject — selection still picks up spaces, looks unfinished. |
| D | Drop `Block` borders entirely; render a manual title row above the textarea. | Reject — duplicates ratatui semantics for no win. |
| E | Touch `session_detail` banners too. | Reject — user does not copy from error banners; box is the right UX cue (kimi concurs). |

## 5. Risk audit

1. **Test updates** — three asserts and one fn name in `input_bar.rs`. Mechanical.
2. **Border-style colour on top rule alone** — visual smoke test needed: Vim-Normal yellow rule, Vim-Visual light-yellow, Insert green, Inactive dark-gray. Confirm the title's background remains readable when the rule is colored.
3. **Cursor placement** (`render` line 1580-1582) computes from `inner.x` / `inner.y`. With `Borders::TOP`, `inner.y = area.y + 1` and `inner.x = area.x` (no left border). Already correct via `block.inner(area)`.
4. **`required_height` empty-width edge** — currently returns 3 when `width <= 2`. After change, returns 2 when `width == 0`. For `width == 1` the new code computes a wrap layout against `inner_w = 1`, which is a degenerate but well-defined input. Verify `input_bar_wrap::wrap` handles `width = 1` without panicking. If unclear, keep an `if width == 0 { return 2; } else if width == 1 { return 2; }` short-circuit.
5. **Cross-platform** — `─` (U+2500) is already required by current `Borders::ALL`. No new font requirements.
6. **Snapshot/golden tests outside this file** — none currently snapshot the input_bar borders directly. The recent `detail_pane_scroll` fixture is in a different component and unaffected.

## 6. Acceptance criteria

- Drag-select 5 lines in the input_bar composer → clipboard contains zero `│` characters and zero leading/trailing whitespace beyond what the user typed.
- Active/inactive/inert focus colour cues remain visually obvious.
- Vim mode badge (` VIM·NORMAL ` / ` VIM·VISUAL ` / ` INSERT `) still readable in the title.
- All `input_bar.rs` tests pass after asserted-value updates.
- Existing `session_detail.rs` and `dashboard.rs` integration tests pass without changes (because `required_height` consumers don't pin the value).

## 7. Out of scope (filed as follow-ups)

- `react_trace/render.rs` (lines 288, 403) — primary copy source for trace bodies; same `TOP | BOTTOM` treatment is the right call but requires its own pass. **When this lands, it's the natural moment to extract a crate-wide `panel_block()` helper** (gemini #2). With two data points (input_bar + react_trace) we can settle the helper signature; doing it speculatively from one site is premature.
- `activity_log.rs`, `detail_pane.rs`, `agents_tree.rs`, `plan_*.rs` — same pattern, dashboard-side. Track in a single follow-up issue.
- Crate-wide `panel_block(focused, title, PanelStyle) -> Block` helper — see above; deferred to the second-panel pass to avoid premature abstraction (acknowledging gemini's "false economy" concern, but the cost of refactoring `input_bar.rs` + one other panel onto a helper later is bounded and small).
- Runtime `copy_mode` toggle (Option B).
- **Kimi rec #3 (deferred):** subtle background tint on composer interior (`Color::Rgb(30, 30, 30)`-style "tray" effect). Adds nice containment but interacts with non-truecolor terminals and tmux pass-through; revisit if the bottom rule alone proves insufficient.
- **Kimi rec #4 (deferred):** dimmed placeholder ("Press Esc to return") inside empty inert composer. Useful but scope-creep; defer until inert-empty state is observed to be confusing.

## 8. Resolved questions + remaining open call

1. ~~**`required_height` width=1 edge**~~ — Resolved (codex): `input_bar_wrap::wrap` is safe at `width = 1`.
2. ~~**Title legibility on coloured top rule**~~ — Resolved (kimi): use **reversed-colour fill** (`bg(border_color).fg(Color::Black)`) on the mode badge so it acts as a high-contrast lamp; bundled in v1.
3. **`session_detail.rs` banner border treatment** — **Reviewer disagreement, needs user decision.** Kimi: keep `Borders::ALL` for alert salience. Gemini: drop sides too because users *do* copy errors for bug reports. Brief's default if no answer = keep `Borders::ALL`. See §3b.
4. ~~**Test naming**~~ — Resolved (gemini #5): re-express literal `+ 2` test asserts in terms of `BORDER_OVERHEAD_ROWS` instead of renaming.
5. ~~**Local helper vs no helper**~~ — Resolved (gemini #1): yes, a small private `build_block()` helper inside `input_bar.rs` is justified by two near-duplicate sites; bundled in v1.

The single remaining open question for the user: **which session_detail.rs banner treatment do you prefer — (1) keep ALL, (2) TOP|BOTTOM for full consistency, or (3) hybrid LEFT|TOP|BOTTOM sidebar style?**

## 9. De-risking experiment (per gemini)

Before committing to v1, run the change locally on `input_bar.rs` and execute `cargo test -p spur-tui` to map the actual blast radius of hardcoded layout assertions. Expectation per the test-impact table above: ~1 assert failure (`last_inner_width = 18 → 20`). Anything beyond that is a signal that other tests we missed are coupled to the side-border footprint.
