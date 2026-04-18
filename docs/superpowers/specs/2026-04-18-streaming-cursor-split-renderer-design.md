# Streaming Cursor-Split Renderer — Design

**Date:** 2026-04-18
**Status:** Design, ready for implementation plan
**Scope:** `crates/spur-tui/src/components/markdown_stream.rs` and `crates/spur-tui/src/components/react_trace/builder.rs`
**Related:** `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md` (root-cause analysis this design addresses)

## Executive Summary

The Session Detail trace exhibits "ghost text" during sustained `AgentMessageChunk`
streaming: new chunks appear delayed or missing, then the reply jumps forward in
batches. Root cause: `MarkdownStream::cached_items` is populated only on debounced
flush (50 ms window), and `ReactTrace` renders from `cached_items` whenever it is
non-empty, so newly-appended bytes in `raw_text` are invisible until the next flush.

This design replaces the "all-or-nothing cache" model with a **cursor-split
renderer**: `cached_items` describes a committed prefix of `raw_text`; the
uncommitted tail is rendered every paint as plain text. The cursor advances
only past pulldown-cmark `Event::End` events whose interpretation is fixed —
identified by the condition `range.end < raw_text.len()`. This matches the
industry-consensus approach (Semidown, solid-streaming-markdown, Lezer's
incremental markdown parser, md2term's safe-boundary heuristic).

The design closes the ghost-text bug by construction, preserves visual
stability under partial markdown tokens, adds no architectural surface, and
reduces per-flush parse work vs. today.

## Invariants

The design establishes five invariants. All tests gate on these; all code
comments anchor to them.

- **I1 (correctness).** Every paint reflects current `raw_text`. The visible
  surface at paint time is a function of `raw_text_at_paint_time`, not of a
  snapshot taken at an arbitrary past flush.
- **I2 (latency).** Wall-clock lag between "byte appended to `raw_text`" and
  "byte influences next paint" ≤ one frame period.
- **I3 (budget).** Per-frame rendering work ≤ frame period minus other TUI
  work. In practice: ≤ a few milliseconds for markdown rendering.
- **I4 (visual stability).** A character, once rendered at position (x, y,
  style), does not toggle styles across consecutive frames unless the
  underlying source semantics changed (e.g., cursor advanced past the
  character's authoritative closure).
- **I5 (monotonic commitment).** The cursor advances only; it never retreats.
  Once bytes are in `cached_items`, they are not re-interpreted unless the
  stream is explicitly reset (currently: no such operation exists).

## Design

### 1. Authoritative-closure cursor model

**Rule for cursor advance.** During `rebuild()`, walk `raw_text` with
`pulldown_cmark::Parser::new_ext(…).into_offset_iter()`. For every
`Event::End(tag)` at any block-level tag, if `range.end < raw_text.len()`,
advance the cursor to `max(cursor, range.end)`.

**Why it works.** pulldown-cmark is not incremental; each call re-parses
`raw_text` from scratch. An event whose range ends strictly before EOF has
had its interpretation finalized against current `raw_text`. The parser is
deterministic and stream appends are append-only, so those earlier-offset
events remain stable under future appends. The only residual ambiguity is
EOF auto-close, which the `range.end < raw_text.len()` guard excludes.

**What the rule is not.** It is not a whitelist of "safe" tags. The set
`{CodeBlock, Heading, Rule, Table, List, Paragraph, Item, BlockQuote, …}`
is treated uniformly. pulldown-cmark's own disambiguation (e.g., emitting
`TagEnd::Heading` for setext-promoted paragraphs, or withholding
`TagEnd::List` when lazy continuation could extend an item) is authoritative.
The len-compare rule only excludes the single residual case pulldown cannot
resolve without more input: EOF.

**Known limitation.** Reference link definitions (`[id]: url`) arriving in
a later chunk than the `[id]` usage do not retroactively activate the link
in the committed prefix. Lezer's incremental markdown parser documents the
same limitation. LLM output rarely uses reference-style links; inline links
(`[text](url)`) are overwhelmingly preferred. Acknowledged; not addressed
in this patch.

### 2. State and accessor contract

**State additions to `MarkdownStream`:**

```rust
pub struct MarkdownStream {
    raw_text: String,
    dirty_since: Option<Instant>,
    cached_items: Vec<StreamItem>,
    fence_placeholders: HashMap<MermaidId, Line<'static>>,
    known_fences: Vec<FenceRef>,
    next_fence_id: u64,
    mermaid_enabled: bool,

    // NEW:
    flushed_byte_len: usize,   // cursor: cached_items covers raw_text[..flushed_byte_len]
    finalized: bool,           // set by flush_final; guards append() via debug_assert
}
```

**Invariant C1 (contract).** `cached_items`, `known_fences`, and
`fence_placeholders` jointly represent the parsed-and-decorated form of
`raw_text[..flushed_byte_len]`. None of these fields depend on bytes beyond
`flushed_byte_len`.

**Invariant C2.** `flushed_byte_len` is monotonically non-decreasing across
any sequence of `append` → `maybe_flush` / `flush_now` / `flush_final`
operations.

**Invariant C3.** `raw_text[flushed_byte_len..]` is the uncommitted tail;
renderers must present it on every paint.

**New accessor:**

```rust
impl MarkdownStream {
    pub fn items_and_tail(&self) -> (&[StreamItem], &str) {
        (&self.cached_items, &self.raw_text[self.flushed_byte_len..])
    }

    pub fn fence_placeholder_for(&self, id: MermaidId) -> Option<Line<'static>> {
        self.fence_placeholders.get(&id).cloned()
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized
    }
}
```

**Legacy accessors.** `items()`, `raw_text()`, `lines()`, `is_dirty()`,
`mark_dirty_now()`, `cached_lines_debug()` remain. `lines()` is not
deprecated in this patch (module-internal test callers would flag). No
external behavior change.

**Coherence fix for mermaid fence registration.** Inside `scan_authoritative`
(below), apply `range.end < raw_text.len()` as the condition for registering
a closed mermaid fence. Symmetric with cursor advance; preserves the
invariant "no registered fence spans the cursor."

### 3. Two-stage `rebuild()`

The existing `rebuild()` parses full `raw_text` via `tui_markdown::from_str`.
Under the new contract, `cached_items` must cover only
`raw_text[..flushed_byte_len]`. Naïvely parsing all of `raw_text` causes
`cached_items` to over-describe and the tail to double-render.

**Restructure.**

```rust
fn rebuild(&mut self, states: &StateLookup<'_>, permit_eof_closure: bool)
    -> Vec<FenceRef>
{
    // Stage 0: pulldown scan for offsets and mermaid fences.
    let (authoritative_end, discovered_fences) =
        scan_authoritative(&self.raw_text, self.mermaid_enabled, permit_eof_closure);

    let new_flushed = authoritative_end;

    // Stage 1: build items from the committed prefix.
    let prefix = &self.raw_text[..new_flushed];
    let (items, placeholders, new_fences) =
        build_items_for(prefix, &discovered_fences, states, &mut self.next_fence_id,
                        &self.known_fences);

    // Stage 2: commit (ordering discipline — flushed_byte_len last).
    self.cached_items = items;
    self.fence_placeholders = placeholders;
    self.known_fences = /* fences whose range ⊂ [0, new_flushed) */;
    self.flushed_byte_len = new_flushed;  // LAST

    new_fences
}
```

`scan_authoritative` is a pure function of `(&str, bool, bool)`; it returns
`(max_end: usize, Vec<(Range<usize>, String)>)`. It runs one pulldown pass
and does no `tui_markdown` work.

`build_items_for` is the existing rebuild Stages 2–5 (fence id assignment,
transform with sentinels, `tui_markdown::from_str`, split-by-sentinels).
Its input is the prefix, not full `raw_text`. Cost: O(flushed_byte_len).

**Panic safety.** Assign `flushed_byte_len` as the final mutation. If any
earlier stage panics, the cursor remains at its prior value; `cached_items`
may be partially stale but the next successful rebuild restores consistency.

**`flush_now`** is `rebuild(states, permit_eof_closure = false)` — the
existing public entry point, unchanged in signature but routed through the
new two-stage implementation.

**`flush_final`** is `rebuild(states, permit_eof_closure = true)` + set
`finalized = true`. The EOF-permissive rule relaxes `range.end < len` to
`range.end <= len`; since no more bytes will arrive after `TurnComplete`,
this is safe.

**`indent_and_clone`** (referenced in Section 5) is a small helper that
prepends a 3-space indent span and clones the source line's spans, style,
and alignment. One-time private helper; not part of the public API.

### 4. Flush trigger policy

```rust
pub const DEBOUNCE: Duration = Duration::from_millis(50);
pub const SAFETY_CAP_BYTES: usize = 64 * 1024;

pub fn maybe_flush(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
    // Guard: nothing to flush if not dirty. Load-bearing: prevents the
    // stateless heuristic from re-firing when cursor fails to advance.
    let Some(dirty_at) = self.dirty_since else { return Vec::new(); };
    if self.raw_text.is_empty() { return Vec::new(); }

    let tail_len = self.raw_text.len() - self.flushed_byte_len;
    let tail = &self.raw_text[self.flushed_byte_len..];

    // Safety valve: very large tail with no closure pattern. Suppress
    // parse; plain-text tail render continues. Re-armed on next append.
    if tail_len > SAFETY_CAP_BYTES && !has_authoritative_closure_pattern(tail) {
        self.dirty_since = None;
        return Vec::new();
    }

    // Fast path: authoritative closure pattern visible in tail.
    if has_authoritative_closure_pattern(tail) {
        return self.flush_now(states);
    }

    // Debounce.
    if dirty_at.elapsed() >= DEBOUNCE {
        return self.flush_now(states);
    }

    Vec::new()
}

fn has_authoritative_closure_pattern(tail: &str) -> bool {
    // (a) Paragraph / block close: \n\n with content after.
    if let Some(idx) = tail.rfind("\n\n") {
        if idx + 2 < tail.len() { return true; }
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
```

**Properties.**
- Stateless heuristic; gated by `dirty_since.is_some()` to prevent busy-loop.
- False positives permitted (wasted rebuild, cursor fails to advance;
  subsequent ticks short-circuit on `dirty_since == None` until next append).
- False negatives bounded at 50 ms by debounce.
- Memchr-fast; < 2 µs at typical tail sizes.

**Interactions.**
- `mark_dirty_now()` (mermaid state transitions) sets `dirty_since = now − DEBOUNCE`;
  next `maybe_flush` triggers the debounce path.
- `TurnComplete` calls `ReactTrace::force_flush_all`, which uses `flush_final`
  instead of `flush_now` under this design.
- `append()` is unchanged aside from the `debug_assert!(!self.finalized)` guard.

### 5. Render-site changes

Both `builder.rs:build_virtual_rows` (primary, virtual-row pagination) and
`builder.rs:build_display_lines` (secondary, flat lines) render AgentMessage
bodies via a shared helper.

**Shared helper (feature-gated).** The helper iterates committed items and
the pending tail in a single pass, routing each emission through one of two
caller-provided closures: `emit_line` for text (and for fence placeholders)
and `emit_fence_image` for fence image rows. This keeps both render paths
traversing the same ordering logic while letting the primary path spill
multiple `ImageRow` entries per fence.

```rust
#[cfg(feature = "markdown")]
fn render_agent_message_body(
    stream: &MarkdownStream,
    fence_state: &HashMap<MermaidId, FenceRender>,
    mut emit_line: impl FnMut(Line<'static>),
    mut emit_fence_image: impl FnMut(MermaidId, u16),
) {
    let (items, tail) = stream.items_and_tail();

    for item in items {
        match item {
            StreamItem::Text(text_lines) => {
                for line in text_lines {
                    emit_line(indent_and_clone(line));
                }
            }
            StreamItem::Fence(id) => {
                match fence_state.get(id).copied() {
                    Some(FenceRender::Ready(h)) if h > 0 => {
                        emit_fence_image(*id, h);
                    }
                    other => {
                        let render = match other {
                            Some(FenceRender::Error) => FenceRender::Error,
                            _ => FenceRender::Pending,
                        };
                        let placeholder = fence_placeholder_line(*id, render);
                        emit_line(indent_and_clone(&placeholder));
                    }
                }
            }
        }
    }

    // Plain-text tail, white, indented.
    for text_line in tail.lines() {
        emit_line(Line::from(vec![
            Span::raw("   "),
            Span::styled(text_line.to_string(), Style::default().fg(Color::White)),
        ]));
    }
}
```

**Primary path call site** (`build_virtual_rows`, around `:490-576`): push
the agent header, call `render_agent_message_body` with:
- `fence_state`: the existing `&HashMap<MermaidId, FenceRender>` argument.
- `emit_line`: `|line| push_wrapped(&mut rows, line)`.
- `emit_fence_image`: `|id, h| { for r in 0..h { rows.push(VirtualRow::ImageRow { id, row_within: r, total_rows: h }); } }`.

**Secondary path call site** (`build_display_lines`, around `:115-143`): push
the agent header, call `render_agent_message_body` with:
- `fence_state`: a reference to a caller-constructed map (or an empty map)
  — the secondary path currently has no state-aware fence rendering; passing
  an empty map forces the `FenceRender::Pending` branch uniformly, which is
  consistent with today's behavior where `build_display_lines` doesn't
  consult fence state.
- `emit_line`: `|line| lines.push(line)`.
- `emit_fence_image`: unreachable under an empty `fence_state`, but provided
  as `|_id, _h| unreachable!("secondary path passes empty fence_state")` for
  type completeness.

**Feature-off fallback** (both paths): unchanged. Render `entry.text.lines()`
with the existing plain-white-indented style.

**Removed:** the `items_rendered: bool` gymnastics at `builder.rs:115-141`
and `:504-575`. The shared helper and unconditional tail render eliminate
the `items().is_empty()` fallback branch.

### 6. Edge cases

- **Setext heading retroactive promotion.** Pulldown emits `TagEnd::Heading`
  (not `TagEnd::Paragraph`) once it sees `===`/`---`. The len-compare rule
  never commits a paragraph whose interpretation is pending. One visible
  style transition at the boundary.
- **Lazy list continuation.** List content lives in the tail (rendered as
  plain markers `- foo`) until the list close is authoritative. One style
  transition at close.
- **Indented code blocks.** Covered by the general rule; rendered as plain
  indented tail until a non-indented line disambiguates.
- **HTML blocks.** Covered by the general rule.
- **Pathological long paragraphs (>64 KiB).** Safety valve suppresses parse.
  Tail renders as plain text until `TurnComplete`. Render cost for the tail
  itself is bounded by `ReactTrace`'s virtual-row construction (pre-existing
  concern, out of scope).
- **UTF-8 safety.** Pulldown's `range.end` is always on a character boundary
  in `&str`; `raw_text[flushed_byte_len..]` is always valid UTF-8.
- **Mermaid fence registered past cursor.** Impossible under the Section 2
  coherence fix; fence registration uses the same `range.end < raw_text.len()`
  guard as cursor advance.
- **Post-finalize append (contract violation).** `debug_assert!(!self.finalized)`
  in `append()`. In release, the next rebuild runs under the normal rule and
  self-heals: buggy trailing bytes stay in the tail, committed prefix stays
  consistent.

## Testing Strategy

Three test layers under `crates/spur-tui`:

| Layer | Location | Scope |
|---|---|---|
| Unit: MarkdownStream | `tests/markdown_stream_tests.rs` + inline | cursor, rebuild stages, flush triggers |
| Unit: render paths | `src/components/react_trace/builder.rs` inline | both paths via shared helper |
| Integration: end-to-end | `src/components/react_trace/streaming_tests.rs` (new) | chunk sequences + paint cycles |

**Test matrix (35 tests, ~5 s cumulative run time):**

Cursor and contract:
- T2.1 `items_and_tail_sum_equals_raw_text_len`
- T2.2 `items_and_tail_matches_legacy_accessors_after_flush`
- T2.3 `append_without_flush_does_not_change_flushed_byte_len`
- T2.4 `flushed_byte_len_is_monotonic_across_flushes`
- T2.5 `scan_authoritative_range_end_lt_len_advances_cursor`
- T2.6 `scan_authoritative_range_end_eq_len_does_not_advance`

Flush policy:
- T3.1 `fast_path_fires_on_double_newline_with_content_after`
- T3.2 `double_newline_at_eof_does_not_trigger_fast_path`
- T3.3 `fence_close_with_trailing_content_triggers_fast_path`
- T3.4 `debounce_path_fires_after_50ms_without_boundary`
- T3.5 `tail_above_safety_cap_without_boundary_suppresses_rebuild`
- T3.6 `tail_above_safety_cap_with_boundary_still_flushes`
- T3.7 `maybe_flush_short_circuits_when_dirty_since_none` (load-bearing)
- T3.8 `fast_path_does_not_re_fire_when_cursor_fails_to_advance` (load-bearing)
- T3.9 `append_after_failed_flush_is_bounded_to_one_additional_rebuild`

Render paths:
- T4.1 `tail_renders_as_plain_white_text`
- T4.2 `committed_items_render_with_tui_markdown_styling`
- T4.3 `both_render_paths_produce_identical_textual_content` (load-bearing)
- T4.4 `tail_with_incomplete_bold_renders_literal_asterisks`
- T4.5 `fence_in_committed_prefix_emits_image_row_in_primary_path`
- T4.6 `fence_in_committed_prefix_emits_placeholder_line_in_secondary_path`
- T4.7 `entry_row_starts_accounts_for_items_and_tail_rows`
- T4.8 `non_markdown_feature_build_falls_back_to_entry_text` (cfg-gated)

Edge cases:
- T5.1 `turn_complete_final_paragraph_renders_styled`
- T5.2 `turn_complete_final_code_fence_renders_styled`
- T5.3 `setext_promotion_retroactively_restyles_at_boundary`
- T5.4 `list_in_progress_renders_markers_as_plain_then_promotes_on_close`
- T5.5 `indented_code_in_tail_renders_readably`
- T5.6 `unicode_content_cursor_advances_on_char_boundary`
- T5.7 `no_fence_registered_with_range_end_at_eof`
- T5.8 `append_after_flush_final_debug_asserts` (debug_assertions-gated)
- T5.9 `append_after_flush_final_in_release_is_self_healing`
- T5.10 `rebuild_tui_markdown_cost_scales_with_flushed_byte_len_not_raw_text_len`

Scope discipline:
- T6.1 `drain_cap_constant_untouched_by_this_patch`
- T6.2 `debounce_constant_untouched_by_this_patch`

Integration:
- TI.1 `streaming_sequence_produces_monotonic_visible_content`
- TI.2 `ghost_text_rc1_regression` (original RCA failing case)
- TI.3 `code_block_with_blank_line_does_not_cause_cpu_spike`

**Test-only instrumentation:**
- `MarkdownStream::rebuild_count_for_tests()` — `#[cfg(test)] pub` counter.
- `MarkdownStream::flushed_byte_len_for_tests()` — `#[cfg(test)] pub` accessor.
- `MarkdownStream::is_finalized()` — production accessor.

## Scope Boundaries

**In scope:**
1. Cursor-split renderer with authoritative-closure rule.
2. Two-stage `rebuild()` (`scan_authoritative` + `build_items_for`).
3. Multi-condition flush policy with dirty-guard, fast path, debounce, safety valve.
4. `flush_final` for `TurnComplete` with `finalized: bool` guard.
5. Shared `render_agent_message_body` helper unifying both render paths.
6. Accessors: `items_and_tail`, `fence_placeholder_for`, `is_finalized`.
7. 35-test regression suite.

**Out of scope (explicit):**
1. `DRAIN_CAP_PER_FRAME` tuning. Orthogonal concern; re-evaluation showed
   bundling introduces backlog risk under bursty producers. Track separately
   against `plans/2026-04-14-spurevent-stream-backbone-plan.md`.
2. `alt+m` raw/rendered toggle (Gemini CLI UX pattern). Trivial under this
   design via `raw_text()` accessor; follow-up PR.
3. Render-cost bound for pathological tails. `build_virtual_rows` constructs
   virtual rows for all content, not just viewport. Pre-existing issue;
   track against `ReactTrace` virtualization.
4. Reference link definition retroactive activation. Acknowledged limitation.
5. Syntax highlighting cache for committed code blocks. Separate optimization.
6. Per-block cache (Semidown-style). O(N) strict total parse cost; overkill
   at current message-size envelope.
7. Replacing pulldown-cmark with an incremental parser (Lezer-style). Out
   of budget.

## Rollback and Risk

**Rollback.** Single `git revert` of the feature commit range. No schema,
event-format, or database migration; all changes are local to two files.

**Feature flag.** Rejected. The design replaces a bug with correct behavior;
there is no safe prior state to fall back to.

**Risk register:**

| Risk | Likelihood | Mitigation |
|---|---|---|
| `scan_authoritative` mis-classifies an event | low | T2.5, T2.6, T5.3, T5.4 |
| Two-stage rebuild lifetime/borrow issue | low | types enforce prefix is `&str` slice of raw_text |
| Fast-path heuristic misses a common pattern | low | debounce bounds worst-case at 50 ms |
| Shared helper regresses ImageRow emission | medium | T4.5 + manual mermaid smoke test |
| Busy-loop fix (dirty_since guard) has missed case | low-medium | T3.7, T3.8, TI.3 |
| Performance regression on short messages | low | T5.10 soft benchmark |

## Deliverables

1. **Source:** `crates/spur-tui/src/components/markdown_stream.rs` — state
   additions, `scan_authoritative`, two-stage `rebuild`, `flush_final`,
   coherence fix for mermaid registration.
2. **Source:** `crates/spur-tui/src/components/react_trace/builder.rs` —
   `render_agent_message_body` helper, both render-path migration, fallback
   branch removal.
3. **Source:** `crates/spur-tui/src/components/react_trace/mod.rs` —
   `force_flush_all` switched from `flush_now` to `flush_final`.
4. **Tests:** additions to `crates/spur-tui/tests/markdown_stream_tests.rs`,
   new `crates/spur-tui/src/components/react_trace/streaming_tests.rs`.
5. **No changes to:** `app.rs`, `session_detail.rs`, event plumbing, MCP
   surface, schemas, `DRAIN_CAP_PER_FRAME`.

## References (industry grounding)

Design grounded against five independent implementations that converge on
block-boundary checkpointing with a plain-rendered pending tail:

- Lezer incremental markdown parser (formalizes "open vs closed" blocks,
  documents setext/lazy-continuation/reference-link hard cases)
  — https://github.com/lezer-parser/markdown
- Semidown (block-level checkpoint, inline re-render per block)
  — https://github.com/chuanqisun/semidown
- solid-streaming-markdown (append-only text nodes, selection stability)
  — https://github.com/andi23rosca/solid-streaming-markdown
- md2term (multi-condition flush triggers, safe-boundary heuristic)
  — https://github.com/statico/md2term
- Nelson research gist (pulldown-cmark `into_offset_iter()` pattern for
  source-range keyed caching)
  — https://gist.github.com/nelson-ddatalabs/21290f85c8bd13bb56676560c114980d
- pulldown-cmark `OffsetIter` documentation
  — https://docs.rs/pulldown-cmark/latest/pulldown_cmark/struct.OffsetIter.html
- Ratatui rendering model (double-buffer diff; confirms terminal-cell
  ghosting is not the root cause)
  — https://ratatui.rs/concepts/rendering/under-the-hood/
