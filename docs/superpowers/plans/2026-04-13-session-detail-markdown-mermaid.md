# Session Detail Markdown + Mermaid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render streaming markdown in the session-detail ReAct trace for assistant messages, and render Mermaid diagrams from ```` ```mermaid ```` fences in a full-screen overlay viewer.

**Architecture:** Incoming `AgentMessageChunk`s append to a per-entry `MarkdownStream` buffer, debounced at 50 ms, then re-parsed end-to-end via `tui-markdown` to yield ratatui `Line`s for the existing `Paragraph`-based trace renderer. Closed ```` ```mermaid ```` fences are substituted with a single-line placeholder in the trace and dispatched to a `tokio::task::spawn_blocking` worker that calls `mermaid-rs-renderer` (`mmdr`) → PNG → `image::DynamicImage`. A new `MermaidViewerView` overlay (a `ViewId::MermaidOverlay` navigation target) renders the cached image via `ratatui-image`.

**Tech Stack:** Rust 1.85+, ratatui 0.29, tokio, `tui-markdown` (^0.3), `mermaid-rs-renderer` (^0.2 default-features=false, features=["png"]), `image` (^0.25), `ratatui-image` (^10, features=["tokio","chafa-dyn"]).

**Spec:** `docs/superpowers/specs/2026-04-13-session-detail-markdown-mermaid-design.md`.

---

## File Structure

New files:
- `crates/spur-tui/src/components/mermaid.rs` — render_mermaid function + types (MermaidId, MermaidState).
- `crates/spur-tui/src/components/markdown_stream.rs` — MarkdownStream: debounced re-parse + fence detection + sentinel post-process.
- `crates/spur-tui/src/views/mermaid_viewer.rs` — overlay view rendering a `StatefulImage`.
- `crates/spur-tui/tests/markdown_stream_tests.rs` — integration tests for the stream.
- `crates/spur-tui/tests/mermaid_render_tests.rs` — integration tests for the renderer.

Modified files:
- `Cargo.toml` (workspace) — `rust-version = "1.85"`.
- `crates/spur-tui/Cargo.toml` — new deps + `markdown` feature flag.
- `crates/spur-tui/src/components/mod.rs` — expose new modules.
- `crates/spur-tui/src/components/react_trace.rs` — TraceEntry AgentMessage gains optional `MarkdownStream`; `append_message` + `render` + `tick` updated.
- `crates/spur-tui/src/components/help_overlay.rs` — document new bindings.
- `crates/spur-tui/src/views/mod.rs` — expose mermaid_viewer.
- `crates/spur-tui/src/views/session_detail.rs` — `Alt-v` binding + mermaid registry + handle `MermaidRenderCompleted`.
- `crates/spur-tui/src/action.rs` — new ViewId variant + new Action variants.
- `crates/spur-tui/src/app.rs` — dispatch MermaidRenderRequest via spawn_blocking; wire MermaidOverlay view.

Gating: everything added by this plan compiles out cleanly under `cargo build -p spur-tui --no-default-features`. All mermaid/markdown code lives behind `#[cfg(feature = "markdown")]` or parallel non-feature fallback paths.

---

### Task 1: Bump MSRV and add dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/spur-tui/Cargo.toml`

- [ ] **Step 1: Bump workspace MSRV.**

Edit `Cargo.toml` at the repository root. Change the `[workspace.package]` `rust-version`:

```toml
[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
license = "MIT"
```

- [ ] **Step 2: Add dependencies and feature flag to spur-tui.**

Replace the `[dependencies]` block of `crates/spur-tui/Cargo.toml`:

```toml
[features]
default = ["markdown"]
markdown = ["dep:tui-markdown", "dep:mermaid-rs-renderer", "dep:image", "dep:ratatui-image"]

[dependencies]
spur-acp = { workspace = true }
spur-core = { workspace = true }
tokio = { workspace = true }
ratatui = { workspace = true }
crossterm = { workspace = true, features = ["event-stream"] }
anyhow = { workspace = true }
chrono = { workspace = true }
futures = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
unicode-width = "0.1"

# Markdown + mermaid stack (gated by `markdown` feature)
tui-markdown = { version = "0.3", optional = true }
mermaid-rs-renderer = { version = "0.2", default-features = false, features = ["png"], optional = true }
image = { version = "0.25", default-features = false, features = ["png"], optional = true }
ratatui-image = { version = "10", default-features = false, features = ["tokio", "chafa-dyn"], optional = true }

[dev-dependencies]
agent-client-protocol = { workspace = true }
```

- [ ] **Step 3: Verify baseline build works.**

Run: `cargo build -p spur-tui`
Expected: success. Build may warn about unused deps until later tasks consume them; that's fine.

Run: `cargo build -p spur-tui --no-default-features`
Expected: success — this validates the feature gate compiles without the new deps.

- [ ] **Step 4: Commit.**

```bash
git add Cargo.toml crates/spur-tui/Cargo.toml
git commit -m "feat(spur-tui): add markdown+mermaid deps behind feature flag, bump MSRV to 1.85"
```

---

### Task 2: Mermaid rendering core

**Files:**
- Create: `crates/spur-tui/src/components/mermaid.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Create: `crates/spur-tui/tests/mermaid_render_tests.rs`

- [ ] **Step 1: Write failing integration tests.**

Create `crates/spur-tui/tests/mermaid_render_tests.rs`:

```rust
#![cfg(feature = "markdown")]

use spur_tui::components::mermaid::{render_mermaid, RenderError};

#[test]
fn renders_valid_flowchart_to_nonzero_image() {
    let code = "flowchart LR\n    A[Start] --> B[End]\n";
    let img = render_mermaid(code).expect("valid flowchart should render");
    assert!(img.width() > 0, "rendered image has zero width");
    assert!(img.height() > 0, "rendered image has zero height");
}

#[test]
fn returns_err_on_malformed_source() {
    let code = "completely not mermaid";
    let result = render_mermaid(code);
    assert!(
        matches!(result, Err(RenderError::Render(_)) | Err(RenderError::Panic(_))),
        "expected a render-side error, got {result:?}"
    );
}

#[test]
fn panics_in_renderer_are_caught() {
    // Input crafted to poke a known-panic edge in mmdr parser is brittle;
    // use an empty string which has historically panicked in some releases.
    // The test asserts we never unwind the caller.
    let result = render_mermaid("");
    assert!(result.is_err(), "empty input should be an Err, not panic");
}
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test -p spur-tui --test mermaid_render_tests`
Expected: compilation failure — `spur_tui::components::mermaid` does not exist.

- [ ] **Step 3: Create the module.**

Create `crates/spur-tui/src/components/mermaid.rs`:

```rust
//! Mermaid diagram rendering.
//!
//! Embeds `mermaid-rs-renderer` (`mmdr`) as a library — modelled after
//! `Epistates/treemd/src/tui/mermaid.rs`: produce PNG bytes via mmdr's `png`
//! feature (which internally uses resvg/usvg), then decode to a
//! `DynamicImage`. All panics in mmdr are caught so a malformed diagram never
//! unwinds the caller.

use std::panic;

use image::DynamicImage;
use mermaid_rs_renderer::{render_with_options, RenderOptions};

/// Monotonically-increasing identifier for a mermaid diagram within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MermaidId(pub u64);

/// State machine for a pending / rendered mermaid diagram.
#[derive(Debug)]
pub enum MermaidState {
    Pending { code: String },
    Rendering,
    Ready { image: DynamicImage },
    Error { message: String },
}

/// Error type for `render_mermaid`.
#[derive(Debug)]
pub enum RenderError {
    /// mmdr returned an `Err` from `render_with_options`.
    Render(String),
    /// mmdr panicked; panic message captured.
    Panic(String),
    /// PNG bytes failed to decode.
    Decode(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Render(m) => write!(f, "mermaid render failed: {m}"),
            Self::Panic(m) => write!(f, "mermaid renderer panicked: {m}"),
            Self::Decode(m) => write!(f, "mermaid PNG decode failed: {m}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Render a mermaid source string to a `DynamicImage`. Synchronous; the
/// caller is expected to run this on a `spawn_blocking` worker when used
/// from an async context.
pub fn render_mermaid(code: &str) -> Result<DynamicImage, RenderError> {
    let code_owned = code.to_string();

    // Catch panics from mmdr — historically some edge inputs (e.g., empty
    // string, malformed state diagrams) have panicked in upstream releases.
    let svg_result = panic::catch_unwind(move || {
        let mut opts = RenderOptions::default();
        opts.output_png = true;
        render_with_options(&code_owned, opts)
    });

    let svg_or_png = match svg_result {
        Ok(r) => r.map_err(|e| RenderError::Render(e.to_string()))?,
        Err(panic_payload) => {
            let msg = panic_payload
                .downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            return Err(RenderError::Panic(msg));
        }
    };

    // When output_png is true, mmdr returns PNG bytes as Vec<u8> encoded in
    // the string; treat the String's bytes as PNG. (mmdr's RenderOptions API
    // returns PNG bytes via its Output variant — adjust below if the real
    // API differs at implementation time.)
    let bytes = svg_or_png.into_bytes();
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| RenderError::Decode(e.to_string()))?;
    Ok(img)
}
```

**Note:** the exact mmdr PNG API call (`render_with_options` return shape) must be confirmed against the installed crate version. If `render_with_options` does not accept `output_png` in `RenderOptions`, substitute the real function (e.g., `mermaid_rs_renderer::write_output_png(...)` writing into a `Vec<u8>`). The test guards against API drift.

- [ ] **Step 4: Register the module.**

Edit `crates/spur-tui/src/components/mod.rs` to add the line (preserve alphabetical order):

```rust
#[cfg(feature = "markdown")]
pub mod mermaid;
```

Place it between `line_wrap` and `react_trace` exports.

- [ ] **Step 5: Run tests to verify they pass.**

Run: `cargo test -p spur-tui --test mermaid_render_tests`
Expected: all three tests pass. If Step 3's API guess was wrong, fix `render_mermaid` to match the real mmdr API; the tests are the contract.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-tui/src/components/mermaid.rs crates/spur-tui/src/components/mod.rs crates/spur-tui/tests/mermaid_render_tests.rs
git commit -m "feat(spur-tui): add mermaid render core with panic-safe mmdr embedding"
```

---

### Task 3: MarkdownStream skeleton

**Files:**
- Create: `crates/spur-tui/src/components/markdown_stream.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Create: `crates/spur-tui/tests/markdown_stream_tests.rs`

- [ ] **Step 1: Write the first failing test (append-then-flush equivalence).**

Create `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#![cfg(feature = "markdown")]

use spur_tui::components::markdown_stream::MarkdownStream;

#[test]
fn append_chunks_then_flush_equals_full_parse() {
    let full = "# Heading\n\nSome **bold** and *italic* text.\n\n- a\n- b\n";
    let mut incremental = MarkdownStream::new();
    for ch in full.chars() {
        incremental.append(&ch.to_string());
    }
    incremental.flush_now();

    let mut one_shot = MarkdownStream::new();
    one_shot.append(full);
    one_shot.flush_now();

    assert_eq!(
        incremental.cached_lines_debug(),
        one_shot.cached_lines_debug(),
        "incremental parse must equal full parse after flush"
    );
}

#[test]
fn debounce_does_not_rebuild_until_flush_or_timeout() {
    let mut s = MarkdownStream::new();
    s.append("# A\n");
    // No flush_now yet → cached_lines may be empty or stale; only assert
    // that an explicit flush produces content.
    s.flush_now();
    assert!(!s.cached_lines_debug().is_empty());
}

#[test]
fn empty_stream_renders_to_empty_lines() {
    let mut s = MarkdownStream::new();
    s.flush_now();
    assert!(s.cached_lines_debug().is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test -p spur-tui --test markdown_stream_tests`
Expected: compilation failure — `MarkdownStream` does not exist.

- [ ] **Step 3: Create the module with a minimal skeleton.**

Create `crates/spur-tui/src/components/markdown_stream.rs`:

```rust
//! Streaming markdown renderer for `AgentMessage` trace entries.
//!
//! Design: `append` is cheap (string push). `maybe_flush` is driven by the
//! UI tick; it rebuilds `cached_lines` when the stream has been dirty for
//! more than `DEBOUNCE_MS` (default 50 ms) or when `flush_now` is called
//! (at `TurnComplete`).
//!
//! Mermaid fences are detected in `rebuild`: each newly-closed
//! ```` ```mermaid ```` fence yields a new `MermaidId`. The fence body is
//! replaced by a sentinel line in the transient string fed to
//! `tui_markdown`; post-processing then swaps the sentinel line for a
//! styled placeholder.

use std::time::{Duration, Instant};

use ratatui::text::Line;

use super::mermaid::MermaidId;

/// How long chunks may accumulate before a flush is triggered.
pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// Internal per-fence record.
#[derive(Debug, Clone)]
pub struct FenceRef {
    pub id: MermaidId,
    pub byte_range: std::ops::Range<usize>,
    pub code: String,
}

/// Accumulated-text markdown renderer.
#[derive(Debug, Default)]
pub struct MarkdownStream {
    raw_text: String,
    dirty_since: Option<Instant>,
    cached_lines: Vec<Line<'static>>,
    known_fences: Vec<FenceRef>,
    next_fence_id: u64,
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a chunk of text. Cheap — does not reparse.
    pub fn append(&mut self, text: &str) {
        self.raw_text.push_str(text);
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Flush if the debounce window has elapsed. Returns any newly-detected
    /// mermaid fences that should be dispatched for rendering.
    pub fn maybe_flush(&mut self) -> Vec<FenceRef> {
        match self.dirty_since {
            Some(t) if t.elapsed() >= DEBOUNCE => self.flush_now(),
            _ => Vec::new(),
        }
    }

    /// Force a flush immediately. Returns any newly-detected mermaid fences.
    pub fn flush_now(&mut self) -> Vec<FenceRef> {
        self.dirty_since = None;
        self.rebuild()
    }

    /// Return the cached rendered lines (valid after a flush).
    pub fn lines(&self) -> &[Line<'static>] {
        &self.cached_lines
    }

    /// Test accessor: returns lines as a Vec<String> for simple equality
    /// checks (Line's Eq is structural including Style which complicates
    /// assertions).
    pub fn cached_lines_debug(&self) -> Vec<String> {
        self.cached_lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    /// Raw text accessor (for diagnostics).
    pub fn raw_text(&self) -> &str {
        &self.raw_text
    }

    /// Rebuild `cached_lines` from `raw_text`. Returns any newly-detected
    /// mermaid fences.
    fn rebuild(&mut self) -> Vec<FenceRef> {
        // Skeleton implementation: parse whole text with tui-markdown,
        // store resulting lines. Fence detection arrives in Task 4.
        let text = tui_markdown::from_str(&self.raw_text);
        self.cached_lines = text
            .lines
            .into_iter()
            .map(|line| Line {
                spans: line.spans.into_iter().map(owned_span).collect(),
                style: line.style,
                alignment: line.alignment,
            })
            .collect();
        Vec::new()
    }
}

fn owned_span(span: ratatui::text::Span<'_>) -> ratatui::text::Span<'static> {
    ratatui::text::Span {
        content: std::borrow::Cow::Owned(span.content.into_owned()),
        style: span.style,
    }
}
```

- [ ] **Step 4: Register the module.**

Add to `crates/spur-tui/src/components/mod.rs`:

```rust
#[cfg(feature = "markdown")]
pub mod markdown_stream;
```

(Place it adjacent to the `mermaid` module line from Task 2.)

- [ ] **Step 5: Run tests to verify they pass.**

Run: `cargo test -p spur-tui --test markdown_stream_tests`
Expected: all three tests pass. If `tui_markdown::from_str` returns a type whose `.lines` field shape differs from the code above, adjust the conversion inline. The `cached_lines_debug()` helper abstracts that away.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/src/components/mod.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): add MarkdownStream skeleton with debounced rebuild"
```

---

### Task 4: MarkdownStream fence detection

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs`
- Modify: `crates/spur-tui/tests/markdown_stream_tests.rs`

- [ ] **Step 1: Add failing tests for fence detection.**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn closed_mermaid_fence_emits_new_fence_ref() {
    let mut s = MarkdownStream::new();
    s.append("# Plan\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nMore text\n");
    let fences = s.flush_now();
    assert_eq!(fences.len(), 1, "expected exactly one fence");
    let f = &fences[0];
    assert!(f.code.contains("flowchart LR"));
    assert!(f.code.contains("A-->B"));
    // Placeholder line replaces the fence in the rendered output.
    let rendered_text: String = s.cached_lines_debug().join("\n");
    assert!(
        rendered_text.contains("mermaid"),
        "expected placeholder mention of mermaid, got: {rendered_text}"
    );
    assert!(
        !rendered_text.contains("A-->B"),
        "mermaid source must not appear in rendered trace"
    );
}

#[test]
fn fence_emission_is_idempotent_across_flushes() {
    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```\n");
    let first = s.flush_now();
    assert_eq!(first.len(), 1);
    let second = s.flush_now();
    assert_eq!(second.len(), 0, "re-flush must not re-emit existing fences");
}

#[test]
fn open_fence_does_not_emit() {
    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n"); // no closing fence
    let fences = s.flush_now();
    assert_eq!(fences.len(), 0, "open fence must not yield a fence ref");
}

#[test]
fn non_mermaid_fences_are_ignored() {
    let mut s = MarkdownStream::new();
    s.append("```rust\nfn main() {}\n```\n");
    let fences = s.flush_now();
    assert_eq!(fences.len(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test -p spur-tui --test markdown_stream_tests`
Expected: 4 new tests fail (existing 3 pass).

- [ ] **Step 3: Implement fence detection.**

Replace the `rebuild` method in `crates/spur-tui/src/components/markdown_stream.rs`:

```rust
fn rebuild(&mut self) -> Vec<FenceRef> {
    // Step 1: scan raw_text for closed ```mermaid fences via pulldown-cmark.
    let mut discovered: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    {
        use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, CodeBlockKind};
        let parser = Parser::new_ext(&self.raw_text, Options::empty()).into_offset_iter();
        let mut open_fence: Option<(usize, String)> = None;
        let mut buf = String::new();
        for (ev, range) in parser {
            match ev {
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                    if info.as_ref().trim().eq_ignore_ascii_case("mermaid") {
                        open_fence = Some((range.start, String::new()));
                        buf.clear();
                    }
                }
                Event::Text(t) if open_fence.is_some() => {
                    buf.push_str(&t);
                }
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((start, _)) = open_fence.take() {
                        discovered.push((start..range.end, std::mem::take(&mut buf)));
                    }
                }
                _ => {}
            }
        }
    }

    // Step 2: match discovered fences against known_fences by byte-range.
    // New ranges get a fresh MermaidId.
    let mut new_fences: Vec<FenceRef> = Vec::new();
    let mut refreshed: Vec<FenceRef> = Vec::with_capacity(discovered.len());
    for (range, code) in discovered {
        let existing = self
            .known_fences
            .iter()
            .find(|f| f.byte_range == range)
            .cloned();
        match existing {
            Some(f) => refreshed.push(f),
            None => {
                let id = MermaidId(self.next_fence_id);
                self.next_fence_id += 1;
                let f = FenceRef { id, byte_range: range, code };
                new_fences.push(f.clone());
                refreshed.push(f);
            }
        }
    }
    self.known_fences = refreshed.clone();

    // Step 3: build transformed input — substitute fence byte-ranges with
    // a single-line sentinel.
    let transformed = {
        let mut out = String::with_capacity(self.raw_text.len());
        let mut cursor = 0;
        // refreshed is in source order because Parser yields in order.
        for f in &refreshed {
            if f.byte_range.start > cursor {
                out.push_str(&self.raw_text[cursor..f.byte_range.start]);
            }
            out.push_str(&format!("\n\u{0000}MERMAID:{}\u{0000}\n", f.id.0));
            cursor = f.byte_range.end;
        }
        if cursor < self.raw_text.len() {
            out.push_str(&self.raw_text[cursor..]);
        }
        out
    };

    // Step 4: parse transformed text via tui-markdown.
    let text = tui_markdown::from_str(&transformed);
    self.cached_lines = text
        .lines
        .into_iter()
        .map(|line| Line {
            spans: line.spans.into_iter().map(owned_span).collect(),
            style: line.style,
            alignment: line.alignment,
        })
        .collect();

    // Step 5: post-process — replace sentinel lines with styled placeholder.
    for line in &mut self.cached_lines {
        let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if let Some(id_num) = raw.strip_prefix('\u{0000}').and_then(|s| s.strip_suffix('\u{0000}')).and_then(|s| s.strip_prefix("MERMAID:")) {
            let placeholder = format!("[📊 mermaid #{id_num} · press Alt-v to view]");
            *line = Line::from(ratatui::text::Span::styled(
                placeholder,
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Magenta)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
    }

    new_fences
}
```

Add to `crates/spur-tui/Cargo.toml` under `[dependencies]` (conditional on the markdown feature):

```toml
pulldown-cmark = { version = "0.13", default-features = false, optional = true }
```

And extend the feature to pull it in:

```toml
markdown = ["dep:tui-markdown", "dep:mermaid-rs-renderer", "dep:image", "dep:ratatui-image", "dep:pulldown-cmark"]
```

- [ ] **Step 4: Run tests to verify they pass.**

Run: `cargo test -p spur-tui --test markdown_stream_tests`
Expected: all 7 tests pass. If pulldown-cmark's `TagEnd` enum differs across versions, adjust the match.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs crates/spur-tui/Cargo.toml
git commit -m "feat(spur-tui): MarkdownStream detects closed mermaid fences and emits placeholders"
```

---

### Task 5: Wire MarkdownStream into ReactTrace

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs`

- [ ] **Step 1: Extend `TraceEntry` to hold an optional stream.**

Edit `crates/spur-tui/src/components/react_trace.rs`. Change the `TraceEntry` struct definition near the top of the file to:

```rust
#[derive(Debug, Clone)]
pub struct TraceEntry {
    pub kind: TraceKind,
    pub text: String,
    pub timestamp: String,
    /// Only populated for `TraceKind::AgentMessage` when the `markdown` feature
    /// is enabled. The stream owns the raw text for that entry; `text` is
    /// kept in sync for backwards compatibility with non-markdown rendering.
    #[cfg(feature = "markdown")]
    pub markdown: Option<super::markdown_stream::MarkdownStream>,
}
```

Update all construction sites in the same file (there are several `TraceEntry { kind, text, timestamp }` struct-literals in `push_*`/`append_*` methods). For each, add `#[cfg(feature = "markdown")] markdown: None,` or `#[cfg(feature = "markdown")] markdown: Some(MarkdownStream::new()),` as appropriate:
- In `append_think`: `markdown: None`.
- In `append_message` (new AgentMessage entry branch): `markdown: Some(MarkdownStream::new())`.
- In `push` and every other site: `markdown: None`.

Also edit the two existing construction sites in `crates/spur-tui/src/views/session_detail.rs`:
- `push_user_message`: `markdown: None`.
- `replay_history`'s two construction sites: `markdown: None` for UserMessage and AgentMessage (replay does not stream).
- `push_permission`: `markdown: None`.

- [ ] **Step 2: Rewrite `append_message` to feed the stream.**

Replace the body of `ReactTrace::append_message`:

```rust
pub fn append_message(&mut self, text: &str, agent: &str, timestamp: String) {
    #[cfg(feature = "markdown")]
    {
        if let Some(last) = self.entries.last_mut() {
            if let TraceKind::AgentMessage { .. } = last.kind {
                last.text.push_str(text);
                if let Some(stream) = last.markdown.as_mut() {
                    stream.append(text);
                }
                if self.is_following {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
        let mut stream = super::markdown_stream::MarkdownStream::new();
        stream.append(text);
        self.push(TraceEntry {
            kind: TraceKind::AgentMessage { agent: agent.to_string() },
            text: text.to_string(),
            timestamp,
            markdown: Some(stream),
        });
        return;
    }
    #[cfg(not(feature = "markdown"))]
    {
        // Fallback: existing plain append logic.
        if let Some(last) = self.entries.last_mut() {
            if matches!(last.kind, TraceKind::AgentMessage { .. }) {
                last.text.push_str(text);
                if self.is_following {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
        self.push(TraceEntry {
            kind: TraceKind::AgentMessage { agent: agent.to_string() },
            text: text.to_string(),
            timestamp,
        });
    }
}
```

- [ ] **Step 3: Extend `tick` to drive the debounce.**

Add this logic near the top of `ReactTrace::tick`, returning any discovered fences (signature change):

```rust
/// Advance spinner counter, decrement permission countdowns, and flush
/// any debounced markdown streams. Returns newly-detected mermaid fences
/// paired with the containing entry index, so the caller can dispatch
/// `MermaidRenderRequest` actions.
pub fn tick(&mut self) -> Vec<(usize, super::markdown_stream::FenceRef)> {
    self.tick_counter = self.tick_counter.wrapping_add(1);

    for entry in &mut self.entries {
        if let TraceKind::Permission { pending, countdown, .. } = &mut entry.kind {
            if *pending && *countdown > 0 {
                *countdown = countdown.saturating_sub(1);
            }
        }
    }

    let mut fences = Vec::new();
    #[cfg(feature = "markdown")]
    {
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if let Some(stream) = entry.markdown.as_mut() {
                for fence in stream.maybe_flush() {
                    fences.push((idx, fence));
                }
            }
        }
    }
    fences
}
```

Under `#[cfg(not(feature = "markdown"))]`, change the return type to `Vec<(usize, ())>` or gate the signature — simpler to always return the same shape by declaring `FenceRef` in both builds (move `FenceRef` out of `markdown_stream` into a shared location OR define an empty dummy under the non-feature build).

**Simpler alternative:** keep `tick()` returning `()` and introduce a separate method `ReactTrace::drain_fence_dispatches() -> Vec<FenceDispatch>` that callers invoke after `tick`. Under `#[cfg(not(feature = "markdown"))]`, the method returns `Vec::new()`. Use this alternative to avoid changing the `View::tick` trait signature.

Apply the simpler alternative now:

```rust
pub fn tick(&mut self) {
    self.tick_counter = self.tick_counter.wrapping_add(1);
    for entry in &mut self.entries {
        if let TraceKind::Permission { pending, countdown, .. } = &mut entry.kind {
            if *pending && *countdown > 0 {
                *countdown = countdown.saturating_sub(1);
            }
        }
    }
}

/// Drain any mermaid fences detected during the last debounce window.
/// Returns (entry_index, FenceRef) pairs. Empty if the `markdown` feature
/// is disabled.
#[cfg(feature = "markdown")]
pub fn drain_fence_dispatches(&mut self) -> Vec<(usize, super::markdown_stream::FenceRef)> {
    let mut out = Vec::new();
    for (idx, entry) in self.entries.iter_mut().enumerate() {
        if let Some(stream) = entry.markdown.as_mut() {
            for fence in stream.maybe_flush() {
                out.push((idx, fence));
            }
        }
    }
    out
}

#[cfg(not(feature = "markdown"))]
pub fn drain_fence_dispatches(&mut self) -> Vec<(usize, ())> {
    Vec::new()
}
```

- [ ] **Step 4: Update `render` to use stream lines when present.**

In `ReactTrace::render`, inside the `match &entry.kind { ... }` block, change the `TraceKind::AgentMessage { agent }` arm body to:

```rust
TraceKind::AgentMessage { agent } => {
    // Header line: timestamp + "✉ agent_name"
    lines.push(Line::from(vec![
        ts_span.clone(),
        Span::styled(
            format!("✉ {}", agent),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    #[cfg(feature = "markdown")]
    let used_markdown = {
        if let Some(stream) = entry.markdown.as_ref() {
            for line in stream.lines() {
                // Indent markdown lines by 3 spaces to match existing format.
                let mut spans = vec![Span::raw("   ")];
                spans.extend(line.spans.iter().cloned());
                lines.push(Line { spans, style: line.style, alignment: line.alignment });
            }
            true
        } else {
            false
        }
    };
    #[cfg(not(feature = "markdown"))]
    let used_markdown = false;

    if !used_markdown {
        for text_line in entry.text.lines() {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    text_line.to_string(),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
    }
}
```

- [ ] **Step 5: Verify build.**

Run: `cargo build -p spur-tui`
Expected: success.
Run: `cargo build -p spur-tui --no-default-features`
Expected: success (both feature axes compile).

- [ ] **Step 6: Run existing tests.**

Run: `cargo test -p spur-tui`
Expected: all existing tests still pass. New markdown_stream and mermaid_render tests continue to pass.

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): route AgentMessage entries through MarkdownStream with feature gate"
```

---

### Task 6: Add new Action and ViewId variants

**Files:**
- Modify: `crates/spur-tui/src/action.rs`

- [ ] **Step 1: Extend `ViewId`.**

Edit `crates/spur-tui/src/action.rs`. Change the `ViewId` enum to:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewId {
    Dashboard,
    SessionDetail(SessionId),
    SessionPicker,
    /// Full-screen overlay showing rendered mermaid diagrams from a session.
    MermaidOverlay(SessionId),
}
```

- [ ] **Step 2: Extend `Action`.**

Add two new variants at the end of the `Action` enum:

```rust
    /// Request the app to render a mermaid diagram on a blocking worker.
    /// Emitted by `SessionDetailView::tick` when a new fence closes.
    MermaidRenderRequest {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        code: String,
    },
    /// Completion of a previously-dispatched render request.
    MermaidRenderCompleted {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        result: Result<image::DynamicImage, String>,
    },
```

Wrap both new variants in `#[cfg(feature = "markdown")]`.

Because `Action` derives `Clone`, and `image::DynamicImage` is `Clone`, that's fine. `Debug` on `DynamicImage` is implemented. OK.

- [ ] **Step 3: Verify build.**

Run: `cargo build -p spur-tui`
Expected: success.
Run: `cargo build -p spur-tui --no-default-features`
Expected: success.

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-tui/src/action.rs
git commit -m "feat(spur-tui): add MermaidOverlay ViewId and mermaid-render Action variants"
```

---

### Task 7: App-level dispatch via spawn_blocking

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Add a Picker field and a completed-results channel to `App`.**

At the top of `app.rs` add:

```rust
#[cfg(feature = "markdown")]
use ratatui_image::picker::Picker;
```

In the `App` struct definition, add (inside the struct, gated):

```rust
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_picker: Option<Picker>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_rx: tokio::sync::mpsc::UnboundedReceiver<Action>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_tx: tokio::sync::mpsc::UnboundedSender<Action>,
```

In the `App::new` constructor (locate the existing constructor by grep for `fn new` in app.rs), initialize:

```rust
#[cfg(feature = "markdown")]
let mermaid_picker = Picker::from_query_stdio().ok();
#[cfg(feature = "markdown")]
let (mermaid_tx, mermaid_rx) = tokio::sync::mpsc::unbounded_channel();
```

…and include them in the returned struct literal.

- [ ] **Step 2: Add action dispatch arms.**

Find the `match` block that dispatches `Action` variants in `app.rs` (grep for `Action::Quit => {`). After the existing arms, add:

```rust
            #[cfg(feature = "markdown")]
            Action::MermaidRenderRequest { session, ref_id, code } => {
                let tx = self.mermaid_tx.clone();
                let session_cloned = session.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::components::mermaid::render_mermaid(&code)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Action::MermaidRenderCompleted {
                        session: session_cloned,
                        ref_id,
                        result,
                    });
                });
            }
            #[cfg(feature = "markdown")]
            Action::MermaidRenderCompleted { session, ref_id, result } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id().0 == session.0 {
                        detail.handle_mermaid_completed(ref_id, result);
                        self.dirty = true;
                    }
                }
            }
            #[cfg(feature = "markdown")]
            Action::NavigateTo(ViewId::MermaidOverlay(ref _session)) => {
                self.current_view = action.clone_view_target().unwrap_or(self.current_view.clone());
                // (If the existing NavigateTo arm already covers ViewId uniformly,
                // remove this specialized arm and let the generic handler work.)
            }
```

Inspect the existing `Action::NavigateTo(ViewId::…)` arms; if they dispatch generically (`Action::NavigateTo(v) => { self.current_view = v; ... }`), drop the third arm above and rely on the generic path.

- [ ] **Step 3: Drain mermaid completions each tick.**

In `App::tick` (near `app.rs:581`), before the `match self.current_view`, add:

```rust
#[cfg(feature = "markdown")]
{
    while let Ok(action) = self.mermaid_rx.try_recv() {
        self.handle_action(action);
    }
}
```

…where `handle_action` is the existing dispatcher function. (If the existing dispatcher has a different name, substitute it.)

- [ ] **Step 4: Verify build.**

Run: `cargo build -p spur-tui`
Expected: success.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): dispatch mermaid renders via spawn_blocking; drain results on tick"
```

---

### Task 8: SessionDetailView mermaid registry and fence dispatch

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Add the registry and completion handler.**

Add two fields to `SessionDetailView` (gated by the feature):

```rust
    #[cfg(feature = "markdown")]
    pub mermaid_registry: std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    #[cfg(feature = "markdown")]
    pub pending_fence_actions: std::collections::VecDeque<crate::action::Action>,
```

Initialize both to defaults in `SessionDetailView::new`.

Add a method on `SessionDetailView`:

```rust
#[cfg(feature = "markdown")]
pub fn handle_mermaid_completed(
    &mut self,
    ref_id: crate::components::mermaid::MermaidId,
    result: Result<image::DynamicImage, String>,
) {
    use crate::components::mermaid::MermaidState;
    let state = match result {
        Ok(image) => MermaidState::Ready { image },
        Err(message) => MermaidState::Error { message },
    };
    self.mermaid_registry.insert(ref_id, state);
}
```

- [ ] **Step 2: Drain fence dispatches during `tick`.**

Find `impl View for SessionDetailView { fn tick(&mut self) { self.react_trace.tick(); } }` and replace with:

```rust
fn tick(&mut self) {
    self.react_trace.tick();
    #[cfg(feature = "markdown")]
    {
        use crate::components::mermaid::{MermaidState};
        for (_idx, fence) in self.react_trace.drain_fence_dispatches() {
            // Mark the slot as Pending immediately so the placeholder can
            // distinguish "not yet rendered" from "not yet discovered."
            self.mermaid_registry.insert(
                fence.id,
                MermaidState::Pending { code: fence.code.clone() },
            );
            self.pending_fence_actions.push_back(
                crate::action::Action::MermaidRenderRequest {
                    session: self.session_id.clone(),
                    ref_id: fence.id,
                    code: fence.code,
                },
            );
        }
    }
}
```

- [ ] **Step 3: Expose pending actions so the app can pull them.**

Add:

```rust
#[cfg(feature = "markdown")]
pub fn take_pending_actions(&mut self) -> Vec<crate::action::Action> {
    self.pending_fence_actions.drain(..).collect()
}
```

In `App::tick` (app.rs), after calling `detail.tick()`, drain these actions:

```rust
#[cfg(feature = "markdown")]
{
    for action in detail.take_pending_actions() {
        self.handle_action(action);
    }
}
```

- [ ] **Step 4: Add the `Alt-v` key binding.**

In `SessionDetailView::handle_key`, near the existing `Alt-m` handler, add:

```rust
// Alt-v → open the mermaid overlay for this session.
#[cfg(feature = "markdown")]
if matches!(key.code, KeyCode::Char('v')) && key.modifiers.contains(KeyModifiers::ALT) {
    return Some(Action::NavigateTo(ViewId::MermaidOverlay(self.session_id.clone())));
}
```

- [ ] **Step 5: Verify build and existing tests.**

Run: `cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): session-detail mermaid registry + Alt-v opens overlay"
```

---

### Task 9: MermaidViewerView overlay

**Files:**
- Create: `crates/spur-tui/src/views/mermaid_viewer.rs`
- Modify: `crates/spur-tui/src/views/mod.rs`

- [ ] **Step 1: Create the overlay view.**

Create `crates/spur-tui/src/views/mermaid_viewer.rs`:

```rust
#![cfg(feature = "markdown")]

//! Full-screen overlay that renders a single mermaid diagram from the
//! active session's registry via `ratatui-image`'s `StatefulImage`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol, Resize, StatefulImage};
use spur_acp::{SessionId, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::mermaid::{MermaidId, MermaidState};

use super::View;

pub struct MermaidViewerView {
    session_id: SessionId,
    /// Which diagram in the registry is currently focused. `None` until
    /// the first render selects the most recent Ready entry.
    focused: Option<MermaidId>,
    /// Lazily-built protocol, bound to the currently focused image.
    protocol: Option<StatefulProtocol>,
}

impl MermaidViewerView {
    pub fn new(session_id: SessionId) -> Self {
        Self { session_id, focused: None, protocol: None }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Supply the sorted list of (id, state) pairs from the session's
    /// mermaid registry. Called by `App::render` before delegating draw.
    pub fn set_available(
        &mut self,
        mut entries: Vec<(MermaidId, &MermaidState)>,
        picker: Option<&Picker>,
    ) {
        entries.sort_by_key(|(id, _)| *id);
        // Default focus to most recent Ready entry.
        if self.focused.is_none() {
            self.focused = entries
                .iter()
                .rev()
                .find(|(_, s)| matches!(s, MermaidState::Ready { .. }))
                .map(|(id, _)| *id);
        }
        // Rebuild protocol if focus changed and we have a picker.
        if let (Some(id), Some(picker)) = (self.focused, picker) {
            if self.protocol.is_none() {
                if let Some(MermaidState::Ready { image }) = entries.iter().find(|(i, _)| *i == id).map(|(_, s)| s) {
                    self.protocol = Some(picker.new_resize_protocol((*image).clone()));
                }
            }
        }
    }

    /// Cycle focus among available Ready entries.
    pub fn cycle(&mut self, entries: &[(MermaidId, &MermaidState)], forward: bool) {
        let ready_ids: Vec<MermaidId> = entries
            .iter()
            .filter(|(_, s)| matches!(s, MermaidState::Ready { .. }))
            .map(|(id, _)| *id)
            .collect();
        if ready_ids.is_empty() {
            self.focused = None;
            self.protocol = None;
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
        self.protocol = None; // force rebuild on next set_available
    }
}

impl View for MermaidViewerView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::NavigateBack),
            KeyCode::Char('[') => {
                // Cycle is done from app context where entries are available.
                // Emit a no-op action; app translates via its own handler if
                // needed. For v1 we rely on `App::render` calling `cycle()`
                // before the next draw via an internal signal.
                None
            }
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent) {}

    fn render(&self, frame: &mut Frame, area: Rect) {
        let [title_area, body, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(area);

        let title = Line::from(vec![ratatui::text::Span::styled(
            " Mermaid Viewer — press q/Esc to close ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )]);
        frame.render_widget(Paragraph::new(title), title_area);

        let hint_line = Line::from(ratatui::text::Span::styled(
            " [/]: cycle · q/Esc: close ",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(hint_line), hint);

        match &self.protocol {
            Some(_) => {
                // Cannot borrow protocol mutably from &self render; the App
                // layer holds the mutable protocol and passes it via a
                // thin wrapper. See Task 10 for the mutable render path.
                let placeholder = Paragraph::new("(image ready — rendered by app layer)")
                    .style(Style::default().fg(Color::DarkGray));
                frame.render_widget(placeholder.block(Block::default().borders(Borders::ALL)), body);
            }
            None => {
                let msg = match self.focused {
                    Some(id) => format!("Diagram #{} — not ready or no graphics protocol available.", id.0),
                    None => "No mermaid diagrams rendered yet.".to_string(),
                };
                let p = Paragraph::new(msg)
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(" status "));
                frame.render_widget(p, body);
            }
        }
    }

    fn tick(&mut self) {}
}
```

**Note:** `StatefulImage` requires `&mut StatefulProtocol`, but `View::render` is `&self`. The clean fix is to bend the render flow: `App::render` detects a `MermaidOverlay` view and calls an internal `render_mermaid_overlay(frame, area, session_detail, view)` that has the needed mutable access. The `View` trait impl above is kept as a stub so the view still satisfies the trait; the real drawing happens in app-level code in Task 10.

- [ ] **Step 2: Register the module.**

Edit `crates/spur-tui/src/views/mod.rs`:

```rust
#[cfg(feature = "markdown")]
pub mod mermaid_viewer;
```

- [ ] **Step 3: Verify build.**

Run: `cargo build -p spur-tui`
Expected: success.

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-tui/src/views/mermaid_viewer.rs crates/spur-tui/src/views/mod.rs
git commit -m "feat(spur-tui): add MermaidViewerView overlay skeleton"
```

---

### Task 10: Wire overlay into App navigation and draw

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Add `MermaidOverlay` handling in navigation.**

Locate the `match action` dispatcher in `app.rs`. In the `Action::NavigateTo(ViewId::...)` arms, add handling for `ViewId::MermaidOverlay`:

```rust
#[cfg(feature = "markdown")]
Action::NavigateTo(ViewId::MermaidOverlay(ref session)) => {
    use crate::views::mermaid_viewer::MermaidViewerView;
    self.mermaid_viewer = Some(MermaidViewerView::new(session.clone()));
    self.current_view = ViewId::MermaidOverlay(session.clone());
    self.dirty = true;
}
```

Add a field to `App`:

```rust
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_viewer: Option<crate::views::mermaid_viewer::MermaidViewerView>,
```

Initialize to `None` in `App::new`.

In the existing `Action::NavigateBack` arm, if `current_view` is a `MermaidOverlay`, transition back to the corresponding `SessionDetail`:

```rust
Action::NavigateBack => {
    #[cfg(feature = "markdown")]
    if let ViewId::MermaidOverlay(ref session) = self.current_view {
        self.current_view = ViewId::SessionDetail(session.clone());
        self.mermaid_viewer = None;
        self.dirty = true;
        return;
    }
    // … existing NavigateBack logic preserved below …
}
```

- [ ] **Step 2: Render the overlay from `App::render`.**

In the existing `App::render` (search for `fn draw` / `terminal.draw`), add:

```rust
#[cfg(feature = "markdown")]
ViewId::MermaidOverlay(ref session) => {
    if let (Some(detail), Some(viewer)) = (self.session_detail.as_ref(), self.mermaid_viewer.as_mut()) {
        if detail.session_id().0 == session.0 {
            let entries: Vec<(
                crate::components::mermaid::MermaidId,
                &crate::components::mermaid::MermaidState,
            )> = detail
                .mermaid_registry
                .iter()
                .map(|(k, v)| (*k, v))
                .collect();
            viewer.set_available(entries.clone(), self.mermaid_picker.as_ref());
            render_mermaid_overlay(frame, area, viewer, &entries);
            return;
        }
    }
}
```

Add the helper (outside the `App` impl in `app.rs`):

```rust
#[cfg(feature = "markdown")]
fn render_mermaid_overlay(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    viewer: &mut crate::views::mermaid_viewer::MermaidViewerView,
    _entries: &[(
        crate::components::mermaid::MermaidId,
        &crate::components::mermaid::MermaidState,
    )],
) {
    use ratatui::{layout::{Constraint, Layout}, style::{Color, Modifier, Style}, text::Line, widgets::Paragraph};
    use ratatui_image::{Resize, StatefulImage};

    let [title_area, body, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(ratatui::text::Span::styled(
            " Mermaid Viewer ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))),
        title_area,
    );

    if let Some(protocol) = viewer_protocol_mut(viewer) {
        let widget = StatefulImage::default().resize(Resize::Fit(None));
        frame.render_stateful_widget(widget, body, protocol);
    } else {
        frame.render_widget(
            Paragraph::new("No diagram available yet. Wait for render to complete.")
                .style(Style::default().fg(Color::DarkGray)),
            body,
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(ratatui::text::Span::styled(
            " [/]: cycle · q/Esc: close ",
            Style::default().fg(Color::DarkGray),
        ))),
        hint,
    );
}

#[cfg(feature = "markdown")]
fn viewer_protocol_mut<'a>(
    viewer: &'a mut crate::views::mermaid_viewer::MermaidViewerView,
) -> Option<&'a mut ratatui_image::protocol::StatefulProtocol> {
    // Access via a pub(crate) accessor on the view. Add the accessor below.
    viewer.protocol_mut()
}
```

And expose the accessor on `MermaidViewerView` (in `mermaid_viewer.rs`):

```rust
impl MermaidViewerView {
    pub(crate) fn protocol_mut(&mut self) -> Option<&mut StatefulProtocol> {
        self.protocol.as_mut()
    }
}
```

- [ ] **Step 3: Route keys to the overlay when active.**

Find the key-dispatch in `app.rs` (search for `handle_key`). Add a branch:

```rust
#[cfg(feature = "markdown")]
ViewId::MermaidOverlay(_) => {
    if let Some(viewer) = self.mermaid_viewer.as_mut() {
        match key.code {
            KeyCode::Char('[') | KeyCode::Char(']') => {
                if let Some(detail) = self.session_detail.as_ref() {
                    let entries: Vec<_> = detail
                        .mermaid_registry
                        .iter()
                        .map(|(k, v)| (*k, v))
                        .collect();
                    viewer.cycle(&entries, key.code == KeyCode::Char(']'));
                    self.dirty = true;
                }
                None
            }
            _ => viewer.handle_key(key),
        }
    } else {
        None
    }
}
```

- [ ] **Step 4: Verify build.**

Run: `cargo build -p spur-tui`
Expected: success.
Run: `cargo build -p spur-tui --no-default-features`
Expected: success.

- [ ] **Step 5: Manual smoke test.**

Run: `cargo run -p spur-cli -- dashboard` (or whatever the existing binary entry point is — check `crates/spur-cli/src/main.rs` for the command), navigate to a session, paste (or have the agent emit) a mermaid fence, wait 50 ms, press **Alt-v**.
Expected: overlay opens; if on a terminal with Kitty/iTerm2/Sixel graphics support, the diagram is rendered. On Alacritty/macOS Terminal.app, chafa falls back to Unicode rendering. Press `q` / `Esc` to close.

If no graphics protocol is detected (`Picker::from_query_stdio()` returns `Err`), the overlay shows the "No diagram available" placeholder — expected degraded behavior.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/mermaid_viewer.rs
git commit -m "feat(spur-tui): render mermaid overlay via ratatui-image, cycle with [/]"
```

---

### Task 11: Document bindings in help overlay

**Files:**
- Modify: `crates/spur-tui/src/components/help_overlay.rs`

- [ ] **Step 1: Add binding hints to the Session Detail section.**

Edit `crates/spur-tui/src/components/help_overlay.rs`. In the `help_text` vector inside `HelpOverlay::render`, inside the "Session Detail" section (after the existing `y / n / a` line and before the closing blank Line), insert three new lines:

```rust
            Line::from("  Alt-m              Toggle plan mode"),
            Line::from("  Alt-v              Open mermaid diagram viewer"),
            Line::from(""),
            Line::from(Span::styled(
                " Mermaid Viewer (overlay)",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  [ / ]              Cycle diagrams"),
            Line::from("  q / Esc            Close overlay"),
```

Bump the popup height if the new block overflows. The popup is currently sized 66×30; change to 66×34:

```rust
let height = 34u16.min(area.height.saturating_sub(4));
```

- [ ] **Step 2: Verify build.**

Run: `cargo build -p spur-tui`
Expected: success.

- [ ] **Step 3: Commit.**

```bash
git add crates/spur-tui/src/components/help_overlay.rs
git commit -m "docs(spur-tui): document Alt-v and mermaid viewer bindings in help overlay"
```

---

### Task 12: Verify feature-off build and run full test suite

**Files:** (none changed — verification task)

- [ ] **Step 1: Feature-off build.**

Run: `cargo build -p spur-tui --no-default-features`
Expected: success. If any module or use-path is not gated, fix it — non-markdown builds must not reference the new crates.

- [ ] **Step 2: Feature-on build.**

Run: `cargo build -p spur-tui`
Expected: success, no warnings beyond pre-existing ones.

- [ ] **Step 3: Full test suite.**

Run: `cargo test -p spur-tui`
Expected: all tests pass, including the new `markdown_stream_tests` (7 cases) and `mermaid_render_tests` (3 cases).

- [ ] **Step 4: Full workspace build.**

Run: `cargo build`
Expected: success. Validates the MSRV bump works for all workspace crates.

- [ ] **Step 5: Commit any verification-driven fixups.**

If steps 1–4 uncovered small issues, commit them as a single cleanup commit:

```bash
git add -A
git commit -m "fix(spur-tui): feature-gate fixups uncovered by final verification"
```

If no fixups are needed, skip.

- [ ] **Step 6: Update the spec status.**

Edit `docs/superpowers/specs/2026-04-13-session-detail-markdown-mermaid-design.md`: change `**Status:** Proposed` to `**Status:** Implemented`. Commit:

```bash
git add docs/superpowers/specs/2026-04-13-session-detail-markdown-mermaid-design.md
git commit -m "docs: mark markdown+mermaid spec implemented"
```

---

## Self-review results

Spec coverage:

| Spec section | Task(s) |
|---|---|
| 4.1 Architectural — inline markdown, overlay mermaid, AgentMessage scope, placeholder line | Tasks 3, 4, 5, 9, 10 |
| 4.2 Streaming parse strategy — 50 ms coalesce + full reparse | Task 3 (debounce constant + flush API), Task 5 (tick drives flush) |
| 4.3 Crate stack | Task 1 |
| 4.4 MSRV bump | Task 1 |
| 4.5 Feature gate `markdown` | Task 1, gates verified in Task 12 |
| 5.2 Module layout | Tasks 2, 3, 9 (new files); Tasks 5, 6, 7, 8, 10, 11 (modifications) |
| 5.3 Data flow steps 1–6 | Task 5 (append → dirty), Task 5+8 (tick debounce), Task 4 (pre-scan + sentinel), Task 7 (worker), Task 8 (completion registry), Task 10 (overlay) |
| 5.4 Error handling — renderer error / decode error / picker unavailable / feature absent | Task 2 (RenderError), Task 8 (MermaidState::Error), Task 10 (picker-None fallback), Task 12 (feature-off build) |
| 5.5 Invariants — Paragraph-Lines preserved, scroll math untouched | Task 5 (only changes Line source for AgentMessage; no scroll-math changes) |
| Section 6 Testing — unit equivalence, fence dedup, sentinel survival; render ok/err; catch_unwind | Tasks 2, 3, 4 |

Placeholder scan: no TBD/TODO/`implement later` strings. Every step contains either code, a shell command, or a specific file edit.

Type consistency:
- `MermaidId` defined in Task 2 (`components::mermaid::MermaidId`), used in Tasks 3, 4, 6, 8, 9, 10.
- `MermaidState` defined in Task 2, used in Tasks 6, 8, 9, 10.
- `RenderError` defined in Task 2, used only there.
- `MarkdownStream` defined in Task 3, used in Tasks 5.
- `FenceRef` defined in Task 3 (struct with fields `id`, `byte_range`, `code`), used in Tasks 3, 4, 5, 8. Field set matches across tasks.
- `Action::MermaidRenderRequest` / `MermaidRenderCompleted` — fields declared in Task 6, referenced in Tasks 7, 8.
- `ViewId::MermaidOverlay(SessionId)` declared in Task 6, used in Tasks 8 (navigation emit), 10 (routing + render).
- `App.mermaid_picker`, `App.mermaid_viewer`, `App.mermaid_tx/rx` declared in Task 7 and Task 10, used in Task 10.
- `SessionDetailView.mermaid_registry`, `take_pending_actions`, `handle_mermaid_completed` declared in Task 8, used in Tasks 7, 10.

All signatures, field names, and cross-task references are consistent.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-13-session-detail-markdown-mermaid.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
