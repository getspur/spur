# Scroll Indicator Option B — UI/UX Review

## 1. Proposal recap

**The Problem:** The current `react_trace` pane uses ratatui's default `Scrollbar` widget, which renders a vertical track (`║`) and thumb (`█`) down the right edge of the pane whenever content overflows. Because terminal selection is rectangular, dragging a mouse across multi-line trace content captures these vertical glyphs into the clipboard, polluting the copied text. This violates the core principle established in the copy-friendly borders design language: *"no vertical glyphs anywhere selectable."*

**Option B Proposal:** Drop the `Scrollbar` widget entirely. Replace it with a text-based scroll-position indicator right-aligned on the bottom border row (e.g., `· 12/27 · 45%`). This utilizes ratatui's `.title_bottom(Line::from(...).right_aligned())` capability on the existing `Block`.

This review evaluates Option B across usability, industry conventions, and edge-case resilience.

## 2. ASCII wireframes

The following wireframes demonstrate the visual state of the `react_trace` pane at ~70 columns wide under Option B.

### A. No overflow (Indicator hidden)
When the content fits entirely within the viewport, the position indicator is omitted to reduce visual noise.
```text
─ ReAct trace ────────────────────────────────────────────────────────
 codex 12:00 single line of content
─ ▼ following ────────────────────────────────────────────────────────
```

### B. Overflow at top of content (offset = 0)
When content overflows but the viewport is at the absolute top.
```text
─ ReAct trace ────────────────────────────────────────────────────────
 codex 12:00 here is some content that wraps and continues
 codex 12:01 more content — user selects from line 1 down
 codex 12:02 and the clipboard is now clean
 codex 12:03 (only horizontal `─` glyphs at top/bottom rows
 codex 12:04 which are benign in pasted text)
 codex 12:05 ...
─ ▼ following ────────────────────────────────────────── · 6/27 · 22% ─
```

### C. Overflow mid-content (offset = mid)
```text
─ ReAct trace ────────────────────────────────────────────────────────
 codex 12:03 (only horizontal `─` glyphs at top/bottom rows
 codex 12:04 which are benign in pasted text)
 codex 12:05 ...
 codex 12:06 mid content
 codex 12:07 more mid content
 codex 12:08 almost there
─ ▼ following ───────────────────────────────────────── · 12/27 · 44% ─
```

### D. Overflow at bottom (offset = end)
```text
─ ReAct trace ────────────────────────────────────────────────────────
 codex 12:22 getting near the end
 codex 12:23 scrolling down
 codex 12:24 more content
 codex 12:25 almost at the bottom
 codex 12:26 the end
 codex 12:27 final line of trace
─ ▼ following ─────────────────────────────────────── · 27/27 · 100% ─
```
*(Note: 27/27 = bottom of viewport reaches line 27, the end of content).*

## 3. Industry references

How do other terminal-based tools handle scroll position when copy-pollution matters?

1. **`less` / `more` (System pagers):** Prioritize raw text output. Zero side borders or vertical scrollbars. `less -M` uses a bottom status line showing `lines i-j/N (k%)`.
2. **`vim` / `neovim` (Text editors):** Prioritize clean text buffers. Scroll position is right-aligned in the bottom statusline (e.g., `12,5  25%` or `Top`/`Bot`/`All`).
3. **`tig` (Git TUI):** Uses a bottom status bar. The right side features a bracketed line number and percentage `[12/100] 12%`. No vertical scrollbars in text views.
4. **`GitHub CLI` (`gh pr view`):** Uses glamour/glow for markdown rendering, piped into `less`. Inherits `less`'s copy-clean, no-vertical-chrome defaults.
5. **`bat` / `delta` (Modern syntax/diff pagers):** Strictly avoid right-side chrome to preserve pure copy semantics, relying on the pager for position metadata.
6. **`lazygit` (TUI Git client):** Uses scrollbars in some interactive list panes, but frequently relies on selected-row indices and bottom-bar metadata for dense text views.
7. **`ratatui` ecosystem defaults:** While many example apps showcase the `Scrollbar` widget (adding the `║` track), "minimal-chrome" or productivity-focused TUIs strip it to avoid the exact clipboard pollution issue SPUR is fixing.

**Conclusion:** "Minimal-chrome" tools uniformly reject vertical scrollbars in favor of bottom-anchored, text-based position indicators. Option B perfectly aligns with this established Unix/CLI lineage.

## 4. Format comparison & recommendation

There are several ways to format the text indicator:

*   `· 12/27 · 45%` (counts + percent, dot-separated)
*   `[12/27] 45%` (brackets)
*   `45% · 12/27` (percent first)
*   `12/27` (counts only)
*   `45%` (percent only)
*   `Top` / `45%` / `Bot` (vim-style adaptive)

**Recommendation: `· 12/27 · 45%`**
*   **Justification (Industry):** The interpunct (`·`) is a modern UI delimiter that feels lighter than brackets (`[]`), avoiding the visual implication of a clickable button.
*   **Justification (Density):** It provides both absolute size (`27` total lines) and relative position (`45%`). Removing the scrollbar removes the "thumb size" visual cue (which tells users how large the document is). Providing the absolute line count replaces this lost signal with precise data.
*   **Justification (Salience):** Formatting it with the same `Color::DarkGray` as the border itself keeps it recessive, ensuring it doesn't distract from the content or the active title badge.

## 5. Edge cases analysis

*   **Collision in narrow panes:** What if `▼ following` and `· 12/27 · 45%` together exceed the pane width?
    *   *Strategy:* Graceful degradation. If `width < 30`, drop the absolute counts and show only `· 45%`. If `width < 20`, drop the scroll indicator entirely (the viewport is too constrained to worry about scroll metadata).
*   **No overflow / tiny content:** What if `total_lines <= visible_height`?
    *   *Strategy:* The indicator must be completely hidden. Displaying `· 1/2 · 100%` on a 2-line pane is unnecessary noise. The block should revert to a simple `─` border.
*   **Visual crowding with `following`:** Both indicators on the same row.
    *   *Analysis:* Because one is strictly left-aligned and the other is strictly right-aligned, the connecting `─` border acts as a natural separator. It reads cleanly as a unified "status bar" (like tmux or vim).
*   **Color scheme:**
    *   *Strategy:* Dimmed (`DarkGray`). It must match the `border_style` so it reads as "border metadata" rather than active content.

## 6. Risks / counter-arguments

*   **Loss of size signal:** The visual height of a scrollbar thumb instantly communicates document length.
    *   *Mitigation:* The `12/27` text format directly mitigates this. While slightly less visceral than a tiny thumb, exact line counts are arguably more useful for text payloads.
*   **Loss of continuous tracking:** A scrollbar updates smoothly; a text indicator jumps in integer increments.
    *   *Mitigation:* Terminal text scrolling is inherently discrete (line-by-line or page-by-page). Keyboard navigation doesn't suffer from discrete position updates.
*   **Discoverability:** New users might not recognize `· 12/27 · 45%` as scroll state.
    *   *Mitigation:* The `%` sign is universally recognized as a scroll/progress indicator in terminal pagers.
*   **Mouse-only interaction:** Users can't click-and-drag a text indicator.
    *   *Mitigation:* Ratatui's default `Scrollbar` doesn't handle click-and-drag natively without custom event plumbing anyway. Mouse wheel scrolling works globally on the pane surface, which is the primary way users scroll in TUIs.

## 7. Top vs bottom title placement

Some TUIs place position metadata right-aligned on the *top* title row (e.g., `─ ReAct trace ─────────────── 45% ─`).

*   **Pros of Top:** Keeps the bottom border completely empty if `following` is off, reducing visual footprint.
*   **Pros of Bottom:**
    *   Historically consistent with Unix pagers (`less`, `vim`, `tmux` status bars).
    *   Groups "viewport state" metadata together. `following` and `scroll position` are both properties of *how you are viewing* the pane, not *what the pane is* (identity).
    *   Keeps the top border reserved strictly for identity, mode badges, and alerts.

**Conclusion:** Bottom placement is strongly preferred for viewport state metadata.

## 8. Verdict & open questions for human

**Verdict: Option B is highly recommended.** It entirely eliminates the clipboard pollution problem while maintaining (and arguably improving) precise positional awareness. It adheres strictly to the approved copy-friendly design language.

**Open questions for implementation:**
1. Do we calculate the percentage based on the *top* visible line (`offset / total`) or the *bottom* visible line (`(offset + height) / total`)? (Standard practice for `100%` at the end usually requires `(offset + visible) / total`).
2. Do we want to implement the graceful truncation (`· 45%` fallback) immediately, or just rely on ratatui's default `.right_aligned()` clipping for v1?
