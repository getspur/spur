# Telegram HTML Chunked Renderer — Design Spec

> **Status:** v1.0 — design approved by 2 design-review gates (codex + gemini), pending user spec-review before implementation dispatch.
> **Scope:** `crates/spur-bot/src/telegram/{format,client,render}.rs` and `crates/spur-bot/tests/`
> **Branch:** `feat/spur-bot-html-rendering` in worktree `/Volumes/Projects/spur/.worktrees/spur-bot-html-rendering`
> **Supersedes:** the `markdown_to_telegram_html(input: &str) -> String` walker added in commit `9067ad70` and the per-chunk render path in `render.rs:31-35` (commit `b2fbf619`).
> **Reviewer trail:** kimi (FIX-UP), gemini (REWRITE+BLOCKER), codex (REWRITE+stronger evidence), claude-code (REWRITE+Option C). Design-review gate: codex APPROVE-WITH-AMENDMENTS, gemini APPROVE-WITH-AMENDMENTS. Amendments synthesized below.

---

## 1. Problem statement

The current implementation splits raw markdown via `split_for_final_answer` (`runtime.rs:682`) at a 4096 UTF-16 budget, then converts each chunk independently via `markdown_to_telegram_html` (`format.rs:11`) and ships through `send_html_to_thread` (`client.rs:331`). Three independent failure modes follow from this ordering:

**F1 — Context-blind split corrupts markdown semantics.** An 8K-char fenced code block split mid-way leaves chunk 2 with no opening ` ``` `. `pulldown_cmark::Parser` has no cross-chunk recovery, so chunk 2 parses as a paragraph. Worse: the original closing fence in chunk 2 becomes a *new opening fence* that captures following prose (codex confirmed via local pulldown probe).

**F2 — HTML expansion overflows post-conversion.** Telegram's 4096 UTF-16 limit applies to the rendered HTML, not the markdown source. Escape expansion is unbounded across input distribution: `<` → `&lt;` is 4×, `&` → `&amp;` is 5×, `"` → `&quot;` is 6×. A 4095-unit markdown chunk dense in escapable chars converts to >4096 HTML and Telegram rejects it.

**F3 — Length-error fallback does not fire.** `client.rs:347-352` matches only parse-error substrings (`"can't parse entities"`, `"parse entities"`, `"find end of the entity"`). `Bad Request: message is too long` skips the fallback branch; the chunk's send returns `Err`, propagates through `?` in `render.rs:29/35`, and is swallowed by `tracing::error!` at `telegram/mod.rs:88`. Net effect: silent message drop.

**F4 — `FinalAnswer` plain-text fallback is raw markdown.** `render.rs:31-35` passes the unprocessed markdown text as `plain_fallback`. On HTML parse failure, the user sees `**bold**`, `` `code` ``, `[label](url)` rendered verbatim.

Plus secondary defects identified in review:
- `format.rs:13-15` does not pass `Options::ENABLE_TASKLISTS` despite `Event::TaskListMarker` being handled at `:74-76` (handler is dead code).
- `format.rs:97-105` force-closes `<blockquote>` to escape `<pre>`, but `End(CodeBlock)` at `:185-188` never re-opens. `tests/telegram_format.rs:52-58` pins this asymmetric behavior as expected, encoding the bug as a regression test.
- `format.rs:94`/`176` updates `blockquote_depth` without ever reading it (dead state).
- `format.rs:123` emits link `dest_url` without preflighting overflow.
- `ServiceMessage` (`render.rs:24-30`) routes through `render_truncated_text` budgeting RAW markdown at 4096; same expansion overflow class as F2.

---

## 2. First principles (hard constraints)

H1. Telegram's per-message body limit is **4096 UTF-16 code units**, measured *post*-HTML-escape and post-tag-overhead.

H2. HTML mode requires syntactically balanced tag tree per message. No cross-message tag inheritance.

H3. HTML escape expansion is unbounded across input distribution; worst case ≈ 6×.

H4. `pulldown_cmark` 0.13 has no cross-chunk recovery — a chunk starting mid-fence parses incorrectly.

H5. The renderer must always deliver a coherent message. Silent drops (F3 above) are catastrophic.

H1+H3 forbid raw-markdown budgeting. H2+H4 forbid post-conversion text splitting. Both kill any heuristic split-then-convert design.

---

## 3. Design — Option C++ (stream-aware chunked renderer)

A single-pass walker over `pulldown_cmark::Parser` events. Maintains an `(html, plain)` buffer plus typed open-context stacks. At each event, computes the cost of applying the event in current state; if it would push past `budget - dynamic_reserve`, flushes a chunk first then applies the event to a fresh buffer.

### 3.1 State model

```rust
pub struct ChunkedHtmlRenderer<'a, I: Iterator<Item = Event<'a>>> {
    events: I,                // standard Iterator, no Peekable
    state: RendererState,
    budget: ChunkBudget,
    chunks: Vec<Chunk>,
}

pub struct Chunk {
    pub html: String,
    pub plain: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ChunkBudget {
    pub max_units: usize,                    // 4096 default
    pub min_safety_floor: usize,             // 32 default — hard floor under dynamic reserve
    pub max_nesting_depth: u8,               // 8 default — for P5 tag-depth cap
}

#[derive(Default)]
struct RendererState {
    current_html: String,
    current_plain: String,
    open_blocks: Vec<BlockContext>,
    open_inlines: Vec<InlineContext>,
    list_stack: Vec<ListContext>,
    table_state: Option<TableState>,
    suspended_blockquotes: u8,               // count of <blockquote>s closed for an active <pre><code>
    pending_paragraph_break: bool,
}

enum BlockContext {
    BlockQuote,
    PreCode { lang: Option<String> },
}

enum InlineContext {
    Bold,
    Italic,
    Strike,
    Code,
    Link { href: String },
}

struct ListContext {
    kind: ListKind,
    next_number: u64,                        // for numbered lists, supports continuation
    item_continuation: bool,                 // true once first paragraph in tight item has been emitted
}

enum ListKind { Bullet, Numbered }

struct TableState {
    in_header: bool,
    column_count: u8,
    current_cell_index: u8,
}
```

### 3.2 Algorithm (gemini's "check budget before apply")

```rust
pub fn markdown_to_telegram_chunks(input: &str) -> Vec<Chunk> {
    let parser = Parser::new_ext(input, Options::ENABLE_TABLES
                                       | Options::ENABLE_STRIKETHROUGH
                                       | Options::ENABLE_TASKLISTS);
    ChunkedHtmlRenderer::new(parser, ChunkBudget::default()).into_chunks()
}

impl<'a, I: Iterator<Item = Event<'a>>> ChunkedHtmlRenderer<'a, I> {
    pub fn into_chunks(mut self) -> Vec<Chunk> {
        for event in self.events {
            let cost = self.event_cost(&event);
            let reserve = self.dynamic_reserve();
            if self.state.current_html_units() + cost + reserve > self.budget.max_units
                && self.state.at_safe_flush_point()
            {
                self.flush_chunk();
            }
            self.apply_event(event);
        }
        self.finalize();
        self.chunks
    }

    fn dynamic_reserve(&self) -> usize {
        // Cost to close every currently-open block + inline + list, expressed in UTF-16 units.
        // Floor at budget.min_safety_floor.
        // Recomputed every event because the open stacks change.
    }

    fn at_safe_flush_point(&self) -> bool {
        // True iff open_inlines is empty AND we are not inside a TableRow.
        // Block contexts CAN be flushed (they re-open on next chunk).
    }

    fn flush_chunk(&mut self) {
        // 1. Pop and close all open inlines (defensive — should be empty by invariant).
        // 2. Pop and close all open blocks in reverse order.
        // 3. Push (current_html, current_plain) as Chunk.
        // 4. Clear current buffers.
        // 5. Re-push and re-open blocks in original order in fresh buffers.
        //    For PreCode { lang }, re-emit `<pre><code>` (lang class omitted — Telegram ignores).
        //    For BlockQuote, re-emit `<blockquote>`.
        //    For list contexts, emit the continuation prefix using next_number.
    }
}
```

### 3.3 Edge-case policies

**P1 — Entity-aware text splitting.** When a single Text event's escaped length exceeds remaining budget, split the text. Boundaries in priority order: line break (`\n`), word boundary (whitespace), char boundary. **Crucial:** never slice mid-entity (`&am` | `p;`). Track entity boundaries explicitly during the char-fallback path.

**P2 — Inline overflow fallback.** If an inline run's full payload (e.g., `[long label](long_href)` where `15 + escaped_href + label > budget`) cannot fit in any single chunk, degrade to plain-text projection: emit `(href)` or `label (href)` as escaped text, no `<a>` tag. Same rule for an oversized inline `Code(text)` — emit escaped text without `<code>` wrapper.

**P3 — Oversized fenced code block.** A single fenced block whose body exceeds `budget - dynamic_reserve - opener_close_cost` must split intra-block. Emit `</code></pre>`, flush, re-emit `<pre><code>`, resume. Boundary preference: `\n` line, then word, then char (entity-aware per P1). Lang class is preserved across re-wraps via `BlockContext::PreCode { lang }`.

**P4 — Blockquote-suspend-during-code (symmetric).** When `Start(CodeBlock)` arrives while `open_blocks` contains `BlockQuote`(s), close them, increment `suspended_blockquotes`, then open `<pre><code>`. On `End(CodeBlock)`, close `<pre><code>`, then re-open `suspended_blockquotes` `<blockquote>` elements before continuing. Replaces the asymmetric force-close currently pinned by `tests/telegram_format.rs:52-58`. **The pinned test is rewritten** to assert symmetric behavior.

**P5 — Tag-depth cap.** Telegram has undocumented HTML nesting limits. If `open_blocks.len() + open_inlines.len() + list_stack.len() > budget.max_nesting_depth`, degrade the current subtree to plain-text projection until depth returns under the cap.

**P6 — Table cells must not split mid-cell.** `at_safe_flush_point()` returns false while `table_state` indicates an active row. Cell content cannot overflow without falling back to inline-overflow rules (P2). If a single cell exceeds budget, emit the row as plain pipe-separated text.

### 3.4 Why this design (Rust idiom)

- `Iterator<Item = Event<'a>>` consumed standardly — no `Peekable`, no upfront `Vec<Event>` allocation. Closes kimi's nit and gemini's "drop predictive lookahead" amendment.
- Stack-based open-context replaces the broken `blockquote_depth` / `open_blockquotes` divorce kimi found.
- Generic over `I: Iterator<Item = Event>` — testable with synthetic event streams.
- 0 `unsafe`. No `clone()` beyond text content into `String`. `BlockContext::PreCode { lang: Option<String> }` owns the lang string for re-wrap; lifetime story is clean.
- Errors live one layer up in `send_html_to_thread`. The renderer is total — every input produces a `Vec<Chunk>` and every chunk satisfies the invariants.

---

## 4. Phased shipping plan

### Phase 0 — Error gate fix (separate PR, lands first)

**Scope:** `client.rs:347-352` only. Replace the 3-substring English regex with code-and-context check.

```rust
// Old
if let Some(desc) = telegram_html_parse_error_description(&err) {
    if desc.contains("can't parse entities") || ...
}

// New
if err.is_400() && parse_mode_was_html {
    retry_with_plain_fallback(...).await
}
```

**Rationale:** durable across Telegram API string changes; catches `message is too long` (F3) and any other 400-class error from an HTML send. Decoupling from the chunker rewrite makes both PRs reviewable in isolation and lets either be reverted independently.

**LOC:** ~20.

**Verification:** `scripts/spur-cargo test -p spur-bot --test telegram_html_send`.

### Phase 1 — Chunked renderer (this design)

**Scope (`crates/spur-bot/src/telegram/format.rs`):**
- Add `ChunkedHtmlRenderer`, `Chunk`, `ChunkBudget`, `RendererState`, `BlockContext`, `InlineContext`, `ListContext`, `ListKind`, `TableState`.
- Add public `markdown_to_telegram_chunks(input: &str) -> Vec<Chunk>`.
- Keep `markdown_to_telegram_html(input: &str) -> String` as a thin wrapper that joins chunks with `"\n\n"` for any caller that wants a single string (currently no production caller; kept for the 11 existing tests' continuity).
- Drop `blockquote_depth` (dead state).
- Replace `format!("{number}. ")` with `write!(self.out, "{number}. ")?` (output is `String`; `write!` cannot fail — use `unreachable!()` arm if needed, NOT `.unwrap()`).
- Add `Options::ENABLE_TASKLISTS` to the parser options.

**Scope (`crates/spur-bot/src/telegram/render.rs`):**
- Replace the FinalAnswer arm at `:31-35` with a loop:
  ```rust
  let chunks = markdown_to_telegram_chunks(&text);
  for chunk in chunks {
      client.send_html_to_thread(chat_id, mt, &chunk.html, &chunk.plain).await?;
  }
  ```
- Same change for `ServiceMessage` at `:24-30`. Drops `render_truncated_text` from this arm.
- `StreamChunk`, `WorkingStatus`, button rows, `CreateTopic` are unchanged (stay plain).

**Scope (`crates/spur-bot/tests/telegram_format.rs`):**
- Rewrite the test at `:52-58` (asymmetric blockquote-close) to assert symmetric P4 behavior.
- Adapt the 11 existing tests to call `markdown_to_telegram_chunks(...)` and assert on `chunks[0].html` + `chunks.len() == 1`.
- Add new unit tests covering each edge-case policy P1-P6.
- Add golden-file tests under `crates/spur-bot/tests/telegram_format/golden/` — 6 representative LLM markdown samples, each with an `expected.json` chunk array.
- Add a proptest harness (random markdown → invariants: `chunk.html.encode_utf16().count() <= 4096`, balanced tags, plain projection contains no `**`/`` ` ``/`[`-`]`-`(`-`)` markdown markers).

**LOC:** 275-400 production + ~250 tests.

**Verification:**
```
scripts/spur-cargo test -p spur-bot
scripts/spur-cargo test -p spur-tui --features markdown
scripts/spur-cargo test -p spur-tui --no-default-features
scripts/spur-cargo clippy -p spur-bot --all-targets --no-deps -- -D warnings
```

### Phase 2 — Deferred (NOT in this design)

- `sendDocument` fallback for raw input above ~50KB.
- StreamChunk HTML rendering (currently raw markdown during streaming — visual inconsistency).
- `Tag::Image` URL preservation in plain-text projection.
- Sender-layer audit: 429 mid-multi-chunk handling, chunk ordering guarantees, partial-success UX.

---

## 5. Test surface detail

### 5.1 Unit tests (`crates/spur-bot/src/telegram/format.rs` `#[cfg(test)] mod tests`)

Existing 11 tests adapted. Plus:

- `tasklists_enabled_renders_checkboxes`
- `entity_aware_split_never_slices_mid_amp`
- `inline_link_overflow_falls_back_to_plain_url`
- `inline_code_overflow_falls_back_to_escaped_text`
- `oversized_fenced_block_splits_at_line_boundary`
- `oversized_fenced_block_single_line_falls_to_char_boundary`
- `blockquote_with_code_re_opens_after_code_block` (replaces the asymmetric pinned test)
- `nested_blockquote_with_code_re_opens_full_depth`
- `tag_depth_cap_degrades_to_plain_at_excess`
- `table_cell_overflow_emits_pipe_row`
- `dynamic_reserve_accounts_for_open_block_depth`
- `numbered_list_continuation_resumes_at_correct_index`
- `non_table_pipe_text_renders_as_text` (codex's regression)

### 5.2 Integration tests (`crates/spur-bot/tests/telegram_format.rs`)

- `golden/llm-output-1.md`: long technical answer with mixed code, lists, links
- `golden/llm-output-2.md`: 8K-char single fenced code block (Rust)
- `golden/llm-output-3.md`: deeply nested blockquote with embedded code
- `golden/llm-output-4.md`: table with 30 rows
- `golden/llm-output-5.md`: adversarial `<<<<` density inside a code block
- `golden/llm-output-6.md`: pathological 5K-char single line with no `\n`

Each fixture has a committed `expected.json` produced once by manual visual review of an initial `markdown_to_telegram_chunks(input)` run, then committed verbatim. Test asserts byte-equality of the JSON-serialized chunk array. Updates require explicit re-review (visible in PR diff).

### 5.3 Property tests (`proptest` in same file)

```rust
proptest! {
    #[test]
    fn no_chunk_exceeds_telegram_limit(input in any_markdown()) {
        for chunk in markdown_to_telegram_chunks(&input) {
            prop_assert!(chunk.html.encode_utf16().count() <= 4096);
            prop_assert!(chunk.plain.encode_utf16().count() <= 4096);
        }
    }

    #[test]
    fn html_chunks_have_balanced_tags(input in any_markdown()) {
        for chunk in markdown_to_telegram_chunks(&input) {
            prop_assert!(is_telegram_html_balanced(&chunk.html));
        }
    }

    #[test]
    fn plain_projection_strips_markdown_markers(input in any_markdown()) {
        for chunk in markdown_to_telegram_chunks(&input) {
            prop_assert!(!chunk.plain.contains("**"));
            prop_assert!(!chunk.plain.contains("```"));
            // ... etc
        }
    }
}
```

`any_markdown()` is a custom strategy producing varied markdown trees with bounded depth.

### 5.4 Send-path tests (`crates/spur-bot/tests/telegram_html_send.rs`)

Existing fallback test (`send_html_to_thread_fallback_on_parse_error`) keeps passing. Phase 0 adds:

- `send_html_to_thread_fallback_on_message_too_long_400`
- `send_html_to_thread_fallback_on_arbitrary_400_with_html_parse_mode`
- `send_html_to_thread_does_not_fallback_on_429` (429 is a separate path — pause-and-retry, not plain fallback)

---

## 6. Reviewer-gate protocol

Each phase needs `APPROVE` from BOTH reviewers before merge. `REVISE` triggers respin.

**Phase 0 reviewers:** kimi, gemini.

**Phase 1 reviewers:** codex, kimi (claude-code recused — designer-of-record for Option C).

---

## 7. Risk register

| Risk | Severity | Mitigation |
|------|----------|------------|
| State machine bugs in unusual event sequences | Medium | Phase 0 error gate catches any 400 → plain fallback, regardless of renderer correctness. Multi-layer defense. |
| `dynamic_reserve` miscalculated, occasional 4097-unit chunk | Low | `min_safety_floor: 32`. Phase 0 fallback catches Telegram rejection. |
| Visual regression from chunk boundaries landing differently than current single-message rendering | Low | Golden-file tests with realistic LLM outputs catch this pre-merge. |
| Complexity creep | Medium | Hard scope cut at this spec. Phase 2 features are out of scope, no exceptions. |
| LOC budget understated (codex flagged 275-400, original C+ proposed 80) | Acknowledged | Spec budget is 275-400; any worker exceeding 500 must signal back to brain via `report_signal` per spurpower-worker-signals. |
| Pulldown 0.13 emits an event variant we haven't enumerated | Low | Codex verified against locked 0.13.3: `Tables`, `TaskListMarker`, `Html`/`InlineHtml` split, all enumerated. The `_` arm logs and emits escaped text (defensive). |

---

## 8. Out of scope (will NOT change)

- `StreamChunk` rendering during turn streaming (still raw markdown — Phase 2).
- 429 rate-limit handling in `client.rs:paused_until` (already exists; no changes).
- Cancellation drain in `telegram/mod.rs:152-182` (drain_remaining_inputs — already correct per gemini's earlier finding fix).
- ACP event handling in `runtime.rs` — chunker is only invoked from `render.rs`.
- `frankenstein` API — chunker emits HTML strings; sender continues to build the same `SendMessageParams`.

---

## 9. Acceptance criteria

- [ ] All 11 existing format.rs tests pass with adapted callsites.
- [ ] All Phase 1 unit tests + golden-file tests + proptest harness pass.
- [ ] `scripts/spur-cargo test -p spur-bot` exits 0.
- [ ] `scripts/spur-cargo test -p spur-tui --features markdown` exits 0.
- [ ] `scripts/spur-cargo test -p spur-tui --no-default-features` exits 0.
- [ ] `scripts/spur-cargo clippy -p spur-bot --all-targets --no-deps -- -D warnings` exits 0.
- [ ] `tests/telegram_format.rs:52-58` is rewritten and asserts symmetric blockquote behavior.
- [ ] No silent message drops on the F3 path: a markdown input that produces overlong HTML reaches the user via plain fallback.
- [ ] Both reviewers `APPROVE` each phase.

---

## 10. Open questions (must be answered before implementation starts)

None at design time. Codex verified the pulldown 0.13.3 event surface. Gemini verified the layering decisions. The Telegram tag-depth limit is undocumented; the spec uses `MAX_NESTING_DEPTH = 8` as a conservative default and exposes it on `RendererState` for tuning if a real input ever hits it.
