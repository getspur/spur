# Image Paste Through Prompt

**Date:** 2026-05-09  
**Status:** Approved

## Overview

Add support for pasting images into Spur's TUI prompt. Images are represented as inline `[Image #N]` atoms in the compose buffer and transmitted to the AI agent as `ContentBlock::Image` (base64-encoded) via the existing ACP pipeline.

Triggers: clipboard image paste (new `Ctrl+Alt+V` keybinding) and text-paste of a file path that resolves to an image.

Platforms: macOS + Linux.

## Architecture

### Protocol Layer (already present)

`ContentBlock::Image(ImageContent { data: String, mime_type: String })` exists in `agent-client-protocol` v0.11 and is re-exported by `spur-acp`. No protocol changes needed.

### Data Model

New type in `spur-tui/src/components/input_bar.rs`:

```rust
struct ImageAttachment {
    id: usize,
    source_path: PathBuf,           // original path (clipboard temp or pasted path)
    mime_type: String,
    dimensions: (u32, u32),
    byte_size: usize,
    _owned_temp: Option<TempPath>,  // keeps clipboard temp file alive; None for path pastes
}
```

`InputBar` gains:
```rust
images: BTreeMap<usize, ImageAttachment>,  // parallel to existing `pastes`
image_id_counter: usize,
```

### Atom System

Add `RangeKind::ImageRef(usize)` to the existing protected-range enum alongside `RangeKind::PasteRef(usize)`. Displays as styled `[Image #N]` in the textarea — same visual treatment as paste atoms.

Session history: image atoms are **not** persisted in `InputStateSnapshot`. They are dropped on session history navigation (images would be unresolvable on restore).

### Trigger Points

| Trigger | Detection | Storage |
|---|---|---|
| `Ctrl+Alt+V` | `arboard::Clipboard::get_image()` | Write PNG bytes to `NamedTempFile`, keep `TempPath` in `ImageAttachment._owned_temp` |
| Text paste of image path | `image::image_dimensions(path)` on pasted text after normalization | Store path directly; `_owned_temp: None` |

Linux clipboard read is **best-effort**: `arboard` may fail under Wayland, headless/SSH sessions, or when no GUI clipboard provider is available. Failures surface as a non-fatal status bar message; the paste falls through to normal text paste.

### New Dependencies

Add to `spur-tui/Cargo.toml`:
- `arboard` — clipboard access (macOS + Linux X11/Wayland via feature flags)
- `image` already present as optional; expand to include JPEG and WebP decoding (not only PNG) — needed for path-paste validation (`image_dimensions()`). All images are re-encoded to PNG before transmission regardless of source format.

Image format detection uses magic bytes / `image::image_dimensions()`, not file extension.

### Submit Path

`assemble_blocks()` in `submit_router.rs` currently receives `(text, ProtectedRanges)` and produces `Vec<ContentBlock>`. It gains a reference to `images: &BTreeMap<usize, ImageAttachment>`.

When `RangeKind::ImageRef(id)` is encountered, all of the following runs inside a `spawn_blocking` task (see Threading below):
1. Read file bytes from `source_path`
2. Decode via `image` crate
3. Resize if either dimension exceeds 2048px (maintain aspect ratio)
4. Re-encode to PNG (all images transmitted as PNG regardless of source format)
5. Base64-encode → `ContentBlock::Image(ImageContent { data, mime_type: "image/png" })`

Text segments between image atoms become separate `ContentBlock::Text` blocks, preserving positional ordering.

**Byte cap:** reject images whose base64-encoded size would exceed 10 MB; surface error to user.

### Threading

Image I/O and encoding run on a Tokio `spawn_blocking` task, not inline in the TUI event loop. The submit action is deferred until encoding completes, then dispatched as a normal `Action::SendMessage`.

### Cleanup

| Event | Action |
|---|---|
| Atom deleted by user | `images.remove(id)`; `TempPath` RAII deletes temp file |
| `InputBar::clear()` | Drain all `images` |
| Submit | Images consumed (moved into block assembly); `images` drained |
| App shutdown | `TempPath` RAII handles temp file deletion automatically |

## Data Flow

```
Ctrl+Alt+V
  → arboard::Clipboard::get_image()         [best-effort; error → status msg]
  → NamedTempFile::new() → write PNG bytes
  → TempPath::keep()
  → ImageAttachment { id, source_path, mime_type, dimensions, _owned_temp }
  → images.insert(id, attachment)
  → textarea.insert_protected("[Image #N]", RangeKind::ImageRef(id))

--- or ---

Event::Paste(text)
  → normalize path
  → image::image_dimensions(path) OK?
  → ImageAttachment { id, source_path=path, _owned_temp: None }
  → images.insert(id, attachment)
  → textarea.insert_protected("[Image #N]", RangeKind::ImageRef(id))

On Enter:
  → assemble_blocks(text, ranges, &images)
  → spawn_blocking: read + decode + resize + encode + base64
  → ContentBlock::Image(ImageContent { data, mime_type })
  → interleaved with ContentBlock::Text blocks
  → Action::SendMessage { blocks: Vec<ContentBlock>, .. }
  → orchestrator → ACP NativeConnection → agent subprocess
```

## Out of Scope

- Drag-and-drop image attachment
- Image preview / thumbnail in the TUI
- Persistent attachment cache across sessions
- WSL clipboard support
- Telegram bot image input
- Session history restore of image atoms
