# ReactTrace Performance Benchmarking Strategy

## Goal

Establish a statistically rigorous benchmark suite for `ReactTrace` that measures the production render path under realistic scale, streaming, and markdown workloads.

## Ground-Check Findings

The first draft captured the right optimization targets but assumed an execution model that the current codebase does not have:

1. `ReactTrace` is **not** a `ratatui::widgets::Widget`. In the default markdown-enabled build, the real render path is `render_with_ctx(...)` on a `Frame`, not `Widget::render(area, &mut Buffer)`.
2. Bench targets in `benches/` can only use the crate's **public API**. Private row builders and cache internals are not directly reachable from a normal benchmark target.
3. `append_message(...)` coalesces consecutive chunks from the same agent. Synthetic history builders must alternate agent ids or insert explicit boundaries, otherwise "1000 entries" collapses into one large message.
4. Markdown is a compile-time feature. Runtime parameters cannot stand in for `--features markdown` vs. `--no-default-features`.
5. Streaming benchmarks must use fresh warmed inputs per sample. Timing one trace that grows forever across benchmark iterations produces misleading drift.

## Industry Grounding

This strategy follows the primary-source guidance from the Rust benchmarking stack:

- Cargo benchmark targets in `benches/` only see public library API.
  Source: <https://doc.rust-lang.org/cargo/reference/cargo-targets.html>
- Divan's `with_inputs(...)` is the correct pattern when setup must be excluded from the timed section.
  Source: <https://docs.rs/divan/latest/divan/struct.Bencher.html>
- Criterion documents the same setup-vs-measured split with `iter_batched` / `iter_batched_ref`.
  Source: <https://docs.rs/criterion/latest/criterion/struct.Bencher.html>
- Ratatui renders through a buffer-backed frame each draw. `TestBackend` + `Terminal::draw` exercises the production path without real terminal I/O.
  Sources: <https://docs.rs/ratatui/latest/ratatui/> and <https://docs.rs/ratatui/latest/ratatui/buffer/struct.Buffer.html>
- The Rust Performance Book recommends representative workloads over overly synthetic microbenchmarks when the goal is actionable optimization.
  Source: <https://nnethercote.github.io/perf-book/benchmarking.html>

## Architecture

### Phase 1: Benchmark the Production Draw Path

Use **Divan** as the benchmark framework, but benchmark the draw path the TUI actually runs:

- own a `Terminal<TestBackend>`
- call `ReactTrace::render_with_ctx(...)` in markdown builds
- call `ReactTrace::render(...)` in non-markdown builds
- keep viewport dimensions fixed, e.g. `120x40`
- keep the mermaid registry empty for the first pass so the benchmark focuses on trace layout, markdown row building, paragraph rendering, and scrollbar work

This deliberately keeps the benchmark attached to real user-visible behavior: frame construction, row cache rebuilds, segmentation, block layout, and repaint cost.

### Phase 2: Optional Internal Microbenchmark Shim

If Phase 1 results later show that `Terminal<TestBackend>` overhead materially obscures `ReactTrace`, add a hidden benchmark-support shim inside `spur-tui` for narrower microbenchmarks:

- virtual-row rebuild only
- markdown finalize / rebuild only
- warm draw from an already-built row cache

This is a follow-up, not the starting point.

## Benchmark Matrix

The benchmark suite should measure four workloads, not one blended "render time":

### 1. Warm Draw, Plain Content

**Question:** What does a steady-state repaint cost when caches are already warm?

- prebuild the trace
- do one warm-up draw
- time repeated draws on the same trace
- vary history size `N = [10, 100, 1000, 5000]`

### 2. First Draw From Fresh Trace

**Question:** What does the first visible render cost after load or width-driven invalidation?

- generate a fresh trace per sample via `with_inputs(...)`
- time exactly one first draw
- vary history size `N = [10, 100, 1000, 5000]`

### 3. Streaming Append + Draw

**Question:** What does one incremental "AI typing" frame cost?

- start from a warmed trace with a live tail entry
- for each sample, use a fresh warmed trace and one fixed chunk payload
- time one append plus one draw
- vary base history `N = [100, 1000]` and chunk size `C = [1, 8, 32]`

### 4. Markdown Finalize vs. Markdown Warm Draw

**Question:** How much time is spent building markdown structures versus simply repainting already-finalized content?

- `markdown_finalize`: append rich content and call `force_flush_all(...)`
- `markdown_warm_draw`: start from finalized content, warm once, then time repeated draws
- rich fixtures should include headings, lists, fenced Rust blocks, and mermaid fences
- vary history size `N = [10, 100, 500]`

Feature-mode comparison (`markdown` on vs. off) must happen across separate Cargo invocations, not a runtime boolean.

## Prototype Validation

To keep the design grounded before landing Divan, a production-path simulator now exists at:

- `crates/spur-tui/examples/react_trace_bench_sim.rs`

It renders through `Terminal<TestBackend>` and reports directional timings for:

- warm draw on plain content
- build plus first draw on fresh plain traces
- a finite append-and-draw streaming run
- markdown finalize
- build/finalize/draw markdown
- warm draw on finalized markdown

### Quick Run Evidence

Command executed:

```bash
cargo run -p spur-tui --example react_trace_bench_sim -- --quick
```

Observed output on 2026-04-18:

- `warm_draw_plain`: `3.259ms`
- `build_plus_first_draw_plain`: `10.036ms`
- `stream_append_draw`: `3.639ms/frame`
- `markdown_finalize`: `21.784ms`
- `build_finalize_draw_markdown`: `48.741ms`
- `warm_draw_markdown`: `2.936ms`

These numbers are **directional only** because they came from an unoptimized dev build and a lightweight simulator, not the final Divan suite. The important signal is structural:

- markdown finalize/build work is materially more expensive than warm repaint
- the production draw path is cheap enough that blending finalize and repaint into one benchmark would hide the real optimization target

## Implementation Outline

1. Add the production-path simulation harness and keep it green.
2. Add Divan infrastructure in `crates/spur-tui/Cargo.toml` and `crates/spur-tui/benches/react_trace.rs`.
3. Build public-API fixtures for plain and markdown traces.
4. Implement the four benchmark workloads above.
5. Run the benchmark suite in separate feature modes where comparison is needed.
6. Only add an internal benchmark shim if Phase 1 data shows the public production path is too noisy.
