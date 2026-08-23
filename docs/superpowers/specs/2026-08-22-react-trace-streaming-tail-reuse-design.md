# ReactTrace Streaming Tail Reuse Design

**Date:** 2026-08-22
**Status:** Approved by user
**Design epic:** `bd-gopp`
**Implementation issue:** `bd-1wvc`

## Goal

Reduce CPU and transient allocation cost while an active `AgentMessage` grows,
without weakening the cursor-split renderer's correctness guarantees.

## Evidence

The 160x40 Session Detail benchmark with 1,000 seeded entries measured:

- steady redraw: 293.868 us/frame;
- 10,000 appended chunks: 1,320.241 us/frame and 56.1 MiB maximum RSS;
- `build_virtual_rows`: 55.124% inclusive CPU;
- `wrap_line_to_width`: 31.619% inclusive CPU;
- allocation-related stacks: 14.013% inclusive CPU;
- a 1.33 MiB live `Vec<(Style, char)>` allocated below `wrap_line_to_width`;
- 6.57 GB cumulative heap/VM allocation in a live stack-logging snapshot,
  while current live heap/VM stayed near 4.8 MiB.

The existing cursor-split design explicitly left pathological tail rendering
and per-block caching out of scope. This profile is the evidence for a bounded
first optimization.

## Invariants

1. The rendered rows after the fast path must equal a cold full rebuild.
2. Previously completed visual rows may be reused only for append-only input.
3. Width, cell metrics, soft-cap, theme/generation, or fence-state drift uses
   the existing full rebuild.
4. Markdown-sensitive input uses the existing preview and full-entry rebuild.
5. A fast-path miss is a performance event, never a correctness failure.
6. Scroll `entry_row_starts`, row/byte-range co-indexing, and the trailing
   separator remain identical to a cold build.

## First-slice design

The first slice recognizes an active, non-finalized `AgentMessage` whose body
is one plain logical line. A conservative predicate rejects control characters,
newlines, and Markdown delimiters that can retroactively reinterpret earlier
text. The initial cold render also verifies that Markdown rendering produced
exactly the indented raw text with a uniform body style.

The virtual-row cache records:

- active entry index and raw byte length;
- global row index of the final visual row;
- raw byte offset where that final visual row starts;
- the body and line styles needed to render its suffix;
- a reuse counter for structural regression tests.

On a safe append, rows before the previous final visual row are retained. Only
`raw_text[final_row_start..]` plus the new bytes are wrapped again, after which
the blank separator and cache metadata are refreshed. This is correct because
all earlier rows were emitted after an actual overflow; only the old EOF row
can change when more characters arrive.

Any failed precondition falls through to the shipped full-entry rebuild. This
includes newlines, Markdown delimiters, finalized streams, a different dirty
entry, width/fence drift, and non-append mutation.

## Test and measurement gates

- RED: a long plain streaming message currently gives every body row the full
  entry byte range and rebuilds the prefix after append.
- GREEN: completed body rows retain bounded source ranges across a safe append,
  and incremental rows/text/styles match a cold build.
- FALLBACK: Markdown-sensitive and multiline appends produce no reuse and still
  match a cold build.
- Run focused line-wrap/render tests, the full `spur-tui` test suite, formatting,
  and clippy through `scripts/spur-cargo`.
- Rebuild the profiling example and repeat the identical steady/10k-stream CPU,
  RSS, flamegraph, and DuckDB coverage queries.

## Out of scope

- General incremental Markdown parsing or per-block caching.
- Cross-line reuse after a newline.
- Event cadence, frame-rate, terminal I/O, Mermaid rasterization, or scrolling
  behavior changes.
- Parallel rendering.
