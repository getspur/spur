# Mermaid SVG Export Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a single keystroke (`o`) in the Mermaid Viewer overlay that renders the focused diagram's SVG to a deterministic temp file and launches the OS default SVG handler — giving users vector-true zoom that iTerm2's PNG cmd-click cannot.

**Architecture:** Two new `Action` variants (`ExportFocusedMermaidSvg` → `MermaidSvgExported`) bridging the keypress and an off-thread SVG render via `tokio::task::spawn_blocking`, mirroring the existing `MermaidRenderRequest`/`MermaidRenderCompleted` pair. The temp file path is keyed on `(session_short, mermaid_id, image_generation)` so re-presses are stat-cached and bucket-up re-rasters auto-invalidate. Cross-platform launch via the `opener` crate. Footer status surfaced through a new `ExportStatus` field on `MermaidViewerView`.

**Tech Stack:** Rust 2024, ratatui, tokio, `mermaid-rs-renderer` 0.2.2 (already in workspace), `opener` 0.7 (new), std `Hasher` / `DefaultHasher` for `session_short`, std `fs::rename` for atomic writes.

**Spec:** `docs/superpowers/specs/2026-04-27-mermaid-svg-export-design.md`

**Repo path:** `/Volumes/Projects/spur` (workdir = repo root, branch = `main` at `337d0a88`).

**Test invocation pattern across this plan:**
```bash
scripts/spur-cargo test -p spur-tui --features markdown --lib <filter>
```
The wrapper handles sccache cross-worktree caching; do not invoke `cargo` directly.

**Lint invocation:**
```bash
scripts/spur-cargo clippy -p spur-tui --features markdown -- -D warnings
```

---

## File Map

| File | Responsibility | Mode |
|---|---|---|
| `crates/spur-tui/Cargo.toml` | Add `opener = "0.7"` dep | Modify |
| `crates/spur-tui/src/components/mermaid.rs` | New helpers: `render_svg_only`, `session_short`, `export_svg_path`, `atomic_write` + tests | Modify |
| `crates/spur-tui/src/action.rs` | Add `ExportFocusedMermaidSvg` and `MermaidSvgExported` variants | Modify |
| `crates/spur-tui/src/views/mermaid_viewer.rs` | Add `ExportStatus` enum + `export_status` field + `o` keybind handler | Modify |
| `crates/spur-tui/src/app.rs` | New action handlers + footer rendering update | Modify |
| `crates/spur-tui/src/components/help_overlay.rs` | Document `o` in the Mermaid Viewer section | Modify |
| `crates/spur-tui/tests/mermaid_svg_export.rs` | New integration test exercising the action round-trip | Create |

---

## Task 1: Add `opener` dependency

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`

- [ ] **Step 1: Add the dep to the markdown feature block**

Edit `crates/spur-tui/Cargo.toml`. Add a new dependency line below the existing `pulldown-cmark` line (around line 42):

```toml
opener = { version = "0.7", default-features = false }
```

Place it AFTER `pulldown-cmark` and BEFORE the `[dev-dependencies]` block. This keeps `opener` outside the `markdown` feature gate because it's needed for runtime launch independently of markdown rendering — but in practice the key handler path is `#[cfg(feature = "markdown")]` so the dep is only invoked under that feature. Putting it unconditional avoids a feature-flag goose chase if a non-markdown caller is ever added.

Final state of the relevant section:

```toml
pulldown-cmark = { version = "0.13", default-features = false, optional = true }
opener = { version = "0.7", default-features = false }

[dev-dependencies]
```

- [ ] **Step 2: Verify build succeeds**

Run: `scripts/spur-cargo build -p spur-tui --features markdown 2>&1 | tail -20`
Expected: `Finished` line appears; no compile errors.

- [ ] **Step 3: Verify lockfile pinned a 0.7.x version**

Run: `grep -A1 'name = "opener"' Cargo.lock | head -4`
Expected: `version = "0.7.X"` for some `X`.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/Cargo.toml Cargo.lock
git commit -m "feat(spur-tui): add opener crate for cross-platform SVG launch

Single new runtime dep for the upcoming mermaid SVG export feature.
Cross-platform 'open file in default app' replacing what would
otherwise be a hand-rolled cfg(target_os) Command branch (which
misses WSL wslview and Linux flatpak gio open)."
```

---

## Task 2: `render_svg_only` helper

**Files:**
- Modify: `crates/spur-tui/src/components/mermaid.rs:214-226` (refactor `render_to_svg_inner` into `render_svg_only`)
- Test: `crates/spur-tui/src/components/mermaid.rs` (existing `tests` module at the bottom)

- [ ] **Step 1: Write the failing tests**

Add these tests to the existing `mod tests` block in `crates/spur-tui/src/components/mermaid.rs` (just before the closing `}` of the module — after the existing `ready_state_holds_provenance_fields` test):

```rust
#[test]
fn render_svg_only_returns_non_empty_for_valid_diagram() {
    let svg = render_svg_only("flowchart TD\nA-->B").expect("valid input renders");
    assert!(svg.starts_with("<svg") || svg.starts_with("<?xml"),
        "should be SVG: starts with {:?}", svg.chars().take(20).collect::<String>());
    assert!(svg.contains("</svg>"), "should be a complete SVG document");
}

#[test]
fn render_svg_only_returns_err_for_garbage_input() {
    let result = render_svg_only("\u{0000}\u{0001}not a diagram\u{ffff}");
    assert!(result.is_err(), "garbage should return Err, got Ok");
}

#[test]
fn render_svg_only_does_not_panic_on_empty_input() {
    let result = std::panic::catch_unwind(|| render_svg_only(""));
    assert!(result.is_ok(), "should not panic, got panic");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib components::mermaid::tests::render_svg_only 2>&1 | tail -20`
Expected: 3 compile errors — `render_svg_only` is not defined.

- [ ] **Step 3: Refactor `render_to_svg_inner` into `render_svg_only`**

In `crates/spur-tui/src/components/mermaid.rs`, find the existing `render_to_svg_inner` function (currently around line 214). REPLACE the entire function with:

```rust
/// Stage 1: mermaid source → SVG string. Public to the crate so the SVG
/// export feature can reach it without going through the raster path.
///
/// Wraps the call in `panic::catch_unwind` so a malformed input cannot
/// unwind the caller. Returns `Err(String)` on either render error or
/// panic — the SVG export feature only needs the message, not the full
/// `RenderError` enum.
pub(crate) fn render_svg_only(code: &str) -> Result<String, String> {
    let code_owned = code.to_string();
    let result = panic::catch_unwind(move || {
        // Library defaults (50/50) produce cramped layouts for typical flowcharts.
        // Use the author's README baseline (60/80) plus a wide-aspect hint suited
        // to TUI panes (typically 2-3× wider than tall). See:
        //   https://github.com/1jehuang/mermaid-rs-renderer/blob/master/README.md
        let opts = mermaid_rs_renderer::RenderOptions::modern()
            .with_node_spacing(60.0)
            .with_rank_spacing(80.0)
            .with_preferred_aspect_ratio(2.5);
        mermaid_rs_renderer::render_with_options(&code_owned, opts)
            .map(|svg| fix_svg_font_families(&svg))
            .map_err(|e| e.to_string())
    });

    match result {
        Ok(outcome) => outcome,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(format!("renderer panic: {msg}"))
        }
    }
}

/// Internal raster-path adaptor — keeps the existing `render_mermaid` API
/// returning `RenderError` while delegating SVG production to
/// `render_svg_only`.
fn render_to_svg_inner(code: &str) -> Result<String, RenderError> {
    render_svg_only(code).map_err(RenderError::Render)
}
```

Note: `render_mermaid` already wraps `render_to_svg_inner` AND `rasterize_svg` in its own `catch_unwind`. With this refactor, `render_to_svg_inner` is redundantly wrapped (`render_svg_only` already catches panics), but the duplication is harmless — it converts panic types to `String` once at the SVG layer, then the outer `render_mermaid` would convert any later raster panic to `RenderError::Panic`. Net behaviour identical.

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib components::mermaid::tests::render_svg_only 2>&1 | tail -20`
Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 5: Run the full lib test suite to verify no existing test broke**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib 2>&1 | tail -10`
Expected: all 515 (now 518) tests pass — the 3 new `render_svg_only_*` tests + 515 prior. The exact prior count is locked at 515 by the v2 mermaid pipeline; if the count differs, investigate before continuing.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs
git commit -m "feat(spur-tui): add render_svg_only helper for SVG export

Refactors the existing private render_to_svg_inner into pub(crate)
render_svg_only that returns Result<String, String>. The raster path
(render_mermaid) keeps its existing RenderError typed return via a
thin adaptor. The SVG export feature can now produce SVG without
paying for resvg/tiny-skia rasterisation."
```

---

## Task 3: `session_short` + `export_svg_path` helpers

**Files:**
- Modify: `crates/spur-tui/src/components/mermaid.rs` (add helpers after `raster_width_for_pane`)
- Test: same file's `mod tests`

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `crates/spur-tui/src/components/mermaid.rs`:

```rust
#[test]
fn session_short_is_8_lower_hex_chars() {
    let s = session_short(&spur_acp::SessionId("anything".into()));
    assert_eq!(s.len(), 8, "expected 8 chars, got {:?}", s);
    assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "expected lowercase hex, got {:?}", s);
}

#[test]
fn session_short_is_deterministic() {
    let a = session_short(&spur_acp::SessionId("session-1".into()));
    let b = session_short(&spur_acp::SessionId("session-1".into()));
    assert_eq!(a, b);
}

#[test]
fn session_short_distinguishes_different_sessions() {
    let a = session_short(&spur_acp::SessionId("session-1".into()));
    let b = session_short(&spur_acp::SessionId("session-2".into()));
    assert_ne!(a, b, "different sessions must produce different shorts");
}

#[test]
fn export_svg_path_format_matches_spec() {
    let p = export_svg_path(
        &spur_acp::SessionId("session-1".into()),
        MermaidId(7),
        42,
    );
    let name = p.file_name().unwrap().to_string_lossy().into_owned();
    let prefix = "spur-mermaid-";
    assert!(name.starts_with(prefix), "name = {:?}", name);
    let rest = &name[prefix.len()..];
    let parts: Vec<&str> = rest.trim_end_matches(".svg").split('-').collect();
    assert_eq!(parts.len(), 3, "expected 3 dash-separated parts, got {:?}", parts);
    assert_eq!(parts[0].len(), 8, "session_short should be 8 chars");
    assert_eq!(parts[1], "7", "mermaid id");
    assert_eq!(parts[2], "42", "generation");
    assert!(name.ends_with(".svg"));
}

#[test]
fn export_svg_path_changes_on_generation_bump() {
    let s = spur_acp::SessionId("session-1".into());
    let p1 = export_svg_path(&s, MermaidId(7), 1);
    let p2 = export_svg_path(&s, MermaidId(7), 2);
    assert_ne!(p1, p2);
}

#[test]
fn export_svg_path_in_temp_dir() {
    let p = export_svg_path(
        &spur_acp::SessionId("session-1".into()),
        MermaidId(0),
        0,
    );
    assert!(p.starts_with(std::env::temp_dir()),
        "path {:?} should be inside {:?}", p, std::env::temp_dir());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib components::mermaid::tests::session_short components::mermaid::tests::export_svg_path 2>&1 | tail -30`
Expected: 6 compile errors — `session_short` and `export_svg_path` undefined.

- [ ] **Step 3: Implement the helpers**

In `crates/spur-tui/src/components/mermaid.rs`, locate the `raster_width_for_pane` function (around line 169). Immediately AFTER its closing `}` add:

```rust
/// Compress a session id into 8 lowercase hex chars suitable for use as a
/// filesystem prefix. Stable across runs (uses std `DefaultHasher`).
///
/// Note: `DefaultHasher` is not cryptographic — collisions are theoretically
/// possible. For temp filenames keyed by (session, id, generation), the
/// collision risk is acceptable: a single user's concurrent sessions would
/// have to hash to the same 32-bit prefix, and even then the (id, generation)
/// suffix disambiguates.
pub(crate) fn session_short(session: &spur_acp::SessionId) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session.0.hash(&mut h);
    format!("{:08x}", (h.finish() & 0xFFFF_FFFF) as u32)
}

/// Idempotent temp-file path for the SVG export of a given diagram. The
/// generation suffix means a bucket-up re-raster (which bumps
/// `image_generation`) automatically lands at a fresh path — stale files
/// simply persist in the OS tmpdir until OS-level cleanup.
pub(crate) fn export_svg_path(
    session: &spur_acp::SessionId,
    id: MermaidId,
    generation: u64,
) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "spur-mermaid-{}-{}-{}.svg",
        session_short(session),
        id.0,
        generation,
    ));
    p
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib components::mermaid::tests::session_short components::mermaid::tests::export_svg_path 2>&1 | tail -10`
Expected: `test result: ok. 6 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs
git commit -m "feat(spur-tui): session_short + export_svg_path helpers for SVG export

Stable 8-hex-char hash of the session id (std DefaultHasher) plus a
deterministic path builder keyed on (session, id, generation). The
generation suffix auto-invalidates cached files when the v2 mermaid
pipeline re-rasters at a higher bucket."
```

---

## Task 4: `atomic_write` helper

**Files:**
- Modify: `crates/spur-tui/src/components/mermaid.rs`
- Test: same file's `mod tests`

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block:

```rust
#[test]
fn atomic_write_creates_file_with_content() {
    let dir = std::env::temp_dir().join(format!("spur-atomic-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test1.svg");
    atomic_write(&path, b"hello world").expect("write succeeds");
    let read = std::fs::read(&path).expect("read back");
    assert_eq!(read, b"hello world");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn atomic_write_replaces_existing_file() {
    let dir = std::env::temp_dir().join(format!("spur-atomic-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test2.svg");
    std::fs::write(&path, b"old content").expect("write old");
    atomic_write(&path, b"new content").expect("write new");
    let read = std::fs::read(&path).expect("read back");
    assert_eq!(read, b"new content");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn atomic_write_leaves_no_tmp_sibling() {
    let dir = std::env::temp_dir().join(format!("spur-atomic-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test3.svg");
    let tmp_path = dir.join("test3.svg.tmp");
    atomic_write(&path, b"content").expect("write succeeds");
    assert!(path.exists(), "final file must exist");
    assert!(!tmp_path.exists(), ".tmp sibling must not be left behind");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib components::mermaid::tests::atomic_write 2>&1 | tail -20`
Expected: 3 compile errors — `atomic_write` undefined.

- [ ] **Step 3: Implement `atomic_write`**

In `crates/spur-tui/src/components/mermaid.rs`, immediately AFTER the `export_svg_path` function added in Task 3, append:

```rust
/// Atomically write `content` to `path` by writing to a sibling `.tmp` file
/// and renaming it. `rename` is atomic on POSIX and on NTFS for files in the
/// same directory.
pub(crate) fn atomic_write(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp_path: std::path::PathBuf = {
        let mut p = path.as_os_str().to_owned();
        p.push(".tmp");
        std::path::PathBuf::from(p)
    };
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib components::mermaid::tests::atomic_write 2>&1 | tail -10`
Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs
git commit -m "feat(spur-tui): atomic_write helper for SVG export

Write-then-rename pattern with fsync. Protects the SVG export against
mashing 'o' rapidly — the launcher subprocess never sees a partially-
written file."
```

---

## Task 5: New action variants

**Files:**
- Modify: `crates/spur-tui/src/action.rs:138` (insert after `MermaidRenderCompleted`)

- [ ] **Step 1: Add the variants**

In `crates/spur-tui/src/action.rs`, find the `MermaidRenderCompleted` variant (around lines 132-138). Insert these two new variants IMMEDIATELY AFTER the closing `},` of `MermaidRenderCompleted` and BEFORE the `CancelStream` variant:

```rust
    /// Request export of the currently-focused mermaid diagram in the
    /// overlay viewer to an SVG temp file. Triggered by the `o` key.
    /// The app handler looks up the diagram's code + generation via
    /// the active SessionDetailView.
    #[cfg(feature = "markdown")]
    ExportFocusedMermaidSvg {
        session: SessionId,
        id: crate::components::mermaid::MermaidId,
    },
    /// Completion of a previously-dispatched SVG export. `Ok(path)` means
    /// the file is on disk and (separately) the launcher subprocess was
    /// spawned. `Err(msg)` covers SVG render failure, atomic-write failure,
    /// or launcher spawn failure.
    #[cfg(feature = "markdown")]
    MermaidSvgExported {
        session: SessionId,
        id: crate::components::mermaid::MermaidId,
        result: Result<std::path::PathBuf, String>,
    },
```

- [ ] **Step 2: Run a build to verify the additions compile**

Run: `scripts/spur-cargo build -p spur-tui --features markdown 2>&1 | tail -10`
Expected: `Finished` line appears. The new variants will trigger `unused variant` warnings — that's fine, they'll be matched in Tasks 6 and 7.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/action.rs
git commit -m "feat(spur-tui): action variants for mermaid SVG export

ExportFocusedMermaidSvg + MermaidSvgExported. Pair mirrors the existing
MermaidRenderRequest/Completed pattern: the request carries enough to
look up state on the app side, the completion carries the resolved
PathBuf or an error message."
```

---

## Task 6: `ExportStatus` + `o` keybind on MermaidViewerView

**Files:**
- Modify: `crates/spur-tui/src/views/mermaid_viewer.rs`

- [ ] **Step 1: Write the failing tests**

At the BOTTOM of `crates/spur-tui/src/views/mermaid_viewer.rs` (after the existing `impl View for MermaidViewerView` block), append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn make_view() -> MermaidViewerView {
        MermaidViewerView::new(spur_acp::SessionId("test-session".into()))
    }

    fn fake_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ready_state() -> MermaidState {
        use image::{DynamicImage, RgbaImage};
        MermaidState::Ready {
            image: std::sync::Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(8, 8))),
            code: "graph TD\nA-->B".into(),
            rastered_at_bucket: 800,
            image_generation: 1,
        }
    }

    #[test]
    fn o_with_focus_emits_export_action() {
        let mut v = make_view();
        let s = ready_state();
        v.set_available(&[(MermaidId(3), &s)]);
        let ctx = super::super::ViewContext::default();
        let action = v.handle_key(fake_key('o'), &ctx);
        match action {
            Some(crate::action::Action::ExportFocusedMermaidSvg { session, id }) => {
                assert_eq!(session.0, "test-session");
                assert_eq!(id.0, 3);
            }
            other => panic!("expected ExportFocusedMermaidSvg, got {other:?}"),
        }
        assert!(matches!(v.export_status, Some(ExportStatus::Pending)));
    }

    #[test]
    fn o_without_focus_is_noop() {
        let mut v = make_view();
        let ctx = super::super::ViewContext::default();
        let action = v.handle_key(fake_key('o'), &ctx);
        assert!(action.is_none(), "expected None, got {action:?}");
        assert!(v.export_status.is_none());
    }

    #[test]
    fn esc_clears_export_status() {
        let mut v = make_view();
        v.export_status = Some(ExportStatus::Pending);
        let ctx = super::super::ViewContext::default();
        let _ = v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &ctx);
        assert!(v.export_status.is_none());
    }

    #[test]
    fn on_export_completed_ok_updates_status_to_opened() {
        let mut v = make_view();
        v.set_available(&[(MermaidId(3), &ready_state())]);
        v.on_export_completed(MermaidId(3), Ok(std::path::PathBuf::from("/tmp/spur-mermaid-x.svg")));
        match &v.export_status {
            Some(ExportStatus::Opened { filename }) => {
                assert_eq!(filename, "spur-mermaid-x.svg");
            }
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn on_export_completed_err_updates_status_to_failed() {
        let mut v = make_view();
        v.set_available(&[(MermaidId(3), &ready_state())]);
        v.on_export_completed(MermaidId(3), Err("disk full".into()));
        match &v.export_status {
            Some(ExportStatus::Failed { detail }) => assert!(detail.contains("disk full")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn on_export_completed_for_unfocused_id_is_dropped() {
        let mut v = make_view();
        v.set_available(&[(MermaidId(3), &ready_state())]);
        v.on_export_completed(MermaidId(99), Ok(std::path::PathBuf::from("/tmp/x.svg")));
        // Focused diagram is 3; completion for 99 is stale and must not overwrite status.
        assert!(v.export_status.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib views::mermaid_viewer 2>&1 | tail -20`
Expected: compile errors on `ExportStatus`, `export_status`, `on_export_completed` — none defined yet.

- [ ] **Step 3: Add `ExportStatus` and view fields**

In `crates/spur-tui/src/views/mermaid_viewer.rs`, REPLACE the entire file content with:

```rust
#![cfg(feature = "markdown")]

//! Full-screen overlay state for mermaid viewing. Owns only the cursor
//! (which diagram is focused) and the SVG-export status string — the
//! actual `StatefulProtocol` lives in `SessionDetailView::image_cache`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{layout::Rect, Frame};
use spur_acp::{SessionId, SpurEvent};

use crate::action::Action;
use crate::components::mermaid::{MermaidId, MermaidState};

use super::View;

/// Status of the most recent SVG export operation. Surfaced in the
/// overlay footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportStatus {
    /// Render dispatched on a blocking pool; awaiting completion.
    Pending,
    /// Success — launcher subprocess spawned. `filename` is the basename
    /// of the temp file (full path would be unwieldy in the footer).
    Opened { filename: String },
    /// Failure path. `detail` carries the upstream error message
    /// (render error, IO error, or launcher spawn error).
    Failed { detail: String },
}

pub struct MermaidViewerView {
    session_id: SessionId,
    /// Which diagram is currently focused. `None` until `set_available`
    /// chooses a default (the most recent Ready entry).
    pub(crate) focused: Option<MermaidId>,
    /// Latest SVG-export operation status, surfaced in the overlay footer.
    pub(crate) export_status: Option<ExportStatus>,
}

impl MermaidViewerView {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            focused: None,
            export_status: None,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Choose the default focus from the registry. Called by the app
    /// layer before `render_mermaid_overlay`.
    pub fn set_available(&mut self, entries: &[(MermaidId, &MermaidState)]) {
        if self.focused.is_none() {
            self.focused = entries
                .iter()
                .rev()
                .find(|(_, s)| matches!(s, MermaidState::Ready { .. }))
                .map(|(id, _)| *id);
        }
    }

    /// Cycle focus among Ready entries.
    pub fn cycle(&mut self, entries: &[(MermaidId, &MermaidState)], forward: bool) {
        let ready_ids: Vec<MermaidId> = entries
            .iter()
            .filter(|(_, s)| matches!(s, MermaidState::Ready { .. }))
            .map(|(id, _)| *id)
            .collect();
        if ready_ids.is_empty() {
            self.focused = None;
            return;
        }
        let idx = self
            .focused
            .and_then(|cur| ready_ids.iter().position(|i| *i == cur))
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % ready_ids.len()
        } else {
            (idx + ready_ids.len() - 1) % ready_ids.len()
        };
        self.focused = Some(ready_ids[next]);
        // Cycling clears the export status — the previous diagram's status is no
        // longer relevant.
        self.export_status = None;
    }

    /// Called by the app on `Action::MermaidSvgExported`. Drops stale
    /// completions whose `id` doesn't match the currently-focused diagram.
    pub(crate) fn on_export_completed(
        &mut self,
        id: MermaidId,
        result: Result<std::path::PathBuf, String>,
    ) {
        if self.focused != Some(id) {
            return;
        }
        self.export_status = Some(match result {
            Ok(path) => ExportStatus::Opened {
                filename: path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            },
            Err(detail) => ExportStatus::Failed { detail },
        });
    }
}

impl View for MermaidViewerView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.export_status = None;
                Some(Action::NavigateBack)
            }
            KeyCode::Char('o') if !key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                && !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) =>
            {
                let id = self.focused?;
                self.export_status = Some(ExportStatus::Pending);
                Some(Action::ExportFocusedMermaidSvg {
                    session: self.session_id.clone(),
                    id,
                })
            }
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &super::ViewContext) {}

    fn render(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &super::ViewContext) {}

    fn tick(&mut self) {}
}
```

Key points:
- `ExportStatus` is `pub(crate)` so the renderer in `app.rs` can match on it.
- `on_export_completed` is `pub(crate)` so `app.rs` can call it from the action handler.
- The `Esc`/`q` arm clears `export_status` (for the test `esc_clears_export_status`).
- The `o` arm guards against Ctrl+O / Alt+O — Ctrl+O is taken in SessionDetailView (`toggle_observe_collapsed`); Alt+O is unused but still excluded for clarity.
- `cycle()` clears `export_status` because the previous diagram's "✓ opened" line shouldn't bleed into the next focus.

- [ ] **Step 4: Run tests to verify they pass**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib views::mermaid_viewer 2>&1 | tail -10`
Expected: `test result: ok. 6 passed; 0 failed`.

- [ ] **Step 5: Run the full lib suite**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib 2>&1 | tail -10`
Expected: all tests pass (518 from Task 2 + 6 + 6 + 3 from Tasks 3/4/6 — adjust if other test counts in the codebase shifted; numbers should be increasing monotonically only).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/mermaid_viewer.rs
git commit -m "feat(spur-tui): 'o' keybind + ExportStatus on MermaidViewerView

Adds the user-facing keybind plus the in-flight/done/failed status enum
that the overlay footer renders. Stale completions for a non-focused
diagram are dropped (cycle protection). Esc and cycle both clear the
status — the next focus starts clean."
```

---

## Task 7: App-side handler

**Files:**
- Modify: `crates/spur-tui/src/app.rs:2117` (after the existing `MermaidRenderCompleted` arm)

- [ ] **Step 1: Add the action handlers**

In `crates/spur-tui/src/app.rs`, locate the `Action::MermaidRenderCompleted` arm (currently around lines 2117-2130). Immediately AFTER its closing `}`, BEFORE the `// Scroll actions are already handled` comment (around line 2132), insert:

```rust
            #[cfg(feature = "markdown")]
            Action::ExportFocusedMermaidSvg { session, id } => {
                // Look up code + generation in the active SessionDetailView's
                // mermaid_registry. If the session/diagram doesn't match,
                // nothing happens — defensive against late-firing actions.
                let lookup = self
                    .session_detail
                    .as_ref()
                    .filter(|d| d.session_id().0 == session.0)
                    .and_then(|d| d.mermaid_registry.get(&id))
                    .and_then(|st| match st {
                        crate::components::mermaid::MermaidState::Ready {
                            code,
                            image_generation,
                            ..
                        } => Some((code.clone(), *image_generation)),
                        _ => None,
                    });
                let Some((code, generation)) = lookup else {
                    self.dirty = true;
                    continue;
                };

                let path = crate::components::mermaid::export_svg_path(&session, id, generation);
                let tx = self.mermaid_tx.clone();
                let session_for_completion = session.clone();

                // Stat-cache: if a non-empty SVG already exists at this
                // (session, id, generation) path, skip the render and go
                // straight to launch.
                let cached = std::fs::metadata(&path)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false);

                if cached {
                    let result = match opener::open(&path) {
                        Ok(()) => Ok(path),
                        Err(e) => Err(format!("opener failed: {e}")),
                    };
                    let _ = tx.send(Action::MermaidSvgExported {
                        session: session_for_completion,
                        id,
                        result,
                    });
                } else {
                    let path_for_task = path.clone();
                    tokio::task::spawn_blocking(move || {
                        let render_result =
                            crate::components::mermaid::render_svg_only(&code);
                        let final_result = match render_result {
                            Ok(svg) => {
                                match crate::components::mermaid::atomic_write(
                                    &path_for_task,
                                    svg.as_bytes(),
                                ) {
                                    Ok(()) => match opener::open(&path_for_task) {
                                        Ok(()) => Ok(path_for_task),
                                        Err(e) => Err(format!("opener failed: {e}")),
                                    },
                                    Err(e) => Err(format!("write failed: {e}")),
                                }
                            }
                            Err(e) => Err(format!("render failed: {e}")),
                        };
                        let _ = tx.send(Action::MermaidSvgExported {
                            session: session_for_completion,
                            id,
                            result: final_result,
                        });
                    });
                }
                self.dirty = true;
            }
            #[cfg(feature = "markdown")]
            Action::MermaidSvgExported { session, id, result } => {
                if let Some(ref mut viewer) = self.mermaid_viewer {
                    if viewer.session_id().0 == session.0 {
                        viewer.on_export_completed(id, result);
                    }
                }
                self.dirty = true;
            }
```

- [ ] **Step 2: Verify build**

Run: `scripts/spur-cargo build -p spur-tui --features markdown 2>&1 | tail -10`
Expected: `Finished` line. Warnings about unused variants from Task 5 should disappear.

- [ ] **Step 3: Run lib tests**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib 2>&1 | tail -10`
Expected: all tests pass; nothing should have regressed.

- [ ] **Step 4: Verify clippy**

Run: `scripts/spur-cargo clippy -p spur-tui --features markdown -- -D warnings 2>&1 | tail -10`
Expected: clean — no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): app-side handlers for mermaid SVG export

ExportFocusedMermaidSvg looks up code+generation, computes the
deterministic path, and either short-circuits to opener::open (file
already cached) or kicks off a spawn_blocking render → atomic_write →
opener::open chain. Either way the result lands as a
MermaidSvgExported action that the viewer consumes via
on_export_completed."
```

---

## Task 8: Overlay footer rendering

**Files:**
- Modify: `crates/spur-tui/src/app.rs:2906-2913` (the existing footer in `render_mermaid_overlay`)

- [ ] **Step 1: Replace the footer**

In `crates/spur-tui/src/app.rs`, locate the trailing footer-paragraph section in `render_mermaid_overlay`:

```rust
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " [/]: cycle · q/Esc: close ",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );
```

REPLACE the entire `frame.render_widget(...)` call with:

```rust
    let base_hint: Span<'_> = Span::styled(
        " [/]: cycle · o: open SVG · q/Esc: close ",
        Style::default().fg(Color::DarkGray),
    );
    let status_span: Option<Span<'_>> = match &viewer.export_status {
        Some(crate::views::mermaid_viewer::ExportStatus::Pending) => Some(Span::styled(
            "  ↻ rendering SVG…",
            Style::default().fg(Color::Yellow),
        )),
        Some(crate::views::mermaid_viewer::ExportStatus::Opened { filename }) => Some(Span::styled(
            format!("  ✓ opened {filename}"),
            Style::default().fg(Color::Green),
        )),
        Some(crate::views::mermaid_viewer::ExportStatus::Failed { detail }) => Some(Span::styled(
            format!("  ✗ open failed — {detail}"),
            Style::default().fg(Color::Red),
        )),
        None => None,
    };
    let footer_line = match status_span {
        Some(s) => Line::from(vec![base_hint, s]),
        None => Line::from(base_hint),
    };
    frame.render_widget(Paragraph::new(footer_line), chunks[2]);
```

The base hint now mentions `o` regardless of state, so the keybind is discoverable from the moment the overlay opens (without needing the help screen).

- [ ] **Step 2: Verify build + lint**

Run: `scripts/spur-cargo build -p spur-tui --features markdown 2>&1 | tail -10`
Expected: `Finished`.

Run: `scripts/spur-cargo clippy -p spur-tui --features markdown -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): mermaid overlay footer now shows export status + 'o' hint

Footer now reads ' [/]: cycle · o: open SVG · q/Esc: close ' with an
appended status segment when an export is in flight or completed. The
'o' hint is always visible — no need to read the help overlay to
discover the keybind."
```

---

## Task 9: Help overlay registration

**Files:**
- Modify: `crates/spur-tui/src/components/help_overlay.rs:148-153`

- [ ] **Step 1: Add the keybind row**

In `crates/spur-tui/src/components/help_overlay.rs`, find the Mermaid Viewer section (currently around lines 148-153):

```rust
        if mermaid_enabled {
            out.push(header(" Mermaid Viewer (overlay)"));
            out.push(Line::from("  [ / ]              Cycle diagrams"));
            out.push(Line::from("  q / Esc            Close overlay"));
            out.push(Line::from(""));
        }
```

REPLACE it with:

```rust
        if mermaid_enabled {
            out.push(header(" Mermaid Viewer (overlay)"));
            out.push(Line::from("  [ / ]              Cycle diagrams"));
            out.push(Line::from(
                "  o                  Open vector SVG in default app",
            ));
            out.push(Line::from("  q / Esc            Close overlay"));
            out.push(Line::from(""));
        }
```

- [ ] **Step 2: Find existing help-content tests and verify they still pass**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --lib help_overlay 2>&1 | tail -10`
Expected: all existing help_overlay tests still pass — adding a line should not regress unless a test asserts on exact line count.

If a test asserts on the specific line count of the Mermaid Viewer block, adjust the assertion to match the new count (existing 4 visible lines → 5 visible lines, including the new `o` row).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/help_overlay.rs
git commit -m "docs(spur-tui): document 'o' keybind in help overlay

The Mermaid Viewer (overlay) section now lists 'o' for open-vector-SVG
alongside cycle and close. Discoverable both from the footer hint and
the help screen."
```

---

## Task 10: Integration test

**Files:**
- Create: `crates/spur-tui/tests/mermaid_svg_export.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/spur-tui/tests/mermaid_svg_export.rs` with:

```rust
//! Integration: verify the SVG-export pipeline writes a deterministic
//! file to the OS tmpdir and that the file content is a valid SVG.
//!
//! Does NOT exercise the launcher (`opener::open`) — that would launch
//! a real external app on the test runner. Instead we drive the
//! mermaid module's helpers directly: `render_svg_only` →
//! `atomic_write` at the deterministic path, then assert the file
//! exists and contains an SVG header.

#![cfg(feature = "markdown")]

use spur_acp::SessionId;
use spur_tui::components::mermaid::{
    atomic_write, export_svg_path, render_svg_only, MermaidId,
};

#[test]
fn round_trip_writes_svg_to_deterministic_path() {
    let session = SessionId("integration-test-session".into());
    let id = MermaidId(42);
    let generation = 7;

    let path = export_svg_path(&session, id, generation);
    let _ = std::fs::remove_file(&path); // ensure clean state

    let svg = render_svg_only("flowchart TD\nA-->B-->C")
        .expect("simple flowchart should render to SVG");
    assert!(svg.starts_with("<svg") || svg.starts_with("<?xml"),
        "render_svg_only should return SVG content, got prefix {:?}",
        svg.chars().take(20).collect::<String>());

    atomic_write(&path, svg.as_bytes()).expect("atomic_write succeeds");

    assert!(path.exists(), "exported file should exist at {:?}", path);
    let on_disk = std::fs::read_to_string(&path).expect("readback");
    assert_eq!(on_disk, svg, "file content should match rendered SVG");
    assert!(on_disk.contains("</svg>"), "should be a complete SVG document");

    let _ = std::fs::remove_file(&path); // cleanup
}

#[test]
fn round_trip_path_changes_on_generation_bump() {
    let session = SessionId("integration-test-session-2".into());
    let id = MermaidId(1);
    let p1 = export_svg_path(&session, id, 1);
    let p2 = export_svg_path(&session, id, 2);
    assert_ne!(p1, p2, "different generations must yield different paths");
    let _ = std::fs::remove_file(&p1);
    let _ = std::fs::remove_file(&p2);
}
```

This requires `MermaidId`, `render_svg_only`, `atomic_write`, and `export_svg_path` to be accessible via the public path `spur_tui::components::mermaid::*`. Verify the items needed are exposed:

- `MermaidId` — already `pub`.
- `render_svg_only`, `export_svg_path`, `atomic_write` — defined as `pub(crate)` in Tasks 2-4.

Integration tests live OUTSIDE the crate, so they can only see `pub` items. The simplest fix: change the three helpers from `pub(crate)` to `pub`. This is a deliberate trade-off — the helpers are testable, the API surface widens by three small functions, and the alternative (a `#[cfg(test)] pub` shim) is more code for the same effect.

Apply this change in `crates/spur-tui/src/components/mermaid.rs`:

```text
- pub(crate) fn render_svg_only(code: &str) -> Result<String, String> {
+ pub fn render_svg_only(code: &str) -> Result<String, String> {
```
```text
- pub(crate) fn session_short(session: &spur_acp::SessionId) -> String {
+ pub fn session_short(session: &spur_acp::SessionId) -> String {
```
```text
- pub(crate) fn export_svg_path(
+ pub fn export_svg_path(
```
```text
- pub(crate) fn atomic_write(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
+ pub fn atomic_write(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
```

Leave `ExportStatus` and `on_export_completed` as `pub(crate)` — those are only needed inside the crate.

- [ ] **Step 2: Run the integration test**

Run: `scripts/spur-cargo test -p spur-tui --features markdown --test mermaid_svg_export 2>&1 | tail -10`
Expected: `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 3: Run the full test suite**

Run: `scripts/spur-cargo test -p spur-tui --features markdown 2>&1 | tail -10`
Expected: all unit + integration tests pass.

Run: `scripts/spur-cargo clippy -p spur-tui --features markdown -- -D warnings 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs crates/spur-tui/tests/mermaid_svg_export.rs
git commit -m "test(spur-tui): integration test for mermaid SVG export round-trip

Drives render_svg_only → atomic_write → readback to assert the file
lands at the deterministic export_svg_path. Does NOT touch
opener::open (no external-app launch in CI).

Widens the visibility of the four mermaid helpers from pub(crate) to
pub so the integration test can reach them."
```

---

## Task 11: Manual verification + sweep

**Files:** none modified. This task is a checklist of manual checks the user runs in their iTerm2 environment.

- [ ] **Step 1: Build a release binary**

Run: `scripts/spur-cargo build -p spur-tui --features markdown --release 2>&1 | tail -5`
Expected: `Finished` line; binary at `target/release/spur-tui` (or the configured workspace target).

- [ ] **Step 2: Manual check M1 — happy path on macOS**

1. Launch SPUR; load a session containing at least one mermaid diagram (e.g., the demo session with `flowchart TD\nA-->B-->C`).
2. Wait for the diagram to finish rendering (placeholder turns into inline image).
3. Press `Alt+v` — the Mermaid Viewer overlay opens; the diagram is shown at full size.
4. Press `o`. Expected: footer briefly shows ` ↻ rendering SVG…` then ` ✓ opened spur-mermaid-XXXXXXXX-N-G.svg`. The OS default SVG handler (Preview, Safari, …) opens with a crisp vector view.

- [ ] **Step 3: Manual check M2 — second press is instant**

Press `o` again on the same focused diagram. Expected: status flips immediately to ` ✓ opened spur-mermaid-…` (no perceptible delay; the cached file path is used). Repeated mashing must never produce a blank file.

- [ ] **Step 4: Manual check M3 — error path**

Force an `opener` failure: temporarily rename `/usr/bin/open` (macOS) or run on a Linux VM with no SVG handler. Press `o`. Expected: footer shows ` ✗ open failed — opener failed: <reason>`. The SVG file should still exist at the path embedded in the error message; user can copy it manually.

After verifying, restore `/usr/bin/open` (`sudo mv /usr/bin/open.bak /usr/bin/open`).

- [ ] **Step 5: Manual check M4 — generation bump invalidation**

1. Open the overlay on a diagram and press `o` (creates file at generation N).
2. Resize the terminal so the diagram pane crosses a `RASTER_BUCKETS` threshold (e.g. resize from ~80 cols to ~140 cols).
3. Wait for the v2 pipeline to bucket-up re-raster (the `image_generation` field bumps).
4. Press `o` again. Expected: a NEW file at generation N+1 is rendered (visible in `ls /tmp/spur-mermaid-*`); the freshly-rendered SVG opens.

- [ ] **Step 6: Final lint + test sweep**

Run all parallel:
```bash
scripts/spur-cargo test -p spur-tui --features markdown 2>&1 | tail -5 &
scripts/spur-cargo clippy -p spur-tui --features markdown -- -D warnings 2>&1 | tail -5 &
wait
```
Expected: both clean.

- [ ] **Step 7: No commit** — Task 11 is verification-only. If any of M1-M4 reveal a defect, file it as a follow-up issue and address in a separate cycle.

---

## Self-review

**Spec coverage:**
- Problem & user flow → covered by Tasks 1-9 (full pipeline) + Task 11 (manual M1-M4).
- Architecture diagram (3-stage flow) → Tasks 5/7 implement the action variants and app handler; Task 6 implements the view-side wiring.
- Components 1-8 of the spec → Task 1 (#5: opener), Task 2 (#2: render_svg_only), Task 3 (#3: export_svg_path), Task 4 (#4: atomic_write), Task 5 (#1: action variants), Task 6 (#7: view + ExportStatus), Task 7 (#6: app handler), Task 8 (footer rendering — sub-component of #7), Task 9 (#8: help registration).
- Error-handling matrix → covered by Task 6 tests (Failed status), Task 7 implementation (catches render/write/opener errors uniformly), Task 11 M3 (manual failure path).
- 12 unit + 1 integration test → Task 2 (3) + Task 3 (6) + Task 4 (3) + Task 6 (6) = 18 unit; Task 10 (2) integration. Spec's 12 was an estimate; we exceeded it.

**Placeholder scan:** No "TBD" / "TODO" / "implement later" / "similar to Task N" in any task body. All steps include exact code or exact commands.

**Type consistency:**
- `MermaidId(u64)` used consistently.
- `ExportStatus::{Pending,Opened{filename},Failed{detail}}` consistently in Tasks 6, 8.
- `Action::ExportFocusedMermaidSvg{session, id}` and `Action::MermaidSvgExported{session, id, result}` — same field names in Tasks 5, 6, 7.
- `render_svg_only(code: &str) -> Result<String, String>` — same signature in Tasks 2, 7, 10.
- `export_svg_path(&SessionId, MermaidId, u64) -> PathBuf` — same in Tasks 3, 7, 10.
- `atomic_write(&Path, &[u8]) -> io::Result<()>` — same in Tasks 4, 7, 10.

No drift detected.

**Scope:** Single feature, one feature flag (`markdown`), one platform-launcher dep, no cross-cutting refactor. Single plan is correct.
