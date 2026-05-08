# Image Paste Through Prompt Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow users to paste images (from clipboard or file path) into the Spur TUI prompt, transmitting them as `ContentBlock::Image` to the AI agent via the existing ACP pipeline.

**Architecture:** Extend the existing `PasteRef` atom system with a parallel `ImageRef` atom. `InputBar` stores `ImageAttachment` structs (temp file path + metadata) keyed by ID. On submit, `assemble_blocks` reads each image file, resizes to ≤2048px, and base64-encodes into `ContentBlock::Image` blocks interleaved with text blocks. Clipboard reads use `arboard`; path-paste detection uses `image::image_dimensions`.

**Tech Stack:** Rust, ratatui, crossterm, `arboard 2`, `image 0.25` (png/jpeg/webp), `tempfile 3`, `base64 0.22` (already present).

---

## File Map

| File | Change |
|---|---|
| `crates/spur-tui/Cargo.toml` | Add `arboard`, `tempfile` deps; expand `image` features |
| `crates/spur-tui/src/components/input_bar.rs` | `ImageAttachment`, `RangeKind::ImageRef`, image store, key handler, path detection, cleanup |
| `crates/spur-tui/src/commands/submit_router.rs` | Extend `assemble_blocks` to handle `ImageRef` → `ContentBlock::Image` |
| `crates/spur-tui/src/views/session_detail.rs` | Pass drained images into the routing call |

---

### Task 1: Add Dependencies

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`

- [ ] **Step 1: Add `arboard` and `tempfile` to `[dependencies]`, expand `image` features**

In `crates/spur-tui/Cargo.toml`, `[dependencies]` section, add:

```toml
arboard = { version = "2", default-features = false, features = ["image"] }
tempfile = "3"
```

`tempfile` is currently only in `[dev-dependencies]`; it stays there too (tests still use it). Add it to `[dependencies]` separately.

Also find the existing `image` entry. It currently looks like:

```toml
image = { version = "0.25", optional = true }
```

under the `markdown` feature. Make it non-optional and add formats:

```toml
image = { version = "0.25", features = ["png", "jpeg", "webp"] }
```

Remove `image` from the `markdown` feature list (or leave it — it's now always present).

- [ ] **Step 2: Verify compile**

```bash
cargo check -p spur-tui 2>&1 | head -40
```

Expected: no errors (warnings about unused imports are fine at this stage).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/Cargo.toml
git commit -m "chore(spur-tui): add arboard + tempfile deps, expand image features"
```

---

### Task 2: Define `ImageAttachment` and Extend `RangeKind`

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write a failing test for `ImageAttachment` construction**

At the bottom of `input_bar.rs` in the existing `#[cfg(test)]` module (or add one):

```rust
#[cfg(test)]
mod image_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn image_attachment_fields_accessible() {
        let a = ImageAttachment {
            id: 0,
            source_path: PathBuf::from("/tmp/test.png"),
            mime_type: "image/png".to_string(),
            dimensions: (800, 600),
            byte_size: 1024,
            owned_temp: None,
        };
        assert_eq!(a.id, 0);
        assert_eq!(a.mime_type, "image/png");
        assert_eq!(a.dimensions, (800, 600));
    }

    #[test]
    fn range_kind_image_ref_is_not_atom() {
        let k = RangeKind::ImageRef(3);
        assert!(!k.is_atom());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p spur-tui image_tests 2>&1 | head -20
```

Expected: compile error — `ImageAttachment` and `RangeKind::ImageRef` not defined.

- [ ] **Step 3: Add `ImageAttachment` struct and `RangeKind::ImageRef` variant**

Near the top of `input_bar.rs`, after the existing `use` imports, add:

```rust
use std::path::PathBuf;
use tempfile::TempPath;
```

(Add only if not already imported.)

Add `ImageAttachment` near the `ProtectedRange` definition:

```rust
/// Holds a pasted image for the duration of the compose session.
/// `owned_temp` keeps a clipboard temp file alive until submit/clear.
pub struct ImageAttachment {
    pub id: usize,
    pub source_path: PathBuf,
    pub mime_type: String,
    pub dimensions: (u32, u32),
    pub byte_size: usize,
    pub owned_temp: Option<TempPath>,
}
```

Extend the `RangeKind` enum (currently lines 22–29):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RangeKind {
    #[default]
    Atom,
    PasteRef(usize),
    /// Inline image attachment; on submit, replaced with ContentBlock::Image.
    #[serde(skip)]
    ImageRef(usize),
}
```

Update `RangeKind::is_atom` to cover the new variant:

```rust
impl RangeKind {
    fn is_atom(&self) -> bool {
        matches!(self, Self::Atom)
    }
}
```

`is_atom` is already correct — `ImageRef` is not an atom. The `#[serde(skip)]` on `ImageRef` means it won't survive serialization (image atoms are not persisted in history, as per spec).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p spur-tui image_tests 2>&1
```

Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input-bar): add ImageAttachment struct and RangeKind::ImageRef"
```

---

### Task 3: Add Image Store to `InputBar`

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write failing tests**

Add to `image_tests` module:

```rust
    #[test]
    fn take_pending_images_drains_store() {
        // Can't construct a full InputBar easily; test the drain logic via the
        // public method on a minimal BTreeMap simulation.
        let mut images: std::collections::BTreeMap<usize, ImageAttachment> =
            std::collections::BTreeMap::new();
        images.insert(0, ImageAttachment {
            id: 0,
            source_path: PathBuf::from("/tmp/a.png"),
            mime_type: "image/png".to_string(),
            dimensions: (10, 10),
            byte_size: 100,
            owned_temp: None,
        });
        let drained: Vec<ImageAttachment> = images.drain(..).map(|(_, v)| v).collect();
        assert_eq!(drained.len(), 1);
        assert!(images.is_empty());
    }
```

- [ ] **Step 2: Run to see it pass (this is a standalone BTreeMap test)**

```bash
cargo test -p spur-tui take_pending_images 2>&1
```

Expected: PASS (tests the drain idiom, not InputBar yet).

- [ ] **Step 3: Add fields and methods to `InputBar`**

Find the `InputBar` struct definition. Add two fields alongside `pastes` and `next_paste_id`:

```rust
    images: BTreeMap<usize, ImageAttachment>,
    next_image_id: usize,
```

In `InputBar::new()` (or wherever defaults are set), initialize them:

```rust
    images: BTreeMap::new(),
    next_image_id: 0,
```

Add a `take_pending_images` method to `InputBar`'s `impl` block:

```rust
    /// Drain all pending image attachments. Called by the view on submit.
    pub fn take_pending_images(&mut self) -> Vec<ImageAttachment> {
        self.images.drain().map(|(_, v)| v).collect()
    }
```

Update `InputBar::submit()`. After `self.pastes.clear()` (line ~1344), add:

```rust
        self.images.clear(); // TempPath RAII deletes temp files
```

Find wherever `InputBar::clear()` is implemented and add:

```rust
        self.images.clear();
        self.next_image_id = 0;
```

- [ ] **Step 4: Verify compile**

```bash
cargo check -p spur-tui 2>&1 | head -30
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input-bar): add images store and take_pending_images to InputBar"
```

---

### Task 4: Implement `insert_image_atom` Helper

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn next_image_id_increments() {
        // Verify counter logic in isolation
        let mut counter: usize = 0;
        let id0 = counter;
        counter += 1;
        let id1 = counter;
        counter += 1;
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        assert_eq!(counter, 2);
    }
```

- [ ] **Step 2: Run (this passes immediately — it validates the counter pattern)**

```bash
cargo test -p spur-tui next_image_id 2>&1
```

Expected: PASS.

- [ ] **Step 3: Implement `insert_image_atom`**

Add to `InputBar`'s `impl` block:

```rust
    /// Store `attachment` and insert a `[Image #N]` protected atom into the textarea.
    fn insert_image_atom(&mut self, mut attachment: ImageAttachment) {
        let id = self.next_image_id;
        self.next_image_id += 1;
        attachment.id = id;
        let (w, h) = attachment.dimensions;
        let label = format!("[Image #{} · {}×{}]", id + 1, w, h);
        self.images.insert(id, attachment);

        // Reuse the same protected-range insertion machinery as insert_paste.
        // Build a ProtectedRange with ImageRef kind; name is the display label.
        let start = self.textarea.cursor_byte_offset(); // approximate; adjust to actual API
        self.textarea.insert_str(&label);
        let end = start + label.len();
        self.protected_ranges.push(ProtectedRange {
            start,
            end,
            kind: RangeKind::ImageRef(id),
            uri: String::new(),
            name: label,
        });
        // Sort ranges by start position (same invariant as paste insertion).
        self.protected_ranges.sort_by_key(|r| r.start);
    }
```

> **Note:** The exact textarea API for `cursor_byte_offset` and range tracking depends on the `tui-textarea` crate internals and how Spur wraps them. Look at how `insert_paste` (line ~1373) appends its `ProtectedRange` and mirror that exactly — the position arithmetic must match.

- [ ] **Step 4: Verify compile**

```bash
cargo check -p spur-tui 2>&1 | head -30
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input-bar): implement insert_image_atom helper"
```

---

### Task 5: Implement Clipboard Image Read (`try_paste_clipboard_image`)

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write failing test for path construction**

```rust
    #[test]
    fn image_attachment_from_rgba_bytes_dimensions() {
        // Simulate the conversion without a real clipboard.
        // Create a 4x4 RGBA image, encode to PNG, check dimensions survive.
        let rgba_bytes: Vec<u8> = vec![0u8; 4 * 4 * 4]; // 4x4 RGBA zeros
        let img = image::RgbaImage::from_raw(4, 4, rgba_bytes).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        assert_eq!(dyn_img.width(), 4);
        assert_eq!(dyn_img.height(), 4);
    }
```

- [ ] **Step 2: Run to verify it passes**

```bash
cargo test -p spur-tui rgba_bytes 2>&1
```

Expected: PASS (validates image crate is usable).

- [ ] **Step 3: Implement `try_paste_clipboard_image`**

Add at module level in `input_bar.rs` (not inside `InputBar` impl — it's a free function):

```rust
use std::io::Write as _;

/// Read the current clipboard image (if any) and write it to a temp PNG file.
/// Returns an `ImageAttachment` with `owned_temp` keeping the file alive.
/// Returns `Err` if the clipboard has no image or arboard is unavailable.
fn try_paste_clipboard_image() -> anyhow::Result<ImageAttachment> {
    use arboard::Clipboard;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    let mut clip = Clipboard::new()?;
    let img_data = clip.get_image()?;

    let width = img_data.width as u32;
    let height = img_data.height as u32;

    // Construct RGBA image from raw bytes.
    let rgba = image::RgbaImage::from_raw(width, height, img_data.bytes.into_owned())
        .ok_or_else(|| anyhow::anyhow!("clipboard image has invalid dimensions"))?;
    let mut dyn_img = image::DynamicImage::ImageRgba8(rgba);

    // Resize if needed.
    const MAX_DIM: u32 = 2048;
    if dyn_img.width() > MAX_DIM || dyn_img.height() > MAX_DIM {
        dyn_img =
            dyn_img.resize(MAX_DIM, MAX_DIM, image::imageops::FilterType::Lanczos3);
    }

    // Encode to PNG bytes.
    let mut cursor = std::io::Cursor::new(Vec::new());
    dyn_img.write_to(&mut cursor, image::ImageFormat::Png)?;
    let png_bytes = cursor.into_inner();

    const MAX_B64_BYTES: usize = 10 * 1024 * 1024; // 10 MB encoded cap
    let encoded_len = (png_bytes.len() * 4).div_ceil(3);
    if encoded_len > MAX_B64_BYTES {
        anyhow::bail!(
            "image too large ({} bytes base64); max 10 MB",
            encoded_len
        );
    }

    // Write to temp file.
    let mut tmp = tempfile::Builder::new()
        .prefix("spur-img-")
        .suffix(".png")
        .tempfile()?;
    tmp.write_all(&png_bytes)?;
    let (_, temp_path) = tmp.into_parts();
    let source_path = temp_path.to_path_buf();
    let byte_size = png_bytes.len();
    let dimensions = (dyn_img.width(), dyn_img.height());

    Ok(ImageAttachment {
        id: 0, // set by insert_image_atom
        source_path,
        mime_type: "image/png".to_string(),
        dimensions,
        byte_size,
        owned_temp: Some(temp_path),
    })
}
```

- [ ] **Step 4: Verify compile**

```bash
cargo check -p spur-tui 2>&1 | head -30
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input-bar): implement try_paste_clipboard_image via arboard"
```

---

### Task 6: Add `Ctrl+Alt+V` Key Handler

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Locate the key event dispatch**

Find the match arm in `InputBar`'s key event handler that already catches `Ctrl+Alt+V`-style combinations. Look for the block handling `KeyModifiers::CONTROL | KeyModifiers::ALT`. It may already handle other shortcuts (e.g., `Ctrl+Alt+Enter`). Add the new arm adjacent to those.

- [ ] **Step 2: Add the key arm**

```rust
KeyEvent {
    code: KeyCode::Char('v'),
    modifiers,
    ..
} if modifiers.contains(KeyModifiers::CONTROL)
    && modifiers.contains(KeyModifiers::ALT) =>
{
    match try_paste_clipboard_image() {
        Ok(attachment) => {
            self.insert_image_atom(attachment);
        }
        Err(e) => {
            // Non-fatal: surface in the status/notification bar.
            // Use whatever the codebase's convention is for ephemeral notices.
            tracing::warn!("clipboard image paste failed: {e}");
            // If there's a self.notify(...) or self.set_status(...) method, call it here.
            // Otherwise, return a ComponentEvent that the parent view can display.
        }
    }
}
```

- [ ] **Step 3: Verify compile**

```bash
cargo check -p spur-tui 2>&1 | head -30
```

- [ ] **Step 4: Smoke test (manual)**

Run `cargo run -p spur-cli -- tui`, open a session, copy an image to the clipboard (screenshot tool or any image), press `Ctrl+Alt+V`. The compose bar should show `[Image #1 · WxH]` as a styled atom.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input-bar): Ctrl+Alt+V pastes clipboard image as [Image #N] atom"
```

---

### Task 7: Detect Image File Paths in Text Paste

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn try_as_image_path_returns_none_for_non_path() {
        let result = try_as_image_path("hello world this is not a path");
        assert!(result.is_none());
    }

    #[test]
    fn try_as_image_path_returns_none_for_text_file() {
        // Create a temp text file — should not be detected as image.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"hello").unwrap();
        let path_str = tmp.path().to_str().unwrap().to_string();
        let result = try_as_image_path(&path_str);
        assert!(result.is_none());
    }

    #[test]
    fn try_as_image_path_returns_path_and_dims_for_png() {
        // Create a minimal valid 1x1 PNG in a temp file.
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        let img = image::RgbaImage::from_raw(1, 1, vec![0u8; 4]).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut cursor = std::io::Cursor::new(Vec::new());
        dyn_img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        std::fs::write(tmp.path(), cursor.into_inner()).unwrap();
        let path_str = tmp.path().to_str().unwrap().to_string();
        let result = try_as_image_path(&path_str);
        assert!(result.is_some());
        let (path, dims) = result.unwrap();
        assert_eq!(path, tmp.path());
        assert_eq!(dims, (1, 1));
    }
```

- [ ] **Step 2: Run to verify failures**

```bash
cargo test -p spur-tui try_as_image_path 2>&1
```

Expected: compile error — `try_as_image_path` not defined.

- [ ] **Step 3: Implement `try_as_image_path`**

```rust
/// If `text` (trimmed) is a path to an existing image file, returns `(path, dimensions)`.
/// Detection uses magic bytes via `image::image_dimensions`, not extension.
fn try_as_image_path(text: &str) -> Option<(PathBuf, (u32, u32))> {
    let path = PathBuf::from(text.trim());
    if !path.exists() {
        return None;
    }
    // image_dimensions reads enough bytes to detect the format.
    image::image_dimensions(&path).ok().map(|dims| (path, dims))
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p spur-tui try_as_image_path 2>&1
```

Expected: all three pass.

- [ ] **Step 5: Hook into the paste handler**

In the existing paste handler (inside `InputBar`, where `Event::Paste(text)` is handled — look for where `self.insert_paste(&text)` is called). Before calling `insert_paste`, add:

```rust
// Check if the pasted text is a path to an image file.
if let Some((img_path, dims)) = try_as_image_path(&text) {
    let byte_size = std::fs::metadata(&img_path)
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    let attachment = ImageAttachment {
        id: 0, // set by insert_image_atom
        source_path: img_path,
        mime_type: "image/png".to_string(), // encode_image_attachment re-encodes to PNG anyway
        dimensions: dims,
        byte_size,
        owned_temp: None, // file not owned by us
    };
    self.insert_image_atom(attachment);
    return; // don't insert raw path text
}
// Fall through to normal text paste.
self.insert_paste(&text);
```

- [ ] **Step 6: Verify compile + smoke test**

```bash
cargo check -p spur-tui 2>&1 | head -20
```

Manual test: paste a PNG file path into the compose bar — it should become `[Image #N · WxH]` instead of raw text.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input-bar): detect image file paths in paste, insert as atom"
```

---

### Task 8: Extend `assemble_blocks` to Emit `ContentBlock::Image`

**Files:**
- Modify: `crates/spur-tui/src/commands/submit_router.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod image_block_tests {
    use super::*;
    use crate::components::input_bar::{ImageAttachment, RangeKind, ProtectedRange};
    use std::io::Write as _;

    fn make_png_file() -> (tempfile::NamedTempFile, (u32, u32)) {
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        let img = image::RgbaImage::from_raw(2, 2, vec![128u8; 16]).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut cursor = std::io::Cursor::new(Vec::new());
        dyn_img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        std::fs::write(tmp.path(), cursor.into_inner()).unwrap();
        (tmp, (2, 2))
    }

    #[test]
    fn assemble_blocks_emits_image_block_for_image_ref() {
        let (tmp, dims) = make_png_file();
        let label = "[Image #1 · 2×2]";
        let attachment = ImageAttachment {
            id: 0,
            source_path: tmp.path().to_path_buf(),
            mime_type: "image/png".to_string(),
            dimensions: dims,
            byte_size: 0,
            owned_temp: None,
        };
        let images = vec![attachment];
        let text = format!("before {} after", label);
        let ranges = vec![ProtectedRange {
            start: "before ".len(),
            end: "before ".len() + label.len(),
            kind: RangeKind::ImageRef(0),
            uri: String::new(),
            name: label.to_string(),
        }];

        let blocks = assemble_blocks(&text, &ranges, &images);

        assert_eq!(blocks.len(), 3, "expected Text + Image + Text");
        assert!(matches!(&blocks[0], spur_acp::ContentBlock::Text(_)));
        assert!(matches!(&blocks[1], spur_acp::ContentBlock::Image(_)));
        assert!(matches!(&blocks[2], spur_acp::ContentBlock::Text(_)));
    }

    #[test]
    fn assemble_blocks_no_images_unchanged() {
        let text = "hello world";
        let ranges: Vec<ProtectedRange> = vec![];
        let images: Vec<ImageAttachment> = vec![];
        let blocks = assemble_blocks(text, &ranges, &images);
        assert_eq!(blocks.len(), 1);
    }
}
```

- [ ] **Step 2: Run to verify failures**

```bash
cargo test -p spur-tui image_block_tests 2>&1 | head -30
```

Expected: compile error — `assemble_blocks` signature doesn't match yet.

- [ ] **Step 3: Update `assemble_blocks` signature and body**

Change the current signature:

```rust
pub fn assemble_blocks(text: &str, ranges: &[ProtectedRange]) -> Vec<ContentBlock>
```

to:

```rust
pub fn assemble_blocks(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
) -> Vec<ContentBlock>
```

Add the import at the top of `submit_router.rs`:

```rust
use crate::components::input_bar::ImageAttachment;
use base64::{engine::general_purpose::STANDARD, Engine as _};
```

In the body, add a branch for `RangeKind::ImageRef(id)`. Where the code currently handles a `ProtectedRange` (the match on `range.kind`), add:

```rust
RangeKind::ImageRef(id) => {
    // Find the attachment for this id.
    match images.iter().find(|a| a.id == *id) {
        Some(att) => {
            match encode_image_attachment(att) {
                Ok(block) => out.push(block),
                Err(e) => {
                    tracing::error!("image encode failed for id={}: {e}", id);
                    out.push(ContentBlock::Text(TextContent::new(
                        format!("[image encode error: {e}]"),
                    )));
                }
            }
        }
        None => {
            tracing::warn!("ImageRef(id={}) not found in images list", id);
        }
    }
}
```

Add the encoding helper (module-level free function in `submit_router.rs`):

```rust
fn encode_image_attachment(att: &ImageAttachment) -> anyhow::Result<ContentBlock> {
    use image::imageops::FilterType;

    let bytes = std::fs::read(&att.source_path)?;
    let mut img = image::load_from_memory(&bytes)?;

    const MAX_DIM: u32 = 2048;
    if img.width() > MAX_DIM || img.height() > MAX_DIM {
        img = img.resize(MAX_DIM, MAX_DIM, FilterType::Lanczos3);
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)?;
    let png_bytes = cursor.into_inner();

    const MAX_B64_BYTES: usize = 10 * 1024 * 1024;
    let encoded_len = (png_bytes.len() * 4).div_ceil(3);
    if encoded_len > MAX_B64_BYTES {
        anyhow::bail!("image too large ({encoded_len} bytes); max 10 MB");
    }

    let data = STANDARD.encode(&png_bytes);
    Ok(ContentBlock::Image(spur_acp::ImageContent {
        data,
        mime_type: "image/png".to_string(),
        ..Default::default()
    }))
}
```

> **Note:** Check the actual fields of `spur_acp::ImageContent`. If it doesn't implement `Default`, construct all fields explicitly: `spur_acp::ImageContent { data, mime_type: "image/png".to_string(), uri: None }` (or whatever the struct contains).

- [ ] **Step 4: Fix all call sites of `assemble_blocks`**

Search for every call to `assemble_blocks`:

```bash
grep -n "assemble_blocks(" crates/spur-tui/src/ -r
```

Each call site must pass an empty slice for `images` until Task 9 wires in the real images:

```rust
assemble_blocks(text, ranges, &[])
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p spur-tui image_block_tests 2>&1
```

Expected: both tests pass.

- [ ] **Step 6: Verify full compile**

```bash
cargo check -p spur-tui 2>&1 | head -30
```

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/commands/submit_router.rs
git commit -m "feat(submit-router): assemble_blocks handles ImageRef -> ContentBlock::Image"
```

---

### Task 9: Wire Images into the Submit Dispatch

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Locate where `route_with_caps` is called**

In `session_detail.rs` around line 1462–1494, find the call to `route_with_caps(text, ranges, registry, interrupt, caps)`. Identify how `text` and `ranges` reach this call — likely via `input_bar.submit()` and a stored `submit_capture`.

- [ ] **Step 2: Drain images immediately after submit**

Immediately after the line that calls `input_bar.submit()` (or wherever the view reads from the input bar on Enter), add:

```rust
let pending_images = self.input_bar.take_pending_images();
```

`take_pending_images` drains `InputBar.images` and returns `Vec<ImageAttachment>`.

- [ ] **Step 3: Thread `pending_images` to `assemble_blocks`**

`route_with_caps` internally calls `assemble_blocks`. You have two options:

**Option A (simpler):** If `route_with_caps` returns `SubmitDecision::Send { blocks, interrupt }` and the `blocks` are assembled inside it, you'll need to thread `images` through `route_with_caps` → `assemble_blocks`. Add `images: &[ImageAttachment]` to both signatures and pass `&pending_images` at the top call site.

**Option B:** If `session_detail.rs` calls `assemble_blocks` directly (not via `route_with_caps`), pass `&pending_images` directly.

Find which applies by checking the actual call chain. Then apply the appropriate change.

At the `session_detail.rs` call site, replace:

```rust
// before
assemble_blocks(text, ranges, &[])
```

with:

```rust
// after
assemble_blocks(text, ranges, &pending_images)
```

Also update `route_with_caps` signature if it wraps `assemble_blocks`:

```rust
pub fn route_with_caps(
    text: &str,
    ranges: &[ProtectedRange],
    registry: &CommandRegistry,
    interrupt: bool,
    caps: Option<&SpurAgentCaps>,
    images: &[ImageAttachment],    // NEW
) -> SubmitDecision
```

Pass through to `assemble_blocks`.

- [ ] **Step 4: Verify compile**

```bash
cargo check -p spur-tui 2>&1 | head -30
cargo check -p spur-cli 2>&1 | head -20
```

- [ ] **Step 5: Full integration smoke test (manual)**

```bash
cargo run -p spur-cli -- tui
```

1. Copy a PNG to clipboard.
2. Press `Ctrl+Alt+V` — verify `[Image #1 · WxH]` appears.
3. Type some text before and after the atom.
4. Press Enter to submit.
5. Verify the message is sent without panic. Check agent receives an image block (add a `tracing::debug!` in `encode_image_attachment` temporarily if needed).

Also test the path-paste trigger:
1. Find a PNG file path on disk.
2. Paste the path string into the compose bar.
3. Verify it becomes `[Image #1 · WxH]` atom instead of raw text.
4. Submit and verify.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/commands/submit_router.rs
git commit -m "feat(session-detail): wire pending images into submit dispatch"
```

---

### Task 10: Cleanup — Atom Deletion and Session History

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write failing test for atom-deletion cleanup**

```rust
    #[test]
    fn images_cleaned_on_clear() {
        // We can't easily construct InputBar, but we can verify the BTreeMap drain.
        let mut images: std::collections::BTreeMap<usize, ImageAttachment> =
            std::collections::BTreeMap::new();
        images.insert(0, ImageAttachment {
            id: 0, source_path: PathBuf::from("/tmp/x.png"),
            mime_type: "image/png".to_string(), dimensions: (1, 1),
            byte_size: 0, owned_temp: None,
        });
        images.clear();
        assert!(images.is_empty());
    }
```

- [ ] **Step 2: Run (this passes immediately)**

```bash
cargo test -p spur-tui images_cleaned 2>&1
```

- [ ] **Step 3: Handle `ImageRef` removal when user deletes the atom character**

Find where `ProtectedRange` deletions are handled in `InputBar` (look for code that removes ranges whose byte span has been edited away — typically called from the character-deletion or backspace handling path).

When a range with `kind: RangeKind::ImageRef(id)` is removed from `protected_ranges`, also remove the corresponding attachment:

```rust
if let RangeKind::ImageRef(id) = removed_range.kind {
    self.images.remove(&id);
    // TempPath inside ImageAttachment is now dropped, deleting the temp file.
}
```

- [ ] **Step 4: Verify session history excludes image atoms**

`InputStateSnapshot` is serialized for history navigation. Since `RangeKind::ImageRef` has `#[serde(skip)]`, it will not survive a round-trip through serde. Verify this is handled gracefully:

When history is restored, any `ImageRef` ranges that survive deserialization (they won't — they'll be skipped) would reference IDs that no longer exist in `images`. Add a guard in `insert_image_atom` / submit path:

In `assemble_blocks`, the `images.iter().find(|a| a.id == *id)` already handles a missing attachment gracefully (logs a warning, produces no block). No additional change needed, but add a test:

```rust
    #[test]
    fn assemble_blocks_missing_image_id_produces_no_block() {
        let text = "hello";
        let ranges: Vec<ProtectedRange> = vec![];
        let images: Vec<ImageAttachment> = vec![];
        // ImageRef in ranges but no matching attachment — should not panic
        let ranges_with_ref = vec![ProtectedRange {
            start: 0, end: 5,
            kind: RangeKind::ImageRef(99), // no attachment with id=99
            uri: String::new(),
            name: "[Image #100 · 1×1]".to_string(),
        }];
        let blocks = assemble_blocks(text, &ranges_with_ref, &images);
        // Should produce zero image blocks; at most a warning.
        assert!(blocks.iter().all(|b| !matches!(b, spur_acp::ContentBlock::Image(_))));
    }
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p spur-tui 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 6: Final compile check across workspace**

```bash
cargo check --workspace 2>&1 | grep "^error" | head -20
```

Expected: no errors.

- [ ] **Step 7: Final commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs crates/spur-tui/src/commands/submit_router.rs
git commit -m "feat(input-bar): cleanup ImageAttachment on atom deletion; guard missing IDs"
```

---

## Follow-Up (Not in This Plan)

- Move `encode_image_attachment` call to `tokio::task::spawn_blocking` at the submit call site (currently synchronous; acceptable for ≤2048px images with 10 MB cap)
- Surface clipboard failure as a visible TUI status bar message
- WSL clipboard support via PowerShell fallback
- Telegram bot image input path
