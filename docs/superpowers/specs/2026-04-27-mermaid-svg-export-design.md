# Mermaid SVG Export — Design Spec

**Status:** Approved 2026-04-27. Ready for plan.
**Owner:** spur-tui maintainers.
**Companion to:** `2026-04-27-mermaid-inline-rendering-v2-design.md` (the v2 inline pipeline that this feature extends).

---

## Problem

After the v2 inline-mermaid pipeline shipped (commit `bf5a2111` + tuning follow-up `8d1d30d8` on `main`), users still see soft pixels when they cmd-click an inline mermaid diagram in iTerm2 to "open image". iTerm2's image cmd-click is hardwired to its embedded PNG — the embedded raster is at most 3200 px wide (top `RASTER_BUCKET`), which renders soft on 4K Retina displays at >100% zoom.

The mermaid pipeline already produces an SVG (vector) as the intermediate artefact before `resvg` rasterises it. Surfacing that SVG to the user — with low UX friction — gives lossless zoom in any external SVG-capable app (Preview, Safari, Chrome, Inkscape, …).

## Non-goals

- **Inline / pre-cmd-click vector zoom inside the terminal.** iTerm2's inline-image protocol is raster-only (PNG/JPEG/GIF) and ratatui-image only supports raster protocols. Out of scope.
- **OSC 8 hyperlink footer below inline images** (cmd-clickable file:// link). Evaluated and rejected: ratatui's `Buffer` does not preserve raw escape sequences across diff renders, requires a custom widget bypassing ratatui's frame model, and risks visual leak of the underline state into adjacent cells. Not worth the complexity for a secondary path.
- **Caching SVG inside `MermaidState::Ready`.** Evaluated and rejected: would require changes to `render_mermaid` signature, the `MermaidRenderCompleted` action variant, and the `ImageCache` assumptions. Larger blast radius than the on-demand re-render approach.
- **Per-diagram cursor in the inline session reader** (so `o` works without entering the overlay). Evaluated and rejected: requires a new selection mode in the streaming markdown reader. Out of scope.
- **Bumping raster DPI** higher than the current top bucket. The user explicitly declined this in the brainstorming round; vector zoom is the right semantic.

## User flow

1. User reads a session containing a mermaid diagram in the inline session view.
2. The diagram looks soft when cmd-clicked → user hits `Alt+v` (existing keybind) → mermaid overlay viewer opens with the most recent Ready diagram focused.
3. User cycles to the desired diagram if needed (existing `Tab` / `Shift+Tab` cycle behaviour).
4. **NEW:** User presses `o`. A 1-line footer in the overlay shows `↻ rendering SVG…` for as long as the off-thread render is in flight (typically 5–50 ms).
5. The OS default SVG handler launches with the rendered diagram. Footer updates to `✓ opened <filename>`.
6. If the launcher fails (no registered handler, etc.): footer shows `❌ open failed — SVG saved to <path>` so the user can copy the path manually.

Total keystrokes for first time: `Alt+v`, optional `Tab` × n, `o` — three to four. Re-press of `o` on the same focused diagram is **instant** because the temp file is reused.

## Architecture

```
                ┌────────────────────────────────────────────┐
                │   MermaidViewerView::handle_key('o')       │
                │   when self.focused = Some(id)             │
                └─────────────────────┬──────────────────────┘
                                      │ Action::ExportFocusedMermaidSvg { session, id }
                                      ▼
                ┌────────────────────────────────────────────┐
                │   App::handle_export_focused_mermaid_svg   │
                │   - look up code + generation in registry  │
                │   - compute idempotent path                │
                │   - if file exists & non-empty: opener     │
                │   - else: spawn_blocking → render_svg_only │
                └─────────────────────┬──────────────────────┘
                                      │ Action::MermaidSvgExported { session, id, result }
                                      ▼
                ┌────────────────────────────────────────────┐
                │   MermaidViewerView consumes result:       │
                │   - Ok(path)  → opener::open(path)         │
                │     update overlay footer to ✓             │
                │   - Err(e)    → update overlay footer to ❌│
                └────────────────────────────────────────────┘
```

### Data flow

- `MermaidState::Ready { code, image_generation, .. }` already exists and is the source of truth.
- The viewer never holds the SVG bytes; the file on disk is the cache.
- The overlay footer's status string is owned by `MermaidViewerView` (new field `export_status: Option<ExportStatus>`).

## Components

### 1. New action variants — `crates/spur-tui/src/action.rs`

```rust
ExportFocusedMermaidSvg {
    session: SessionId,
    id: MermaidId,
},
MermaidSvgExported {
    session: SessionId,
    id: MermaidId,
    result: Result<std::path::PathBuf, String>,
},
```

### 2. Module-internal helper in `mermaid.rs`

```rust
pub(crate) fn render_svg_only(code: &str) -> Result<String, String>;
```

Calls `render_with_options + fix_svg_font_families` in `panic::catch_unwind`. Returns the SVG string. **No raster** — this is the cheap part of the pipeline. Existing `render_to_svg_inner` is refactored to call into `render_svg_only`, so behaviour for the existing raster path is identical.

### 3. Idempotent path helper in `mermaid.rs`

```rust
pub(crate) fn export_svg_path(session: &SessionId, id: MermaidId, generation: u64) -> std::path::PathBuf;
```

Returns `${TMPDIR}/spur-mermaid-${session_short}-${id}-${gen}.svg` where `session_short` is the first 8 hex chars of the session id. The generation key auto-invalidates the cached file on bucket-up re-raster (which bumps `image_generation`).

### 4. Atomic write helper in `mermaid.rs`

```rust
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()>;
```

Writes to `<path>.tmp`, calls `File::sync_all`, then `rename`. Standard pattern.

### 5. Cross-platform launcher

Add the `opener` crate (`opener = "0.7"`) as a new spur-tui dependency. `opener::open(&path)` returns immediately after spawning the launcher subprocess — non-blocking, no `.await`. Handles macOS `open`, Linux `xdg-open` / `gio open`, WSL `wslview`, Windows `start` automatically.

### 6. App handler — `crates/spur-tui/src/app.rs`

`Action::ExportFocusedMermaidSvg`:
1. Look up `(code, image_generation)` from the active session's `mermaid_registry` (via `SessionDetailView`).
2. Compute the path via `export_svg_path`.
3. If file exists and `fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false)`: dispatch `Action::MermaidSvgExported { Ok(path) }` immediately (no render needed).
4. Else: `tokio::task::spawn_blocking({ let code = code.clone(); move || render_svg_only(&code) })`. On completion, write atomically and dispatch `MermaidSvgExported`.
5. On `MermaidSvgExported`:
    - `Ok(path)`: call `opener::open(&path)`. If opener returns Err, convert to `Err(format!(...))` and update overlay footer; otherwise success.
    - `Err(msg)`: update overlay footer.

### 7. View — `crates/spur-tui/src/views/mermaid_viewer.rs`

New field on `MermaidViewerView`:
```rust
pub(crate) export_status: Option<ExportStatus>,
```

Where:
```rust
pub(crate) enum ExportStatus {
    Pending,                        // "↻ rendering SVG…"
    Opened { filename: String },    // "✓ opened <filename>"
    Failed { detail: String },      // "❌ open failed — <detail>"
}
```

`handle_key`:
- `KeyCode::Char('o')` when `self.focused.is_some()` → set `export_status = Some(Pending)` and emit `Action::ExportFocusedMermaidSvg`.
- `KeyCode::Char('o')` when `self.focused.is_none()` → no-op.
- `KeyCode::Char('q') | Esc` (existing) → also clears `export_status`.

Overlay rendering: extend the existing footer rendering inside `app.rs::render_mermaid_overlay` (the same function that already draws the focus-cycle hint). The footer line gains a second segment showing the export status. Falls back to the existing focus-cycle hint when status is `None`.

### 8. Help registration — `crates/spur-tui/src/components/help_overlay.rs`

Add `o` to the mermaid-overlay-context key map: `"o   Open vector SVG in default app"`.

## Error handling

- **SVG render fails** (mmdr panic, malformed SVG): captured by existing `panic::catch_unwind` in `render_with_options` wrapper; `render_svg_only` returns `Err(String)`. Footer shows `❌ render failed — <reason>`.
- **Atomic write fails** (disk full, perms): `MermaidSvgExported { Err(io_error.to_string()) }`. Footer shows the IO error.
- **opener fails** (no SVG handler registered, especially on minimal Linux): `MermaidSvgExported { Err(...) }`, footer includes the path so the user can copy it.
- **Generation skew** (re-raster bumped generation between key press and dispatch): file path will be different; the new path will be rendered fresh. No bug — generation key handles this.

## Testing

### Unit tests in `mermaid.rs`

- `render_svg_only_returns_non_empty_for_valid_diagram`: feed `flowchart TD\nA-->B`, assert SVG starts with `<svg`.
- `render_svg_only_returns_err_for_invalid_diagram`: feed garbage, assert `Err`.
- `export_svg_path_is_deterministic`: same inputs → same path.
- `export_svg_path_changes_on_generation_bump`: bump generation → different filename.
- `atomic_write_succeeds_on_clean_path`: round-trip through tmpfile.
- `atomic_write_replaces_existing_file_atomically`: pre-existing file at path; after call, content matches new bytes; no `.tmp` left behind.

### Unit tests in `views/mermaid_viewer.rs`

- `handle_key_o_with_focus_emits_export_action`.
- `handle_key_o_without_focus_is_noop`.
- `export_status_pending_after_key_press`.
- `export_status_cleared_on_esc`.
- `export_status_updates_on_completed_action_ok`.
- `export_status_updates_on_completed_action_err`.

### Integration test

- `mermaid_export_round_trip` (in `crates/spur-tui/tests/`): drive `Action::ExportFocusedMermaidSvg` through a stub action loop, assert that the temp file exists at the deterministic path and has SVG content. Skip the actual `opener::open` call (would launch an external app); inject a test-mode launcher hook that records the path instead.

### Manual verification

- (M1) macOS iTerm2: render diagram → `Alt+v` → `o` → Preview/browser opens crisp SVG.
- (M2) Mash `o` 5× rapidly: no partial-file launch; second-onward presses are instant.
- (M3) On a Linux VM without an SVG handler: footer shows `❌ open failed — SVG saved to <path>`.
- (M4) Re-raster (resize terminal triggering bucket-up) → press `o` again → new generation file rendered.

## Out-of-scope follow-ups

- Eager pre-render at first overlay open (zero-latency `o` always). Defer until usage shows the lazy approach is friction.
- Configurable export path (e.g., `~/Downloads/`). The temp dir default is fine for a "view this once" workflow.
- "Save as PNG at high DPI" alternative key. YAGNI; SVG is the user's stated need.

## Cost estimate

- LOC: ~250 added / ~10 modified across 5 files.
- New deps: `opener = "0.7"` (~5 KB compiled).
- Test count delta: +12 unit, +1 integration.
- Implementation rounds: 1 single plan, ~5 tasks (action variants → mermaid helpers → app handler → view wiring → tests). No multi-task type-migration block needed.
