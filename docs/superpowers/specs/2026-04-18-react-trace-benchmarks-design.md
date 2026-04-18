# ReactTrace Performance Benchmarking Strategy

## Goal
Establish a statistically rigorous, highly isolated benchmarking framework for the `ReactTrace` TUI component to measure and optimize its performance under high-frequency streaming updates, immense scale (thousands of log entries), and expensive rich-text (Markdown/Mermaid) parsing.

## Architecture

We will use **Divan** as the benchmarking framework. Divan provides rapid compile times, clean macro-based parameter matrices, and built-in global memory allocation tracking.

Crucially, we will bypass the `ratatui::Terminal` abstraction layer. Instead of piping into a mock backend (`TestBackend`), we will allocate raw `ratatui::buffer::Buffer` structs and invoke `<ReactTrace as ratatui::widgets::Widget>::render()` directly. This guarantees we are measuring *strictly* the CPU cycles and memory allocations of `ReactTrace`'s layout algorithms, text wrapping logic, and markdown AST generation, completely stripped of physical I/O or terminal emulator noise.

## The Benchmark Matrix

The benchmarks will be located in `crates/spur-tui/benches/react_trace.rs` and will measure three distinct operational vectors:

### 1. Scale Capacity (Cold Render)
**Purpose:** Measure the algorithmic complexity of layout calculation (`last_total_lines` computation, `ScrollAnchor` resolution) when a massive trace is rendered from scratch (e.g. after a terminal resize or initial load).
**Parameter:** History size ($N$) = `[10, 100, 1000, 5000]`.
**Execution:** Clone a generated trace and measure the wall-clock time and allocations of a complete `render` into a fresh `Buffer`.

### 2. High-Frequency Token Streaming (Hot Append)
**Purpose:** Measure the overhead of appending single tokens to an existing trace entry while the terminal `tick` loop is firing rapidly. This is the primary "AI typing" hot path.
**Parameter:** Fixed history size (e.g., 1000 entries).
**Execution:** Pre-render the trace once to populate internal caches (`last_total_lines`, `anchor`), then within the benchmark hot loop, append a tiny string to the latest entry and call `render` on the *same* `Buffer` to simulate incremental frame updates.

### 3. Rich-Content Parsing (Markdown/Mermaid Overhead)
**Purpose:** Quantify the cost of `pulldown-cmark` parsing and virtual row building when rich features are enabled versus disabled.
**Parameter:** Markdown enabled vs. Disabled.
**Execution:** Compare the render time of a 100-item plain-text trace against a 100-item trace densely packed with code blocks, lists, and mermaid stubs.

## Implementation Steps

1. Add `divan` as a `[dev-dependencies]` to `crates/spur-tui/Cargo.toml`.
2. Configure `Cargo.toml` to register the new benchmark (`[[bench]] name = "react_trace" harness = false`).
3. Scaffold the `ReactTrace` mock data generators. Ensure the mocks accurately reflect production `TraceEntry` payloads (e.g., User messages, Tool inputs, Agent outputs).
4. Implement the three benchmark suites defined above using `divan::bench` and `divan::black_box`.
5. Run the initial baseline `cargo bench -p spur-tui` and document the findings.
