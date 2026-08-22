# ReactTrace Streaming Tail Reuse Implementation Plan

> **For SPUR orchestrator:** This plan is intentionally executed inline because
> the user explicitly requested no worker delegation. Its single task is tracked
> in beads as `bd-1wvc`.

**Source spec:** `docs/superpowers/specs/2026-08-22-react-trace-streaming-tail-reuse-design.md`
**Formal @spec cells:** none
**Design epic:** `bd-gopp` (closed after recording the approved design)

**Goal:** Reuse completed visual rows for a correctness-safe plain-text streaming tail and verify the measured CPU/memory reduction.

**Architecture:** Extend the markdown virtual-row cache with conservative active-tail metadata. A safe append rewraps only the previous EOF row plus the delta; all Markdown-sensitive or cache-key changes retain the existing full rebuild.

**Tech Stack:** Rust 2021, Ratatui `Line`/`Span`, existing `MarkdownStream`, TestBackend, Instruments folded stacks, DuckDB/quack-flamegraph.

---

### Task 1: Add and verify plain streaming-tail row reuse

**Task ID:** `bd-1wvc`

**Files:**
- Modify: `crates/spur-tui/src/components/line_wrap.rs`
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs`
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`
- Modify only if cache-construction compatibility requires it: `crates/spur-tui/src/components/react_trace/mod.rs`
- Test: `crates/spur-tui/src/components/react_trace/render.rs`

**Depends on:** none

**Acceptance Criteria:**
- [x] Existing code exhibits RED for prefix-range preservation on safe append.
- [x] Safe plain-text append reuses all completed visual rows before the prior EOF row.
- [x] Incremental output matches a cold rebuild for text, styles, row starts, and row count.
- [x] Newline, Markdown-sensitive, and append-unstable block-prefix cases use the full-rebuild fallback.
- [ ] Focused and full `spur-tui` tests pass through `scripts/spur-cargo`.
  The 1,263 unit tests pass; the full target reaches two unrelated stale
  raw-session-ID assertions in `issue_browser_execute_epic.rs`.
- [x] The identical 10k benchmark is rebuilt, timed, profiled, and compared with a detached-`HEAD` baseline.

**Suggested Worker:** self, inline by explicit user instruction.

**Scope Boundary:**
- IN scope: active markdown `AgentMessage` cache metadata, line-wrap source offsets, safe suffix rewrap, tests, and same-workload profiling.
- OUT of scope: event cadence, terminal backend, Mermaid behavior, general multiline/Markdown incremental parsing, other crates.
- If correctness requires a general Markdown cache or files outside this list, stop and record scope drift before proceeding.

**Implementation:**

- [x] **Step 1: Write the failing render-cache test.**

  Render a long single-line plain agent message, capture the completed prefix
  row range, append a safe text chunk, render again, and assert the prefix range
  is preserved and bounded below the new raw length. Compare the resulting row
  text/style projection with a cold trace containing the final full message.

- [x] **Step 2: Run RED.**

  Run:

  ```bash
  scripts/spur-cargo test -p spur-tui streaming_plain_tail_reuses_completed_rows --no-default-features --features markdown -- --nocapture
  ```

  Expected: assertion failure because every existing body row carries the full
  entry range and the dirty entry is rebuilt from its header.

- [x] **Step 3: Add line-wrap source-start metadata.**

  Refactor the current wrapper through a private callback-based implementation
  so ordinary callers allocate no metadata. Add a crate-visible helper that
  returns visual lines plus their source character starts for cache creation
  and suffix refresh.

- [x] **Step 4: Add conservative active-tail cache metadata and fast path.**

  Capture metadata only for a verified one-line plain body. On safe append,
  truncate at the previous final visual row, construct the raw suffix directly,
  wrap it, restore the separator, refresh ranges/generation, and count reuse.
  Every failed precondition follows the existing full rebuild.

- [x] **Step 5: Run GREEN and fallback tests.**

  Add newline and Markdown-delimiter cases that assert zero reuse and cold-build
  equivalence, then run the focused module tests.

- [x] **Step 6: Verify and profile.**

  Run `scripts/spur-cargo fmt --all`, focused tests, full `spur-tui` tests, and
  clippy. Rebuild the profiling example with the profiling profile, repeat the
  steady and 10k streaming `/usr/bin/time -l` runs, capture a folded profile,
  and query it through quack-flamegraph/DuckDB.

- [x] **Step 7: Record and commit.**

  Add a completion audit to `bd-1wvc`, close it only after verification, and
  commit only the scoped files with an intent-focused message.

## Measured outcome

The final A/B used the same temporary stream-only harness on detached `HEAD`
and the current worktree: 1,000 prior entries, 10,000 plain token appends, a
120x40 viewport, profiling optimizations, and five timed runs per binary.

- Median render time: **0.864 ms/frame → 0.356 ms/frame (2.425x faster)**.
- Median user CPU: **6.84 s → 2.95 s (2.319x less CPU time)**.
- Retired instructions: **115.27 B → 53.35 B (-53.7%)**.
- CPU cycles: **20.76 B → 8.62 B (-58.5%)**.
- Maximum RSS: **39.7 MiB → 11.4 MiB (-71.3%)**.
- Peak physical footprint: **21.6 MiB → 7.2 MiB (-66.5%)**.
- A 20,000-frame Instruments run widened from **1.457 ms/frame** at `HEAD`
  to **0.359 ms/frame** after the fix, a **4.054x** difference as the active
  message grew. `leaks --atExit` reported **0 leaks** for the optimized run.

Quack-flamegraph/DuckDB unique-stack coverage localized the change:

- `build_virtual_rows`: **72.24% → absent from the optimized hot set**;
- `wrap_line_to_width`: **59.61% → suffix wrapper 0.32%**;
- Markdown rebuild: **7.30% → absent from the optimized hot set**;
- `reuse_plain_text_tail`: **0.47%** after the change.

The remaining profile is a different floor: Ratatui `Paragraph` rendering has
59.47% coverage, `unicode_width::str_width` has 40.00%, and TestBackend flush
has 33.10%. A separate graph build used about eight cores and 11.8 GiB RSS
during timing, so the comparison emphasizes per-process CPU, instruction, and
cycle counters in addition to wall time. Folded stacks and SVGs are retained in
`/tmp/spur-react-tail-20260822`.
