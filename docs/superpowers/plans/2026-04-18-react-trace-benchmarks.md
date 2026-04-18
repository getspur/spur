# ReactTrace Performance Benchmarking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish a statistically rigorous benchmark suite for `ReactTrace` that measures the production render path under realistic scale, streaming, and markdown workloads.

## Ground-Check Review

The earlier draft had the right intent but several assumptions do not match the current codebase:

1. `ReactTrace` is **not** a `ratatui::widgets::Widget`. In the markdown-enabled build that ships by default, the production path is [`render_with_ctx`](</Volumes/Projects/spur/crates/spur-tui/src/components/react_trace/render.rs:381>) via a `Frame`, not `Widget::render(area, &mut Buffer)`.
2. Bench targets in `benches/` only get the crate's **public API**. They cannot directly call private helpers such as `build_virtual_rows`, cache internals, or module-private row builders without an intentional support shim.
3. `append_message` coalesces consecutive chunks from the same agent. A naive history generator that loops on one agent string produces one giant entry, not `N` distinct entries.
4. The markdown feature is compile-time. A benchmark parameter like `args = [true, false]` does **not** compare `--features markdown` vs. `--no-default-features`; it only changes fixture content inside the same compiled mode.
5. The hot-stream benchmark in the draft mutates one trace across all iterations. That produces iteration-dependent timing drift instead of comparable per-sample measurements.

## Industry Grounding

These decisions align with the benchmark patterns recommended by the primary tooling docs:

- Cargo benchmark targets in `benches/` can use only the library's public API, while private API access requires library/binary-local benchmarks or a public shim.
  Source: <https://doc.rust-lang.org/cargo/reference/cargo-targets.html>
- Divan's `with_inputs(...)` excludes setup time from the measured section and is the right tool when each sample needs a fresh input.
  Source: <https://docs.rs/divan/latest/divan/struct.Bencher.html>
- Criterion documents the same principle with `iter_batched` / `iter_batched_ref`: separate per-iteration setup from the routine being timed.
  Source: <https://docs.rs/criterion/latest/criterion/struct.Bencher.html>
- Ratatui renders through an intermediate buffer each frame. Using `TestBackend` + `Terminal::draw` exercises the real frame/block/scrollbar path without terminal I/O.
  Sources: <https://docs.rs/ratatui/latest/ratatui/> and <https://docs.rs/ratatui/latest/ratatui/buffer/struct.Buffer.html>
- Divan's `AllocProfiler` affects timing because allocation counting runs during timed sections, so allocation-enabled timings must be interpreted as relative comparisons, not pure wall-clock truth.
  Source: <https://docs.rs/divan/latest/divan/struct.AllocProfiler.html>
- The Rust Performance Book recommends representative workloads over purely synthetic microbenchmarks when the goal is actionable optimization.
  Source: <https://nnethercote.github.io/perf-book/benchmarking.html>

## Architecture

Phase 1 should benchmark the **production draw path**:

- Create a `Terminal<TestBackend>`.
- Render `ReactTrace` through `render_with_ctx(...)` in markdown builds and `render(...)` otherwise.
- Use a fixed viewport such as `120x40` so results are comparable.
- Keep the mermaid registry empty in the first pass; benchmark mermaid raster/image rendering separately if needed.

This is the best first benchmark because it measures what users actually exercise: cache rebuilds, row segmentation, block layout, paragraph rendering, and scrollbar math.

If later profiling shows `Terminal<TestBackend>` overhead is materially obscuring `ReactTrace`, add a **hidden benchmark-support shim** inside `spur-tui` and benchmark internal phases directly:

- virtual-row rebuild
- warm draw from cached rows
- markdown finalize / rebuild

Do not start with that shim. Start with the production path, then split deeper only if the measurements demand it.

## Benchmark Matrix

We want four distinct workloads, not one blended number:

### 1. Warm Draw, Plain Content

**Question:** How expensive is drawing an already-built trace when caches are warm?

- Fixture: prebuilt plain-text trace, fixed width/height, one warm-up draw before timing.
- Measurement: repeated `draw(...)` on the same trace.
- Parameters: history size `N = [10, 100, 1000, 5000]`.

This isolates steady-state repaint cost.

### 2. First Draw From Fresh Trace

**Question:** What does the initial visible render cost after loading or after width-driven cache invalidation?

- Fixture: prebuilt trace cloned or regenerated outside the measured section.
- Measurement: the first `draw(...)` on a fresh instance.
- Parameters: history size `N = [10, 100, 1000, 5000]`.

This captures cache population and first layout work.

### 3. Streaming Append + Draw

**Question:** What does one incremental "AI typing" frame cost?

- Fixture: warmed trace with a live tail entry reserved for streaming.
- Measurement unit: one fixed chunk append plus one draw on a fresh warmed trace per sample.
- Parameters:
  - base history `N = [100, 1000]`
  - chunk size `C = [1, 8, 32]` bytes

Important: use fresh warmed input per sample via `with_inputs(...)`. Do **not** let one benchmark sample grow forever across iterations.

### 4. Markdown Finalize vs. Markdown Warm Draw

**Question:** How much time is spent finalizing/building markdown structures versus simply repainting already-finalized content?

- Fixture: rich markdown entries with headings, lists, code fences, and mermaid fences.
- Measurements:
  - `markdown_finalize`: append rich content, call `force_flush_all(...)`, stop there
  - `markdown_warm_draw`: use a finalized trace, warm once, then time repeated draws
- Parameters: `N = [10, 100, 500]`

This prevents "markdown cost" from collapsing parse/finalize and repaint into one noisy number.

## Task 0: Add a Production-Path Simulation Harness

**Files:**
- Create: `crates/spur-tui/examples/react_trace_bench_sim.rs`

- [ ] Add a small example harness that exercises the real draw path with `TestBackend`.
- [ ] The harness must print timings for:
  - warm draw on plain content
  - build plus first draw on a fresh trace
  - one finite streaming run (`append + draw` repeated for a fixed number of frames)
  - markdown finalize and markdown warm draw
- [ ] The history generator must alternate agent ids for historical entries so `append_message(...)` produces multiple `TraceEntry`s.
- [ ] Run:

```bash
cargo run -p spur-tui --example react_trace_bench_sim -- --quick
```

- [ ] Commit:

```bash
git add crates/spur-tui/examples/react_trace_bench_sim.rs
git commit -m "test(spur-tui): add react trace benchmark simulation harness"
```

This harness is not the final statistical benchmark. Its job is to prove the plan is attached to the real production path.

## Task 1: Add Divan Infrastructure

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`
- Create: `crates/spur-tui/benches/react_trace.rs`

- [ ] Add `divan = "0.1.21"` to `[dev-dependencies]`.
- [ ] Register the benchmark target:

```toml
[[bench]]
name = "react_trace"
harness = false
```

- [ ] Create the bench root:

```rust
use divan::AllocProfiler;

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn main() {
    divan::main();
}
```

- [ ] Run a compile-only smoke check:

```bash
cargo bench -p spur-tui --bench react_trace --no-run
```

- [ ] Commit:

```bash
git add crates/spur-tui/Cargo.toml crates/spur-tui/benches/react_trace.rs
git commit -m "chore(spur-tui): add divan react-trace harness"
```

## Task 2: Build Public-API Fixtures

**Files:**
- Modify: `crates/spur-tui/benches/react_trace.rs`

- [ ] Add a `RenderHarness` wrapper that owns a `Terminal<TestBackend>` and exposes one `draw(&mut ReactTrace)` method.
- [ ] Add a plain-trace fixture builder:
  - `ReactTrace::with_kind(...)`
  - alternate agent names across history entries
  - fixed timestamp formatting
  - fixed viewport size
- [ ] Add a rich-markdown fixture builder:
  - headings
  - lists
  - fenced Rust blocks
  - mermaid fences
  - explicit `force_flush_all(...)` path for finalized-content benchmarks
- [ ] Keep mermaid registry empty for v1 so markdown row-building is measured without image-render noise.
- [ ] Run:

```bash
cargo bench -p spur-tui --bench react_trace --no-run
```

- [ ] Commit:

```bash
git add crates/spur-tui/benches/react_trace.rs
git commit -m "test(spur-tui): add react-trace benchmark fixtures"
```

## Task 3: Implement the Bench Suites

**Files:**
- Modify: `crates/spur-tui/benches/react_trace.rs`

- [ ] Implement `warm_draw_plain`.
  - Use `with_inputs(...)` to provide a fresh warmed trace when needed.
  - Use Divan counters so throughput can be read per entry count.
- [ ] Implement `first_draw_plain`.
  - Generate or clone the trace outside the measured closure.
  - Time exactly one first draw on the fresh instance.
- [ ] Implement `stream_append_draw`.
  - Each sample must receive a fresh warmed trace and a fixed chunk payload.
  - Time one append plus one draw.
- [ ] Implement `markdown_finalize`.
  - Time `force_flush_all(...)` on a fresh rich trace.
- [ ] Implement `markdown_warm_draw`.
  - Time repeated draws on finalized rich traces.
- [ ] Run targeted commands:

```bash
cargo bench -p spur-tui --bench react_trace -- warm_draw_plain
cargo bench -p spur-tui --bench react_trace -- first_draw_plain
cargo bench -p spur-tui --bench react_trace -- stream_append_draw
cargo bench -p spur-tui --bench react_trace -- markdown_finalize
cargo bench -p spur-tui --bench react_trace -- markdown_warm_draw
```

- [ ] Commit:

```bash
git add crates/spur-tui/benches/react_trace.rs
git commit -m "bench(spur-tui): add grounded react-trace benchmark suites"
```

## Task 4: Feature-Mode Comparison

The feature comparison must happen across **separate bench invocations**, not a runtime boolean.

- [ ] Run the benchmark suite in the default build:

```bash
cargo bench -p spur-tui --bench react_trace
```

- [ ] Run a non-markdown comparison build where appropriate:

```bash
cargo bench -p spur-tui --bench react_trace --no-default-features
```

- [ ] Record the exact command lines and environment in the benchmark notes.
- [ ] If `AllocProfiler` timing distortion makes absolute latency comparisons confusing, add a second wall-clock-only pass without the profiling allocator.

## Task 5: Report and Decide Whether an Internal Shim Is Needed

- [ ] Summarize the results in a short doc or PR note:
  - warm draw scaling
  - first-draw scaling
  - streaming frame cost
  - markdown finalize cost
  - markdown repaint cost
- [ ] If the production-path results are already stable and actionable, stop here.
- [ ] Only add a hidden internal benchmark-support API if the data suggests `TestBackend` overhead is masking `ReactTrace` itself.

## Success Criteria

- Benchmarks compile and run on the current workspace toolchain (`rust-version = 1.88`).
- The benchmark suite measures the production draw path rather than a fictional `Widget` implementation.
- Streaming benchmarks use comparable per-sample inputs instead of one ever-growing trace.
- Markdown cost is split into finalize/build work vs. repaint work.
- The plan is backed by a runnable simulation harness before the full Divan suite lands.
