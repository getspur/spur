# Mermaid dual-mode rendering: image or text fallback

**Status:** design
**Date:** 2026-04-14
**Area:** `crates/spur-tui` — markdown / mermaid rendering

## Problem

`crates/spur-tui/src/components/mermaid.rs` always rasterizes Mermaid sources
to an RGBA `DynamicImage` through the `mmdr → usvg → resvg → tiny-skia`
pipeline. Whether the pixels are ever shown depends on `ratatui_image::Picker`
detecting a supported graphics protocol (kitty, sixel, iTerm2). When that
probe fails:

1. `render_mermaid` is still invoked, burning CPU and memory to produce
   pixels the TUI has no way to display.
2. The rendered pane shows only a placeholder line such as
   `[📊 mermaid #N · press Alt-v to view]`, but Alt-v cannot show anything
   either — there is no `StatefulProtocol` without a `Picker`.
3. The mermaid *source* — which is already plain, human-readable text
   (`A --> B`, `sequenceDiagram`, etc.) — is never surfaced, even though it
   is the best available signal in the absence of images.

## Goal

A terminal-capability-aware mermaid presentation with exactly two modes,
selected once at startup:

1. **Image mode** (image protocol detected): today's pipeline, unchanged.
2. **Text mode** (no image protocol): mermaid fences render as ordinary
   code blocks; the entire mermaid-specific machinery stays dormant.

## Non-goals

- No user-facing toggle or CLI flag. The mode is inferred from
  `Picker::from_query_stdio()` and never changes mid-session.
- No ASCII-art rendering of mermaid graphs. The source itself is the
  fallback.
- No mid-session mode switching. The terminal's image-protocol capability
  does not change at runtime in any scenario we support.
- No refactor of `mermaid.rs` internals (minimal-touch).

## Design

### Capability model

The existing `App.mermaid_picker: Option<Picker>` already encodes the mode.
No new enum, no new config:

| `mermaid_picker` | Mode       |
|------------------|------------|
| `Some(_)`        | Image mode |
| `None`           | Text mode  |

The value is set once in `App::new` via `Picker::from_query_stdio().ok()`.
It is never reassigned.

### Call-site gating

Four touch-points consult `mermaid_picker.is_some()`:

1. **`markdown_stream.rs` — fence classification.**
   - Image mode: unchanged. Allocate `MermaidId`, push
     `MermaidState::Pending { code }` into the registry, dispatch the
     render job, emit the inline placeholder line.
   - Text mode: delegate to the generic code-block renderer with
     info-string `mermaid`. No `MermaidId`, no registry entry, no job.

2. **`app.rs` — `MermaidViewerView` and dispatch.**
   - In text mode, `MermaidViewerView` is never constructed; the Alt-v
     key handler is a no-op.
   - `mermaid_tx` / `mermaid_rx` remain allocated (negligible cost); the
     dispatcher task runs but never receives messages.

3. **`help_overlay.rs` — key-binding advertisement.**
   - The "Alt-v · Mermaid viewer" row is suppressed when
     `mermaid_picker.is_none()`. The overlay is given the bit explicitly.

4. **`mermaid.rs` — no changes.** The module is dormant library code in
   text mode. `render_mermaid`, `MermaidState`, `FenceRender`, and
   `fence_placeholder_line` are only referenced from image-mode call sites.

### Text-mode rendering contract

A ```` ```mermaid ```` fence in text mode is handled identically to any
other code fence:

- **Header:** whatever the existing generic code-block renderer emits for
  info-string `mermaid`. No mermaid-specific chrome is introduced.
- **Body:** verbatim source, passed through the same highlight/style path.
  No highlighter recognises the `mermaid` grammar, so the result is plain
  code-styled text — which is exactly the desired outcome.
- **Trailing hint:** none. The reason for text mode is a global terminal
  property; annotating every fence would be noise.
- **Inline layout:** the fence occupies only the rows the text needs. No
  `ImageRow` pre-reservation. `FenceRender` variants are never
  constructed in text mode.

### Data flow

```
startup:  Picker::from_query_stdio() → Option<Picker>
                     │
                     ▼
            App.mermaid_picker
                     │
   ┌─────────────────┴─────────────────┐
   Some(picker)                       None
   (IMAGE MODE)                    (TEXT MODE)
   │                                   │
   markdown_stream:                    markdown_stream:
   ```mermaid → MermaidId +             ```mermaid → generic code-fence
   Pending { code } +                   (no registry touch)
   enqueue render job                  │
   │                                    ▼
   ▼                              Alt-v hidden + no-op
   render_mermaid →
   Ready { image } | Error
   │
   ▼
   inline: ImageRow / placeholder
   Alt-v: MermaidViewerView
```

Invariants in text mode:

- `MermaidId` is never minted.
- `MermaidState` is never constructed.
- `render_mermaid` is never called.
- `MermaidViewerView` is never instantiated.

The mode decision is made once at `App::new` and never revisited.

### Error handling & edge cases

- **Image-mode render errors.** Unchanged (`RenderError::{Render, Panic,
  Decode}` → `Error { message }` → warning placeholder).
- **Malformed mermaid source in text mode.** Nothing parses it, so there
  is no error path — the source renders as-is.
- **Terminal mis-detection.** Out of scope — an existing image-mode
  concern.
- **SSH / CI / screenshot pipelines.** These automatically land in text
  mode and see readable diagram sources instead of placeholders.
- **Session replay / saved transcripts.** No format change. The mermaid
  source is already in the event stream.

### Testing

Three new tests, all lightweight; existing `mermaid.rs` tests remain:

1. **`markdown_stream` in text mode treats ```mermaid as a code block.**
   Build a `MarkdownStream` with `mermaid_picker = None`, feed a mermaid
   fence, assert: (a) no entry is written to the mermaid registry,
   (b) rendered output matches that of a generic code fence with
   info-string `mermaid`.

2. **`render_mermaid` is never called in text mode.** With a `#[cfg(test)]`
   counter on the dispatcher worker, construct an `App` with
   `mermaid_picker = None`, feed a session containing a mermaid fence,
   assert the counter remains `0`.

3. **Help overlay hides Alt-v in text mode.** Snapshot or line-match
   assertion that the overlay omits the "Alt-v · Mermaid viewer" row
   when `mermaid_picker.is_none()`.

Retained tests (unchanged):
- `fix_font_families_replaces_inner_quotes`
- `malformed_svg_rasterization_does_not_panic`
- `ready_state_holds_inline_protocol_slot`

## Affected files

- `crates/spur-tui/src/components/markdown_stream.rs` — mermaid-fence
  branch in classification / render.
- `crates/spur-tui/src/app.rs` — conditional `MermaidViewerView`
  construction and Alt-v dispatch.
- `crates/spur-tui/src/components/help_overlay.rs` — conditional
  Alt-v row.
- `crates/spur-tui/src/components/mermaid.rs` — unchanged (minimal-touch).
