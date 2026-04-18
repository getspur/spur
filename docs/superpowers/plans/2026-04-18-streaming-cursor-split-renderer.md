# Streaming Cursor-Split Renderer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate "ghost text" during sustained `AgentMessageChunk` streaming by replacing `MarkdownStream`'s all-or-nothing cache with a cursor-split renderer: `cached_items` covers a committed prefix of `raw_text`; the uncommitted tail renders every paint as plain text.

**Architecture:** Two-stage `rebuild()` — Stage 0 scans `raw_text` with `pulldown_cmark::into_offset_iter()` to find the maximum `Event::End` whose depth returns to 0 and whose `range.end < raw_text.len()` (authoritative closure); Stage 1 parses only `raw_text[..flushed_byte_len]` via `tui_markdown::from_str`. Renderers consume `items_and_tail()` and emit the tail as plain white text under the same indent.

**Tech Stack:** Rust, pulldown-cmark 0.13, tui-markdown 0.3, ratatui 0.29. Changes scoped to `crates/spur-tui`.

**Spec:** `docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md`
**Grounding tests (already committed):** `crates/spur-tui/tests/pulldown_cmark_grounding.rs`
**RCA:** `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md`

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-tui/src/components/markdown_stream.rs` | modify | state additions, `scan_authoritative`, two-stage `rebuild`, `flush_final`, multi-condition `maybe_flush`, `has_authoritative_closure_pattern`, accessor additions |
| `crates/spur-tui/src/components/react_trace/builder.rs` | modify | `render_agent_message_body` shared helper; migrate both render paths |
| `crates/spur-tui/src/components/react_trace/mod.rs` | modify | `force_flush_all` uses `flush_final` |
| `crates/spur-tui/tests/markdown_stream_tests.rs` | modify | T2.*, T3.*, T5.* test additions |
| `crates/spur-tui/src/components/react_trace/streaming_tests.rs` | create | TI.1, TI.2, TI.3 integration tests |

No changes to: `app.rs`, `session_detail.rs`, event plumbing, schemas, MCP surface, `DRAIN_CAP_PER_FRAME`.

---

## Phase 1 — State and Accessors

### Task 1: Add `flushed_byte_len` field with default = 0

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs`
- Test: `crates/spur-tui/tests/markdown_stream_tests.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn flushed_byte_len_starts_at_zero() {
    let s = MarkdownStream::new();
    assert_eq!(s.flushed_byte_len_for_tests(), 0);
}
```

- [ ] **Step 2: Run test, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests flushed_byte_len_starts_at_zero --features markdown`
Expected: compilation error "no method `flushed_byte_len_for_tests` on `MarkdownStream`"

- [ ] **Step 3: Add the field and the test-only accessor**

In `markdown_stream.rs`, in the `MarkdownStream` struct (currently ending around line 190), add the field:

```rust
pub struct MarkdownStream {
    raw_text: String,
    dirty_since: Option<Instant>,
    cached_items: Vec<StreamItem>,
    fence_placeholders: std::collections::HashMap<MermaidId, Line<'static>>,
    known_fences: Vec<FenceRef>,
    next_fence_id: u64,
    mermaid_enabled: bool,

    /// Byte offset up to which `cached_items` is authoritative.
    /// Invariant (C1): cached_items, known_fences, fence_placeholders
    /// jointly represent the parsed-decorated form of raw_text[..flushed_byte_len].
    flushed_byte_len: usize,
}
```

Update the `Default` impl:

```rust
impl Default for MarkdownStream {
    fn default() -> Self {
        Self {
            raw_text: String::new(),
            dirty_since: None,
            cached_items: Vec::new(),
            fence_placeholders: std::collections::HashMap::new(),
            known_fences: Vec::new(),
            next_fence_id: 0,
            mermaid_enabled: true,
            flushed_byte_len: 0,
        }
    }
}
```

Add the test-only accessor inside `impl MarkdownStream`:

```rust
    #[cfg(test)]
    pub fn flushed_byte_len_for_tests(&self) -> usize {
        self.flushed_byte_len
    }
```

- [ ] **Step 4: Run test, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests flushed_byte_len_starts_at_zero --features markdown`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): add flushed_byte_len cursor to MarkdownStream

First step of the cursor-split renderer. Field is added but not yet
consulted by rebuild() or any renderer. Test-only accessor exposed for
subsequent task verification.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md
EOF
)"
```

### Task 2: Add `finalized: bool` field with `is_finalized()` accessor

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn finalized_starts_false() {
    let s = MarkdownStream::new();
    assert!(!s.is_finalized());
}
```

- [ ] **Step 2: Run test, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests finalized_starts_false --features markdown`
Expected: compile error "no method `is_finalized`".

- [ ] **Step 3: Add field and accessor**

In `markdown_stream.rs`, add to the struct:

```rust
    /// Set by `flush_final` when the stream is finalized (TurnComplete).
    /// `append` after finalize is a contract violation; enforced via
    /// debug_assert (see Task 14).
    finalized: bool,
```

Add to the `Default` impl: `finalized: false,`.

Add public accessor:

```rust
    pub fn is_finalized(&self) -> bool {
        self.finalized
    }
```

- [ ] **Step 4: Run test, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests finalized_starts_false --features markdown`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): add finalized flag + is_finalized accessor

Companion to flushed_byte_len for TurnComplete finalization (Section 5.1).
Consumed by flush_final in a later task.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md"
```

### Task 3: Add `items_and_tail()` accessor

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn items_and_tail_empty_stream() {
    let s = MarkdownStream::new();
    let (items, tail) = s.items_and_tail();
    assert_eq!(items.len(), 0);
    assert_eq!(tail, "");
}

#[test]
fn items_and_tail_before_flush_shows_entire_raw_text_as_tail() {
    let mut s = MarkdownStream::new();
    s.append("Hello world");
    let (items, tail) = s.items_and_tail();
    assert_eq!(items.len(), 0, "no flush yet, no committed items");
    assert_eq!(tail, "Hello world", "all raw_text should be in the tail");
}
```

- [ ] **Step 2: Run tests, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests items_and_tail --features markdown`
Expected: compile error "no method `items_and_tail`".

- [ ] **Step 3: Add the accessor**

In `markdown_stream.rs` inside `impl MarkdownStream`, add:

```rust
    /// Split view of committed parsed items + uncommitted tail text.
    ///
    /// - `items`: parsed StreamItems covering `raw_text[..flushed_byte_len]`.
    /// - `tail`: `raw_text[flushed_byte_len..]`, to be rendered as plain text.
    ///
    /// Renderers must emit both: items styled, tail plain.
    pub fn items_and_tail(&self) -> (&[StreamItem], &str) {
        (&self.cached_items, &self.raw_text[self.flushed_byte_len..])
    }
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests items_and_tail --features markdown`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): add items_and_tail() accessor on MarkdownStream

Primary API for renderers: returns (committed items, uncommitted tail).
Not yet consumed by builder.rs; migration follows in Phase 5.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md"
```

### Task 4: Add `fence_placeholder_for(id)` accessor

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn fence_placeholder_for_unknown_id_returns_none() {
    use spur_tui::components::mermaid::MermaidId;
    let s = MarkdownStream::new();
    assert!(s.fence_placeholder_for(MermaidId(999)).is_none());
}
```

- [ ] **Step 2: Run test, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests fence_placeholder_for_unknown --features markdown`
Expected: compile error "no method `fence_placeholder_for`".

- [ ] **Step 3: Add the accessor**

In `markdown_stream.rs` inside `impl MarkdownStream`, add:

```rust
    /// Look up the state-aware placeholder line for a previously-registered
    /// fence id. Returns `None` for ids not in `fence_placeholders`.
    /// Used by `build_display_lines` (secondary render path) to render a
    /// placeholder line without constructing a `FenceRender` HashMap.
    pub fn fence_placeholder_for(&self, id: MermaidId) -> Option<Line<'static>> {
        self.fence_placeholders.get(&id).cloned()
    }
```

- [ ] **Step 4: Run test, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests fence_placeholder_for_unknown --features markdown`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): add fence_placeholder_for accessor

Encapsulates the fence_placeholders HashMap lookup for the secondary
render path (build_display_lines), which doesn't receive a FenceRender
state map.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md"
```

---

## Phase 2 — `scan_authoritative` Function

### Task 5: Implement `scan_authoritative` with depth gating

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs`
- Test: `crates/spur-tui/tests/markdown_stream_tests.rs`

**Context:** The grounding tests at `crates/spur-tui/tests/pulldown_cmark_grounding.rs` define the behavior this function must match. Re-reading them confirms the contract.

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn scan_authoritative_empty_input() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let (end, fences) = scan_authoritative_for_tests("", /*mermaid*/ true, /*permit_eof*/ false);
    assert_eq!(end, 0);
    assert!(fences.is_empty());
}

#[test]
fn scan_authoritative_paragraph_at_eof_not_authoritative() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let (end, _) = scan_authoritative_for_tests("Hello", true, false);
    assert_eq!(end, 0, "paragraph at EOF must not advance");
}

#[test]
fn scan_authoritative_paragraph_with_content_after_advances() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let input = "Hello\n\nworld";
    let (end, _) = scan_authoritative_for_tests(input, true, false);
    assert!(end > 0 && end < input.len(),
        "end={} len={}", end, input.len());
}

#[test]
fn scan_authoritative_open_list_not_authoritative() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    // Depth-gate test: open list at EOF must not leak End(Item) authority.
    let (end, _) = scan_authoritative_for_tests("- item1\n- item2\n", true, false);
    assert_eq!(end, 0);
}

#[test]
fn scan_authoritative_eof_permissive_commits_final_paragraph() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let input = "Hello";
    let (end, _) = scan_authoritative_for_tests(input, true, /*permit_eof*/ true);
    assert_eq!(end, input.len());
}

#[test]
fn scan_authoritative_registers_closed_mermaid_fence() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let input = "```mermaid\nflowchart LR\nA-->B\n```\nmore\n";
    let (_, fences) = scan_authoritative_for_tests(input, true, false);
    assert_eq!(fences.len(), 1);
    assert!(fences[0].1.contains("flowchart LR"));
}

#[test]
fn scan_authoritative_does_not_register_fence_at_eof() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    // Fence close is the last byte; coherence fix must not register it.
    let input = "```mermaid\nflowchart LR\nA-->B\n```";
    let (_, fences) = scan_authoritative_for_tests(input, true, false);
    assert_eq!(fences.len(), 0, "fence ending at EOF must not register");
}
```

- [ ] **Step 2: Run tests, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests scan_authoritative --features markdown`
Expected: compile error "no function `scan_authoritative_for_tests`".

- [ ] **Step 3: Implement `scan_authoritative`**

In `markdown_stream.rs`, add this free function outside the `impl MarkdownStream` block (near the bottom, before the conversion helpers):

```rust
/// Pulldown scan over `raw_text`, gathering:
/// - `authoritative_end`: max byte offset where an Event::End brings
///   nesting depth back to 0 AND `range.end < raw_text.len()` (or
///   `<= len` when `permit_eof_closure` is true for flush_final).
/// - `discovered_fences`: closed mermaid fences whose End range is also
///   before EOF (coherence with cursor advance, per Section 5.9).
///
/// Pure over `(&str, bool, bool)`. Does no `tui_markdown` work.
pub(crate) fn scan_authoritative(
    raw_text: &str,
    mermaid_enabled: bool,
    permit_eof_closure: bool,
) -> (usize, Vec<(std::ops::Range<usize>, String)>) {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut max_end: usize = 0;
    let mut depth: i32 = 0;
    let mut discovered: Vec<(std::ops::Range<usize>, String)> = Vec::new();

    let mut open_fence_start: Option<usize> = None;
    let mut fence_buf = String::new();

    for (ev, range) in Parser::new_ext(raw_text, Options::empty()).into_offset_iter() {
        match &ev {
            Event::Start(tag) => {
                depth += 1;
                if mermaid_enabled {
                    if let Tag::CodeBlock(CodeBlockKind::Fenced(info)) = tag {
                        if info.as_ref().trim().eq_ignore_ascii_case("mermaid") {
                            open_fence_start = Some(range.start);
                            fence_buf.clear();
                        }
                    }
                }
            }
            Event::Text(t) if open_fence_start.is_some() => {
                fence_buf.push_str(t);
            }
            Event::End(tag_end) => {
                depth -= 1;
                // Authoritative cursor advance: top-level block close.
                let permitted = if permit_eof_closure {
                    range.end <= raw_text.len()
                } else {
                    range.end < raw_text.len()
                };
                if depth == 0 && permitted {
                    max_end = max_end.max(range.end);
                }
                // Mermaid fence coherence: register only truly-closed fences
                // whose End range is before EOF (or permitted at finalize).
                if matches!(tag_end, TagEnd::CodeBlock) {
                    if let Some(start) = open_fence_start.take() {
                        let slice_trimmed = raw_text[..range.end]
                            .trim_end_matches(['\n', '\r', ' ', '\t']);
                        let closed_by_fence = slice_trimmed.ends_with("```");
                        let fence_permitted = if permit_eof_closure {
                            range.end <= raw_text.len()
                        } else {
                            range.end < raw_text.len()
                        };
                        if closed_by_fence && fence_permitted {
                            discovered.push((start..range.end, std::mem::take(&mut fence_buf)));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    (max_end, discovered)
}

#[cfg(test)]
pub fn scan_authoritative_for_tests(
    raw_text: &str,
    mermaid_enabled: bool,
    permit_eof_closure: bool,
) -> (usize, Vec<(std::ops::Range<usize>, String)>) {
    scan_authoritative(raw_text, mermaid_enabled, permit_eof_closure)
}
```

- [ ] **Step 4: Run tests, expect all pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests scan_authoritative --features markdown`
Expected: 7 passed.

Also re-run the grounding suite to confirm parity:

Run: `cargo test -p spur-tui --test pulldown_cmark_grounding --features markdown`
Expected: 18 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): scan_authoritative with depth gating + fence coherence

Pure free function that returns (max_authoritative_end, closed_fences)
in a single pulldown pass. Depth gating excludes nested End events
(e.g., End(Item) inside an open List) whose commit would violate I5
(monotonic commitment) due to container-level tight/loose resolution.

Not yet wired into rebuild(); integration in the next task.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 1, 3)
Grounded by: crates/spur-tui/tests/pulldown_cmark_grounding.rs"
```

---

## Phase 3 — Two-Stage `rebuild`

### Task 6: Extract `build_items_for(prefix)` from current `rebuild()`

**Context.** The current `rebuild()` at `markdown_stream.rs:304-460` interleaves (a) pulldown scan for mermaid fences, (b) fence id assignment, (c) transform with sentinels, (d) tui_markdown parse, (e) split by sentinels. Stages (a) is replaced by `scan_authoritative`; stages (b)-(e) become `build_items_for`. This task does the extraction without changing behavior.

- [ ] **Step 1: Read the existing rebuild() carefully**

Read `crates/spur-tui/src/components/markdown_stream.rs:304-460` and note:
- Stage 1 (lines ~308-341): fence discovery loop
- Stage 2 (lines ~343-367): known_fences matching, id assignment
- Stage 3 (lines ~369-386): transformed input construction
- Stage 3.5 (line ~393): `inject_hard_breaks_in_tables`
- Stage 4 (lines ~395-413): tui_markdown parse
- Stage 5 (lines ~415-457): split into StreamItems

Stage 1 is now provided by `scan_authoritative` (returns `discovered_fences`). Stages 2-5 become `build_items_for`.

- [ ] **Step 2: Write a shim that will break the build intentionally**

Replace the existing `rebuild()` (keeping the old logic temporarily) by introducing `build_items_for` as a new method that takes the discovered fences from stage 0 as input. This task does NOT yet change behavior; it only splits the function.

In `markdown_stream.rs`, add a new method inside `impl MarkdownStream`:

```rust
    /// Stages 2-5 of rebuild: given a prefix of raw_text and the closed
    /// mermaid fences discovered within that prefix, produce cached_items,
    /// fence_placeholders, and the list of NEW fences (not previously
    /// known). Caller is responsible for passing a consistent (prefix,
    /// discovered_fences) pair — `scan_authoritative(&self.raw_text[..X])`
    /// semantics, with X = prefix.len().
    fn build_items_for(
        &mut self,
        prefix: &str,
        discovered_fences: Vec<(std::ops::Range<usize>, String)>,
        states: &StateLookup<'_>,
    ) -> (
        Vec<StreamItem>,
        std::collections::HashMap<MermaidId, Line<'static>>,
        Vec<FenceRef>,
        Vec<FenceRef>, // refreshed (to be stored as self.known_fences)
    ) {
        let mut new_fences: Vec<FenceRef> = Vec::new();
        let mut refreshed: Vec<FenceRef> = Vec::with_capacity(discovered_fences.len());
        for (range, code) in discovered_fences {
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

        // Build transformed input from the prefix + sentinels.
        let transformed = {
            let mut out = String::with_capacity(prefix.len());
            let mut cursor = 0usize;
            for f in &refreshed {
                if f.byte_range.start > cursor {
                    out.push_str(&prefix[cursor..f.byte_range.start]);
                }
                out.push_str(&format!("\n\u{0000}MERMAID:{}\u{0000}\n", f.id.0));
                cursor = f.byte_range.end;
            }
            if cursor < prefix.len() {
                out.push_str(&prefix[cursor..]);
            }
            out
        };

        let transformed = inject_hard_breaks_in_tables(&transformed);

        if transformed.is_empty() {
            return (Vec::new(), std::collections::HashMap::new(), new_fences, refreshed);
        }

        let text = tui_markdown::from_str(&transformed);
        let parsed_lines: Vec<ratatui::text::Line<'static>> = text
            .lines
            .into_iter()
            .map(|line| {
                let spans: Vec<ratatui::text::Span<'static>> =
                    line.spans.into_iter().map(convert_span).collect();
                let mut out = ratatui::text::Line::from(spans);
                out.style = convert_style(line.style);
                out.alignment = line.alignment.map(convert_alignment);
                out
            })
            .collect();

        let mut items: Vec<StreamItem> = Vec::new();
        let mut current_text: Vec<ratatui::text::Line<'static>> = Vec::new();
        let mut placeholders: std::collections::HashMap<MermaidId, Line<'static>> =
            std::collections::HashMap::new();

        for line in parsed_lines {
            let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let trimmed = raw.trim();
            if let Some(rest) = trimmed
                .strip_prefix('\u{0000}')
                .and_then(|s| s.strip_suffix('\u{0000}'))
                .and_then(|s| s.strip_prefix("MERMAID:"))
            {
                if !current_text.is_empty() {
                    items.push(StreamItem::Text(std::mem::take(&mut current_text)));
                }
                let id_num: u64 = rest.parse().unwrap_or(0);
                let id = MermaidId(id_num);

                use super::mermaid::FenceRender;
                let render = if states.is_err(id) {
                    FenceRender::Error
                } else if states.is_pending(id) {
                    FenceRender::Pending
                } else {
                    FenceRender::Ready(1)
                };
                placeholders.insert(id, super::mermaid::fence_placeholder_line(id, render));
                items.push(StreamItem::Fence(id));
            } else {
                current_text.push(line);
            }
        }
        if !current_text.is_empty() {
            items.push(StreamItem::Text(current_text));
        }

        (items, placeholders, new_fences, refreshed)
    }
```

- [ ] **Step 3: Verify build still works (no behavior change yet)**

Run: `cargo build -p spur-tui --features markdown`
Expected: clean compile, no behavior change since `build_items_for` is not yet called.

- [ ] **Step 4: Run existing tests to confirm no regression**

Run: `cargo test -p spur-tui --features markdown`
Expected: all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs
git commit -m "refactor(spur-tui): extract build_items_for from rebuild (no-op)

Defines build_items_for as Stages 2-5 of the existing rebuild logic
(fence id assignment, transform, tui_markdown parse, sentinel split).
Not yet invoked; integration follows.

Existing tests pass unchanged.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 3)"
```

### Task 7: Refactor `rebuild()` to two-stage with `permit_eof_closure` parameter

- [ ] **Step 1: Write failing tests**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn rebuild_advances_cursor_past_authoritative_events() {
    let mut s = MarkdownStream::new();
    s.append("# Title\n\nBody paragraph\n\nMore");
    s.flush_now(&StateLookup::empty());
    let flushed = s.flushed_byte_len_for_tests();
    assert!(flushed > 0 && flushed < s.raw_text().len(),
        "flushed={} raw_len={}", flushed, s.raw_text().len());
}

#[test]
fn rebuild_does_not_advance_past_open_list() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n");
    s.flush_now(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), 0,
        "open list at EOF must not advance cursor");
}

#[test]
fn flushed_byte_len_is_monotonic() {
    let mut s = MarkdownStream::new();
    s.append("# A\n\n");
    s.flush_now(&StateLookup::empty());
    let a = s.flushed_byte_len_for_tests();
    s.append("# B\n\n");
    s.flush_now(&StateLookup::empty());
    let b = s.flushed_byte_len_for_tests();
    assert!(b >= a, "monotonic: {} -> {}", a, b);
}
```

- [ ] **Step 2: Run tests, expect fail or inconsistent behavior**

Run: `cargo test -p spur-tui --test markdown_stream_tests rebuild_ flushed_byte_len_is_monotonic --features markdown`
Expected: these new tests fail (flushed_byte_len stays at 0 because rebuild doesn't yet update it).

- [ ] **Step 3: Replace `rebuild()` body with the two-stage implementation**

In `markdown_stream.rs`, replace the existing `rebuild` method (lines ~304-460) with:

```rust
    /// Rebuild `cached_items` from raw_text. Two stages:
    /// Stage 0: pulldown scan for offsets + mermaid fences.
    /// Stage 1: tui_markdown parse of raw_text[..authoritative_end].
    ///
    /// `permit_eof_closure = true` relaxes the cursor rule to allow events
    /// at EOF; used by `flush_final` on TurnComplete.
    ///
    /// Panic safety: mutations to cached_items / fence_placeholders /
    /// known_fences happen before flushed_byte_len is assigned. If any
    /// stage panics, flushed_byte_len retains its prior value; the next
    /// successful rebuild restores consistency (C1).
    fn rebuild(
        &mut self,
        states: &StateLookup<'_>,
        permit_eof_closure: bool,
    ) -> Vec<FenceRef> {
        // Stage 0: pulldown scan.
        let (new_flushed, discovered_fences) =
            scan_authoritative(&self.raw_text, self.mermaid_enabled, permit_eof_closure);

        // Stage 1: build items for the committed prefix.
        let prefix = &self.raw_text[..new_flushed];
        let prefix_owned = prefix.to_owned();
        let (items, placeholders, new_fences, refreshed) =
            self.build_items_for(&prefix_owned, discovered_fences, states);

        // Stage 2: commit. flushed_byte_len is assigned LAST (panic discipline).
        self.cached_items = items;
        self.fence_placeholders = placeholders;
        self.known_fences = refreshed;
        self.flushed_byte_len = new_flushed;

        new_fences
    }
```

Then update `flush_now` and `maybe_flush` to pass `permit_eof_closure = false`:

```rust
    /// Force a flush immediately.
    pub fn flush_now(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        self.dirty_since = None;
        self.rebuild(states, /* permit_eof_closure */ false)
    }
```

And inside `maybe_flush`, change `self.flush_now(states)` call to pass through correctly (signature unchanged):

```rust
    pub fn maybe_flush(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        match self.dirty_since {
            Some(t) if t.elapsed() >= DEBOUNCE => self.flush_now(states),
            _ => Vec::new(),
        }
    }
```

(`maybe_flush` will be further refactored in Phase 4.)

- [ ] **Step 4: Run the new tests + existing tests**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass, including the three new tests from Step 1.

If any existing test fails, inspect the assertion. The likely source of a break is a test that implicitly assumed `cached_items` covered all of `raw_text`; under the new contract, cached_items covers only the committed prefix. Such tests should be updated to append content with a trailing `\n\n` or explicit block boundary, so the cursor advances past all the content being asserted.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): two-stage rebuild with authoritative cursor advance

rebuild() now runs scan_authoritative (pulldown scan) + build_items_for
(tui_markdown on prefix). flushed_byte_len tracks the committed boundary;
items outside [0, flushed_byte_len) are NOT in cached_items — they live
in the tail returned by items_and_tail().

This closes the contract gap where cached_items previously over-described
raw_text. Renderers will consume items_and_tail in Phase 5.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 3)"
```

### Task 8: Add `flush_final` method

- [ ] **Step 1: Write failing tests**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn flush_final_commits_trailing_paragraph() {
    let mut s = MarkdownStream::new();
    s.append("# Title\n\nFinal paragraph");
    s.flush_final(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), s.raw_text().len(),
        "flush_final must commit all bytes including EOF");
    assert!(s.is_finalized());
}

#[test]
fn flush_final_commits_trailing_fence() {
    let mut s = MarkdownStream::new();
    s.append("Intro\n\n```rust\nfn x() {}\n```");
    s.flush_final(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), s.raw_text().len());
}
```

- [ ] **Step 2: Run tests, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests flush_final --features markdown`
Expected: compile error "no method `flush_final`".

- [ ] **Step 3: Implement `flush_final`**

In `markdown_stream.rs` inside `impl MarkdownStream`, add:

```rust
    /// TurnComplete flush. Permits cursor advance past events at EOF
    /// (`range.end == raw_text.len()`) since no more bytes will arrive.
    /// Sets `finalized = true`; subsequent `append` is a contract
    /// violation (debug_assert'd, self-heals in release).
    pub fn flush_final(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        self.dirty_since = None;
        let out = self.rebuild(states, /* permit_eof_closure */ true);
        self.finalized = true;
        out
    }
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests flush_final --features markdown`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): flush_final with EOF-permissive cursor advance

Used by TurnComplete via force_flush_all (wired in a later task). Relaxes
range.end < len to range.end <= len so trailing content (final paragraph,
fence closing at EOF) becomes part of the committed prefix with proper
markdown styling instead of rendering as the plain white tail.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5.1)"
```

---

## Phase 4 — Multi-Condition Flush Policy

### Task 9: Add `SAFETY_CAP_BYTES` constant and `has_authoritative_closure_pattern` helper

- [ ] **Step 1: Write failing tests**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn heuristic_fires_on_double_newline_with_content() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(has_authoritative_closure_pattern_for_tests("para\n\nmore"));
}

#[test]
fn heuristic_declines_double_newline_at_eof() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(!has_authoritative_closure_pattern_for_tests("para\n\n"));
}

#[test]
fn heuristic_fires_on_fence_close_with_content() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(has_authoritative_closure_pattern_for_tests("```\ncode\n```\nmore"));
}

#[test]
fn heuristic_declines_fence_close_at_eof() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(!has_authoritative_closure_pattern_for_tests("```\ncode\n```\n"));
}

#[test]
fn safety_cap_is_64kib() {
    use spur_tui::components::markdown_stream::SAFETY_CAP_BYTES;
    assert_eq!(SAFETY_CAP_BYTES, 64 * 1024);
}
```

- [ ] **Step 2: Run tests, expect compile fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests heuristic safety_cap --features markdown`
Expected: compile error.

- [ ] **Step 3: Implement the constant and helper**

In `markdown_stream.rs`, near the existing `DEBOUNCE` constant, add:

```rust
pub const SAFETY_CAP_BYTES: usize = 64 * 1024;
```

Add free function below `scan_authoritative`:

```rust
/// Cheap stateless scan for patterns that typically indicate an
/// authoritative block close. False positives allowed (wasted rebuild);
/// false negatives bounded by DEBOUNCE.
pub(crate) fn has_authoritative_closure_pattern(tail: &str) -> bool {
    // (a) Paragraph / block close: `\n\n` with content after the last
    //     occurrence. Content-after required so we don't waste a rebuild
    //     on a tail whose trailing `\n\n` is at EOF (where pulldown
    //     emits End at range.end == len — non-authoritative).
    if let Some(idx) = tail.rfind("\n\n") {
        if idx + 2 < tail.len() {
            return true;
        }
    }
    // (b) Fence close on its own line with content after.
    if let Some(idx) = tail.find("\n```") {
        let after = idx + 4;
        if tail.as_bytes().get(after) == Some(&b'\n') && after + 1 < tail.len() {
            return true;
        }
    }
    false
}

#[cfg(test)]
pub fn has_authoritative_closure_pattern_for_tests(tail: &str) -> bool {
    has_authoritative_closure_pattern(tail)
}
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests heuristic safety_cap --features markdown`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): SAFETY_CAP_BYTES + has_authoritative_closure_pattern

Stateless heuristic used by maybe_flush (next task) to detect likely
block closures without running the full parse. Checks for \\n\\n with
content after OR fence-close line with content after. Both patterns
require content-after to avoid false-positive flushes at EOF.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 4)"
```

### Task 10: Refactor `maybe_flush` with multi-condition logic

- [ ] **Step 1: Write failing regression tests**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
fn maybe_flush_short_circuits_when_not_dirty() {
    let mut s = MarkdownStream::new();
    s.append("# A\n\nbody");
    s.flush_now(&StateLookup::empty());
    assert!(!s.is_dirty(), "flush_now clears dirty_since");
    // maybe_flush on a clean stream must return empty without work.
    let out = s.maybe_flush(&StateLookup::empty());
    assert!(out.is_empty());
}

#[test]
fn maybe_flush_fast_path_fires_on_boundary_pattern() {
    let mut s = MarkdownStream::new();
    // Append content that contains \n\n with trailing content — heuristic
    // fires before DEBOUNCE elapses.
    s.append("paragraph\n\nmore content");
    let before = s.flushed_byte_len_for_tests();
    s.maybe_flush(&StateLookup::empty());
    let after = s.flushed_byte_len_for_tests();
    assert!(after > before,
        "fast path should have flushed immediately; before={} after={}",
        before, after);
}

#[test]
fn maybe_flush_declines_when_no_boundary_before_debounce() {
    let mut s = MarkdownStream::new();
    s.append("streaming without boundaries");
    // Immediately after append, dirty but no boundary, debounce not
    // elapsed → no flush.
    s.maybe_flush(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), 0);
    // Stream still dirty.
    assert!(s.is_dirty());
}

#[test]
fn maybe_flush_safety_cap_suppresses_rebuild() {
    use spur_tui::components::markdown_stream::SAFETY_CAP_BYTES;
    let mut s = MarkdownStream::new();
    // A long boundary-free tail.
    let huge = "x".repeat(SAFETY_CAP_BYTES + 100);
    s.append(&huge);
    let out = s.maybe_flush(&StateLookup::empty());
    assert!(out.is_empty());
    // Safety valve clears dirty_since so we don't re-enter on next tick.
    assert!(!s.is_dirty(),
        "safety valve must clear dirty_since to prevent tight looping");
}
```

- [ ] **Step 2: Run tests, expect at least one fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests maybe_flush --features markdown`
Expected: `maybe_flush_fast_path_fires_on_boundary_pattern` and `maybe_flush_safety_cap_suppresses_rebuild` fail.

- [ ] **Step 3: Replace `maybe_flush` body**

In `markdown_stream.rs`, replace the existing `maybe_flush` with:

```rust
    /// Flush if conditions warrant. Priority order:
    /// 1. Not dirty → no-op (load-bearing: prevents busy-looping when
    ///    cursor fails to advance under the heuristic).
    /// 2. Empty raw_text → no-op.
    /// 3. Tail > SAFETY_CAP_BYTES without boundary → suppress rebuild,
    ///    clear dirty_since, let plain-text tail render until TurnComplete.
    /// 4. Tail contains authoritative closure pattern → flush immediately.
    /// 5. DEBOUNCE elapsed → flush.
    /// 6. Otherwise → no-op.
    pub fn maybe_flush(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        let Some(dirty_at) = self.dirty_since else { return Vec::new(); };
        if self.raw_text.is_empty() { return Vec::new(); }

        let tail = &self.raw_text[self.flushed_byte_len..];
        let tail_len = tail.len();

        // Safety valve: large boundary-free tail.
        if tail_len > SAFETY_CAP_BYTES && !has_authoritative_closure_pattern(tail) {
            self.dirty_since = None;
            return Vec::new();
        }

        // Fast path: authoritative closure pattern present.
        if has_authoritative_closure_pattern(tail) {
            return self.flush_now(states);
        }

        // Debounce.
        if dirty_at.elapsed() >= DEBOUNCE {
            return self.flush_now(states);
        }

        Vec::new()
    }
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): multi-condition maybe_flush with dirty-guard

Priority: dirty-guard (load-bearing) → safety valve → fast path
(heuristic) → debounce. The dirty-guard prevents the busy-loop when
the heuristic matches but cursor cannot advance (e.g., open code fence
with blank line inside).

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 4)"
```

### Task 11: Add `debug_assert` guard to `append()` for post-finalize contract

- [ ] **Step 1: Write failing test (debug-only)**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "append after flush_final")]
fn append_after_flush_final_debug_asserts() {
    let mut s = MarkdownStream::new();
    s.append("hello");
    s.flush_final(&StateLookup::empty());
    // Contract violation:
    s.append("more");
}

#[test]
fn append_before_flush_final_never_panics() {
    let mut s = MarkdownStream::new();
    s.append("hello");
    s.append(" world");
    assert_eq!(s.raw_text(), "hello world");
}
```

- [ ] **Step 2: Run tests, expect debug test to fail**

Run: `cargo test -p spur-tui --test markdown_stream_tests append_ --features markdown`
Expected: `append_after_flush_final_debug_asserts` fails (no panic yet).

- [ ] **Step 3: Add the debug_assert**

In `markdown_stream.rs`, modify `append`:

```rust
    /// Append a chunk of text. Cheap — does not reparse.
    ///
    /// Contract: callers must not append after `flush_final`. Enforced via
    /// `debug_assert!` in debug builds; in release the state self-heals
    /// (next rebuild runs under normal cursor rule).
    pub fn append(&mut self, text: &str) {
        debug_assert!(
            !self.finalized,
            "append after flush_final is a contract violation (MarkdownStream finalized)"
        );
        self.raw_text.push_str(text);
        self.dirty_since.get_or_insert_with(Instant::now);
    }
```

- [ ] **Step 4: Run tests, expect pass**

Run: `cargo test -p spur-tui --test markdown_stream_tests append_ --features markdown`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "feat(spur-tui): debug_assert append after flush_final

Contract guard: post-TurnComplete appends are a caller bug. Debug builds
panic with a clear message; release self-heals via two-stage rebuild
statelessness (no corruption, just one wasted rebuild per buggy append).

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5.1, 6)"
```

---

## Phase 5 — Render-Site Migration

### Task 12: Add shared `render_agent_message_body` helper in `builder.rs`

**Context.** The helper takes two closure sinks so both render paths (primary: VirtualRow with ImageRow support; secondary: flat Line list) traverse the same items + tail logic.

- [ ] **Step 1: Write a test-shaped contract**

The helper's contract is tested indirectly via the render-path migration in Task 13. For now, add the function without a direct unit test; the migration tasks cover it end-to-end.

- [ ] **Step 2: Define `FenceRenderDecision` is NOT needed** — the helper uses a `fence_state` map directly.

- [ ] **Step 3: Add helper at top of `builder.rs`**

In `crates/spur-tui/src/components/react_trace/builder.rs`, after the imports (before `impl ReactTrace`), add:

```rust
#[cfg(feature = "markdown")]
use crate::components::markdown_stream::{MarkdownStream, StreamItem};

#[cfg(feature = "markdown")]
use crate::components::mermaid::{FenceRender, MermaidId, fence_placeholder_line};

/// Render an AgentMessage body via the cursor-split contract.
///
/// Emits:
/// 1. Committed items from `stream.items_and_tail().0` — styled text and
///    fence rows (image via `emit_fence_image`, placeholder via `emit_line`).
/// 2. The uncommitted tail from `stream.items_and_tail().1` — plain white
///    lines with the 3-space indent.
///
/// The two-closure split lets the primary render path emit multiple
/// `VirtualRow::ImageRow` entries per mermaid fence while the secondary
/// path renders a single placeholder line (no ImageRow concept).
#[cfg(feature = "markdown")]
fn render_agent_message_body(
    stream: &MarkdownStream,
    fence_state: &std::collections::HashMap<MermaidId, FenceRender>,
    mut emit_line: impl FnMut(ratatui::text::Line<'static>),
    mut emit_fence_image: impl FnMut(MermaidId, u16),
) {
    use ratatui::{
        style::{Color, Style},
        text::{Line, Span},
    };

    let (items, tail) = stream.items_and_tail();

    for item in items {
        match item {
            StreamItem::Text(text_lines) => {
                for line in text_lines {
                    let mut spans = vec![Span::raw("   ")];
                    spans.extend(line.spans.iter().cloned());
                    let mut new_line = Line::from(spans);
                    new_line.style = line.style;
                    new_line.alignment = line.alignment;
                    emit_line(new_line);
                }
            }
            StreamItem::Fence(id) => match fence_state.get(id).copied() {
                Some(FenceRender::Ready(h)) if h > 0 => {
                    emit_fence_image(*id, h);
                }
                other => {
                    let render = match other {
                        Some(FenceRender::Error) => FenceRender::Error,
                        _ => FenceRender::Pending,
                    };
                    let placeholder = fence_placeholder_line(*id, render);
                    let mut spans = vec![Span::raw("   ")];
                    spans.extend(placeholder.spans.iter().cloned());
                    let mut line = Line::from(spans);
                    line.style = placeholder.style;
                    line.alignment = placeholder.alignment;
                    emit_line(line);
                }
            },
        }
    }

    // Plain-text tail, indented, white.
    for text_line in tail.lines() {
        emit_line(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                text_line.to_string(),
                Style::default().fg(Color::White),
            ),
        ]));
    }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p spur-tui --features markdown`
Expected: clean compile (helper not yet called).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs
git commit -m "feat(spur-tui): render_agent_message_body shared helper

Emits committed items + plain tail via two caller-provided closures
(emit_line, emit_fence_image). Unused until call-site migration in the
next two tasks.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5)"
```

### Task 13: Migrate `build_virtual_rows` AgentMessage branch

- [ ] **Step 1: Write RC1 regression test**

Create `crates/spur-tui/src/components/react_trace/streaming_tests.rs`:

```rust
//! End-to-end streaming tests. Covers the original ghost-text RCA case
//! and related scenarios.

#![cfg(feature = "markdown")]

use super::ReactTrace;
use crate::components::markdown_stream::StateLookup;

/// TI.2 — the original ghost-text failing case.
#[test]
fn ghost_text_rc1_regression() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("First chunk. ", "claude", "10:00:00".to_string());
    // Force a flush to populate cached_items.
    trace.force_flush_all(&StateLookup::empty());
    // Now append a second chunk; BEFORE any further flush, the tail must
    // be visible in rendered output.
    trace.append_message("Second chunk.", "claude", "10:00:00".to_string());

    let (rows, _) = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let rendered: String = rows.iter().filter_map(|r| match r {
        crate::components::react_trace::VirtualRow::Text(line) => Some(
            line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
        ),
        _ => None,
    }).collect::<Vec<_>>().join("\n");

    assert!(
        rendered.contains("Second chunk"),
        "ghost-text regression: second chunk must appear in rendered rows before the next flush.\nRendered:\n{}",
        rendered
    );
}
```

Also add test-only helpers to `react_trace/mod.rs`. In `mod.rs`, inside `impl ReactTrace`, add:

```rust
    #[cfg(test)]
    pub fn new_for_tests() -> Self {
        Self::new()
    }

    #[cfg(test)]
    pub fn build_virtual_rows_for_tests(
        &self,
        from: usize,
        width: u16,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> (Vec<super::VirtualRow>, Vec<usize>) {
        self.build_virtual_rows(from, width, states, lineage)
    }
```

Ensure the new test file is discovered by adding `mod streaming_tests;` inside `mod.rs` under a `#[cfg(test)]` block:

```rust
#[cfg(test)]
mod streaming_tests;
```

- [ ] **Step 2: Run test, expect fail (tail not yet rendered)**

Run: `cargo test -p spur-tui ghost_text_rc1_regression --features markdown`
Expected: FAIL — old behavior renders `cached_items` only, not the tail.

- [ ] **Step 3: Migrate `build_virtual_rows` AgentMessage branch**

In `builder.rs`, locate the `TraceKind::AgentMessage { agent }` branch inside `build_virtual_rows` (around lines 490-576). Replace the entire branch body (from the header `push_wrapped` through the `items_rendered = …` closure and the `if !items_rendered` fallback block) with:

```rust
                TraceKind::AgentMessage { agent } => {
                    push_wrapped(
                        &mut rows,
                        Line::from(vec![
                            ts_span.clone(),
                            Span::styled(
                                format!("✉ {}", agent),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                    );

                    #[cfg(feature = "markdown")]
                    {
                        if let Some(stream) = entry.markdown.as_ref() {
                            render_agent_message_body(
                                stream,
                                states,
                                |line| push_wrapped(&mut rows, line),
                                |id, h| {
                                    for r in 0..h {
                                        rows.push(VirtualRow::ImageRow {
                                            id,
                                            row_within: r,
                                            total_rows: h,
                                        });
                                    }
                                },
                            );
                        } else {
                            for text_line in entry.text.lines() {
                                push_wrapped(
                                    &mut rows,
                                    Line::from(vec![
                                        Span::raw("   "),
                                        Span::styled(
                                            text_line.to_string(),
                                            Style::default().fg(Color::White),
                                        ),
                                    ]),
                                );
                            }
                        }
                    }

                    #[cfg(not(feature = "markdown"))]
                    for text_line in entry.text.lines() {
                        push_wrapped(
                            &mut rows,
                            Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
                                    Style::default().fg(Color::White),
                                ),
                            ]),
                        );
                    }
                }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass, including `ghost_text_rc1_regression`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): build_virtual_rows uses items_and_tail

Primary render path now calls render_agent_message_body, emitting
committed items (styled) + uncommitted tail (plain white). Removes
the items_rendered fallback gymnastics. Closes the ghost-text RC1.

Adds streaming_tests.rs with the RC1 regression.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5)"
```

### Task 14: Migrate `build_display_lines` AgentMessage branch

- [ ] **Step 1: Write verification test**

Append to `streaming_tests.rs`:

```rust
/// T4.3 — both render paths produce the same textual content.
#[test]
fn both_render_paths_produce_identical_textual_content() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("# Title\n\nBody text. ", "claude", "10:00".to_string());
    trace.force_flush_all(&StateLookup::empty());
    trace.append_message("tail bytes", "claude", "10:00".to_string());

    let flat = trace.build_display_lines_for_tests("", None);
    let flat_text: String = flat.iter().map(|l| {
        l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
    }).collect::<Vec<_>>().join("\n");

    let (rows, _) = trace.build_virtual_rows_for_tests(0, 200, &std::collections::HashMap::new(), None);
    let virt_text: String = rows.iter().filter_map(|r| match r {
        crate::components::react_trace::VirtualRow::Text(line) => Some(
            line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
        ),
        _ => None,
    }).collect::<Vec<_>>().join("\n");

    // Allow minor whitespace differences from wrapping; require every
    // substantive content fragment to appear in both.
    for needle in ["Title", "Body text", "tail bytes"] {
        assert!(flat_text.contains(needle), "flat missing {:?}: {}", needle, flat_text);
        assert!(virt_text.contains(needle), "virt missing {:?}: {}", needle, virt_text);
    }
}
```

Add to `react_trace/mod.rs` test helpers:

```rust
    #[cfg(test)]
    pub fn build_display_lines_for_tests(
        &self,
        spinner_frame: &str,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) -> Vec<ratatui::text::Line<'static>> {
        self.build_display_lines(spinner_frame, lineage)
    }
```

- [ ] **Step 2: Run test, expect fail**

Run: `cargo test -p spur-tui both_render_paths_produce_identical --features markdown`
Expected: fail — secondary path still uses `stream.lines()` which excludes the tail.

- [ ] **Step 3: Migrate `build_display_lines` AgentMessage branch**

In `builder.rs`, locate the `TraceKind::AgentMessage { agent }` branch inside `build_display_lines` (around lines 103-155). Replace the entire branch body with:

```rust
                TraceKind::AgentMessage { agent } => {
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
                    {
                        if let Some(stream) = entry.markdown.as_ref() {
                            let empty_state: std::collections::HashMap<
                                crate::components::mermaid::MermaidId,
                                crate::components::mermaid::FenceRender,
                            > = std::collections::HashMap::new();
                            render_agent_message_body(
                                stream,
                                &empty_state,
                                |line| lines.push(line),
                                |_id, _h| {
                                    // Secondary path passes empty fence_state,
                                    // so this closure is never invoked. The
                                    // placeholder branch inside the helper
                                    // fires instead.
                                    unreachable!("secondary path uses empty fence_state")
                                },
                            );
                        } else {
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

                    #[cfg(not(feature = "markdown"))]
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass, including `both_render_paths_produce_identical_textual_content`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): build_display_lines uses items_and_tail

Secondary render path now traverses the same helper as the primary,
eliminating the drift risk that originally allowed RC1 to ship. Uses
an empty fence_state map; the helper's placeholder branch fires for
any fences.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5)"
```

### Task 15: Wire `force_flush_all` to use `flush_final`

- [ ] **Step 1: Write a test that exercises force_flush_all on a finalize-worthy input**

Append to `streaming_tests.rs`:

```rust
/// T5.2 — trailing fenced code block renders with markdown styling on
/// TurnComplete (force_flush_all), not as plain tail.
#[test]
fn turn_complete_final_code_fence_renders_styled() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "Here's the code:\n\n```rust\nfn main() {}\n```",
        "claude",
        "10:00".to_string(),
    );
    trace.force_flush_all(&StateLookup::empty());

    // After flush_final, the stream's tail should be empty — everything
    // committed.
    let stream = trace
        .entries_for_tests()
        .iter()
        .find_map(|e| e.markdown.as_ref())
        .expect("agent message entry has a markdown stream");
    let (_, tail) = stream.items_and_tail();
    assert_eq!(tail, "", "TurnComplete must commit all raw_text; tail={:?}", tail);
    assert!(stream.is_finalized());
}
```

Add to `react_trace/mod.rs` test helpers:

```rust
    #[cfg(test)]
    pub fn entries_for_tests(&self) -> &[crate::components::react_trace::types::TraceEntry] {
        &self.entries
    }
```

- [ ] **Step 2: Run test, expect fail**

Run: `cargo test -p spur-tui turn_complete_final_code_fence_renders_styled --features markdown`
Expected: fail — current `force_flush_all` uses `flush_now`, which leaves fence-at-EOF in the tail.

- [ ] **Step 3: Change `force_flush_all` to call `flush_final`**

In `crates/spur-tui/src/components/react_trace/mod.rs`, locate `force_flush_all` (around line 447-461). Replace the call to `stream.flush_now(states)` with `stream.flush_final(states)`:

```rust
    /// Force an immediate rebuild of every markdown stream.
    #[cfg(feature = "markdown")]
    pub fn force_flush_all(
        &mut self,
        states: &super::markdown_stream::StateLookup<'_>,
    ) -> Vec<(usize, super::markdown_stream::FenceRef)> {
        let mut out = Vec::new();
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if let Some(stream) = entry.markdown.as_mut() {
                for fence in stream.flush_final(states) {
                    out.push((idx, fence));
                }
            }
        }
        self.invalidate_cache();
        out
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): force_flush_all uses flush_final (TurnComplete)

On TurnComplete, switching to flush_final lets the cursor advance past
EOF-authoritative events, so trailing paragraphs / closing fences render
with markdown styling instead of as plain white tail.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5.1)"
```

---

## Phase 6 — Remaining Integration Tests

### Task 16: Add TI.3 busy-loop regression

- [ ] **Step 1: Write test + instrumentation**

Append to `streaming_tests.rs`:

```rust
/// TI.3 — open code fence containing `\n\n` must not busy-loop.
///
/// Under the stateless heuristic, this tail matches \n\n+content but
/// pulldown can't advance the cursor (fence still open). The
/// dirty_since guard must make the SECOND maybe_flush short-circuit.
#[test]
fn code_block_with_blank_line_does_not_cause_cpu_spike() {
    use crate::components::markdown_stream::{MarkdownStream, StateLookup};

    let mut s = MarkdownStream::new();
    s.append("```rust\nfn main() {\n\n    body\n\n}\n");

    let count_before = s.rebuild_count_for_tests();
    s.maybe_flush(&StateLookup::empty());
    let count_after_first = s.rebuild_count_for_tests();
    s.maybe_flush(&StateLookup::empty());
    s.maybe_flush(&StateLookup::empty());
    s.maybe_flush(&StateLookup::empty());
    let count_after_three_more = s.rebuild_count_for_tests();

    assert_eq!(count_after_first - count_before, 1,
        "first maybe_flush triggers one rebuild (fast-path fires)");
    assert_eq!(count_after_three_more - count_after_first, 0,
        "subsequent maybe_flushes must short-circuit on dirty_since=None; \
         got {} extra rebuilds", count_after_three_more - count_after_first);
}
```

Add `rebuild_count_for_tests()` instrumentation in `markdown_stream.rs`:

In the struct, add a `#[cfg(test)]` field:

```rust
    #[cfg(test)]
    rebuild_count: std::cell::Cell<u64>,
```

Update `Default` impl:

```rust
    #[cfg(test)]
    rebuild_count: std::cell::Cell::new(0),
```

Add a test-only accessor:

```rust
    #[cfg(test)]
    pub fn rebuild_count_for_tests(&self) -> u64 {
        self.rebuild_count.get()
    }
```

Inside `rebuild()`, increment the counter at entry:

```rust
    fn rebuild(
        &mut self,
        states: &StateLookup<'_>,
        permit_eof_closure: bool,
    ) -> Vec<FenceRef> {
        #[cfg(test)]
        self.rebuild_count.set(self.rebuild_count.get() + 1);
        // ... rest of rebuild body
    }
```

- [ ] **Step 2: Run test, expect fail on dirty-guard absence if it regressed**

Run: `cargo test -p spur-tui code_block_with_blank_line_does_not_cause_cpu_spike --features markdown`
Expected: pass (the dirty-guard was added in Task 10).

- [ ] **Step 3: Verify rebuild_count increments correctly**

The test above also verifies this. If the assertion "first maybe_flush triggers one rebuild" fails with count 0, the counter instrumentation is wrong; re-check the `rebuild()` entry increment.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "test(spur-tui): TI.3 busy-loop regression gate

Adds rebuild_count instrumentation (test-only) and asserts that an
open fence with \\n\\n inside produces exactly one rebuild, not a
per-frame flood. Load-bearing regression against the Section 4
busy-loop failure mode.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 4, 6)"
```

### Task 17: Add remaining edge-case tests

- [ ] **Step 1: Write the tests**

Append to `tests/markdown_stream_tests.rs`:

```rust
#[test]
fn setext_promotion_retroactively_restyles_at_boundary() {
    let mut s = MarkdownStream::new();
    s.append("Hello\n");
    s.flush_now(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), 0);

    s.append("===\n\nbody");
    s.flush_now(&StateLookup::empty());
    assert!(s.flushed_byte_len_for_tests() > 0,
        "setext + trailing content should advance cursor");

    let rendered = s.cached_lines_debug().join("\n");
    assert!(rendered.to_lowercase().contains("hello"),
        "committed prefix should contain the heading text");
}

#[test]
fn list_in_progress_renders_markers_as_plain_tail() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n");
    s.flush_now(&StateLookup::empty());
    let (items, tail) = s.items_and_tail();
    assert_eq!(items.len(), 0, "open list: nothing committed");
    assert_eq!(tail, "- item1\n- item2\n");
}

#[test]
fn list_promotes_on_close() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n\nafter");
    s.flush_now(&StateLookup::empty());
    let (items, _) = s.items_and_tail();
    assert!(!items.is_empty(), "closed list should produce committed items");
}

#[test]
fn unicode_content_cursor_advances_on_char_boundary() {
    let mut s = MarkdownStream::new();
    s.append("# 漢字 🎉\n\nmore content");
    s.flush_now(&StateLookup::empty());
    let flushed = s.flushed_byte_len_for_tests();
    assert!(s.raw_text().is_char_boundary(flushed),
        "flushed_byte_len {} must be on UTF-8 char boundary", flushed);
}

#[test]
fn no_fence_registered_with_range_end_at_eof() {
    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 0, "fence at EOF must not register");

    s.append("\n\ntrailing");
    let fences2 = s.flush_now(&StateLookup::empty());
    assert_eq!(fences2.len(), 1, "once trailing content arrives, fence registers");
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-tui --test markdown_stream_tests --features markdown`
Expected: all new tests pass.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass.

- [ ] **Step 4: Run the grounding suite one more time to confirm no drift**

Run: `cargo test -p spur-tui --test pulldown_cmark_grounding --features markdown`
Expected: 18 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "test(spur-tui): edge-case regressions for setext, lists, unicode, fences

Covers Section 5 edge cases: setext retroactive promotion, open-list
tail rendering, unicode char-boundary safety, and mermaid fence EOF
coherence.

Refs: docs/superpowers/specs/2026-04-18-streaming-cursor-split-renderer-design.md (Section 5)"
```

---

## Phase 7 — Build Verification

### Task 18: Non-markdown-feature build sanity check

- [ ] **Step 1: Run a feature-disabled build**

Run: `cargo build -p spur-tui --no-default-features`
Expected: clean compile. If it fails with errors about `MarkdownStream`, `items_and_tail`, or `render_agent_message_body`, the `#[cfg(feature = "markdown")]` gates in `builder.rs` need inspection — non-feature builds must fall back to the `entry.text` path.

- [ ] **Step 2: Run default-feature tests**

Run: `cargo test -p spur-tui`
Expected: all pass.

- [ ] **Step 3: Run full workspace tests to catch cross-crate regressions**

Run: `cargo test`
Expected: all pass. Any failure outside `spur-tui` indicates an unintended coupling; investigate before proceeding.

- [ ] **Step 4: No commit needed** — this task is verification only.

---

## Self-Review Summary

**Spec coverage.** Every in-scope item from the spec maps to a task:
- Section 1 (authoritative rule, depth gate) → Task 5 + grounding tests already committed.
- Section 2 (state, accessors, contract) → Tasks 1, 2, 3, 4.
- Section 3 (two-stage rebuild) → Tasks 6, 7, 8.
- Section 4 (flush policy) → Tasks 9, 10.
- Section 5 (render migration + flush_final + coherence) → Tasks 12, 13, 14, 15, 17.
- Section 6 edge cases → Tasks 11 (append guard), 17.
- Section 6 test suite → Tasks across phases + Task 16.
- Section 7 scope discipline (non-markdown build) → Task 18.

**Placeholder scan.** No `TBD`, `TODO`, or hand-wavy phrases. Every step includes exact code, commands, or line targets.

**Type consistency.** `MarkdownStream` accessor names used across tasks:
- `flushed_byte_len_for_tests()` — Tasks 1, 7, 8, 10, 11, 17.
- `is_finalized()` — Tasks 2, 8, 15.
- `items_and_tail()` — Tasks 3, 12, 15.
- `fence_placeholder_for()` — Task 4 (introduced); not called elsewhere in this plan, matches spec Section 2.
- `scan_authoritative()` + `_for_tests` wrapper — Task 5, consumed by Task 7.
- `has_authoritative_closure_pattern()` + `_for_tests` — Task 9, consumed by Task 10.
- `rebuild_count_for_tests()` — Task 16.
- `flush_final()` — Task 8, wired in Task 15.

`render_agent_message_body` closures `emit_line: FnMut(Line)`, `emit_fence_image: FnMut(MermaidId, u16)` — consistent across Tasks 12, 13, 14.

All types and method names line up.

---

Plan complete and saved to `docs/superpowers/plans/2026-04-18-streaming-cursor-split-renderer.md`.

Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
