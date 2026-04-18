# ReactTrace Performance Benchmarking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish a statistically rigorous Divan benchmarking suite for the `ReactTrace` TUI component to measure algorithmic complexity under massive scale, high-frequency updates, and rich-text parsing.

**Architecture:** Bypassing `ratatui::Terminal` entirely, we allocate raw `ratatui::buffer::Buffer` structs and invoke `<ReactTrace as ratatui::widgets::Widget>::render()` directly. This strictly isolates CPU/Memory overhead from terminal I/O. 

**Tech Stack:** Rust, Ratatui, Divan (Benchmarking framework)

---

### Task 1: Setup Divan Infrastructure

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`
- Create: `crates/spur-tui/benches/react_trace.rs`

- [ ] **Step 1: Add divan dependency to Cargo.toml**
Add `divan` under `[dev-dependencies]` and configure the benchmark harness.

```toml
# In crates/spur-tui/Cargo.toml, append:

[dev-dependencies]
divan = "0.1.14"

[[bench]]
name = "react_trace"
harness = false
```

- [ ] **Step 2: Create the bench root file**

```rust
// crates/spur-tui/benches/react_trace.rs
use divan::AllocProfiler;

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn main() {
    divan::main();
}
```

- [ ] **Step 3: Run baseline check**

Run: `cargo bench -p spur-tui`
Expected: Compiles successfully and outputs "No benchmarks found" or an empty table.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/Cargo.toml crates/spur-tui/benches/react_trace.rs
git commit -m "chore(spur-tui): add divan benchmark harness"
```

---

### Task 2: Build the TraceEntry Mock Generator

**Files:**
- Modify: `crates/spur-tui/benches/react_trace.rs`

- [ ] **Step 1: Add the mock generator function**
We need a robust way to generate `N` items of `TraceEntry`.

```rust
// In crates/spur-tui/benches/react_trace.rs, below main():
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use spur_tui::components::react_trace::{ReactTrace, types::{TraceEntry, TraceKind}};
use spur_tui::components::AgentKind;

fn generate_mock_trace(n: usize, use_markdown: bool) -> ReactTrace {
    let mut trace = ReactTrace::new();
    // Simulate agent kind assignment
    trace.set_agent_kind(AgentKind::SpurCore);

    for i in 0..n {
        let text = if use_markdown {
            format!("Here is some text.\n```rust\nfn stub_{}() {{}}\n```\nAnd a list:\n- Item 1\n- Item 2", i)
        } else {
            format!("Plain text log entry number {} with no special formatting just wrapping length.", i)
        };

        trace.push_entry(TraceEntry {
            kind: TraceKind::AgentMessage,
            text,
            timestamp: "12:00:00".to_string(),
            #[cfg(feature = "markdown")]
            markdown: if use_markdown {
                Some(spur_tui::components::markdown_stream::MarkdownStream::new()) // Need empty stub
            } else {
                None
            },
        });
    }
    trace
}
```

- [ ] **Step 2: Fix compilation errors if any**
Note: `MarkdownStream::new()` might not exist or might need context. We'll refine the `TraceEntry` construction directly based on `ReactTrace::push_entry` or manual `Vec` pushing if `push_entry` is private. Looking at `mod.rs`, `entries` is `pub(super)`, so we might need a test helper or use public `push_*` methods.
*Self-correction for step 1 code*: Use the public API to push entries.

```rust
// Replace the generate_mock_trace body with public API calls:
fn generate_mock_trace(n: usize, use_markdown: bool) -> ReactTrace {
    let mut trace = ReactTrace::new();
    trace.set_agent_kind(AgentKind::SpurCore);

    for i in 0..n {
        let text = if use_markdown {
            format!("Here is some text.\n```rust\nfn stub_{}() {{}}\n```\nAnd a list:\n- Item 1\n- Item 2", i)
        } else {
            format!("Plain text log entry number {} with no special formatting just wrapping length.", i)
        };
        
        // Pushing text token by token triggers the internal TraceEntry construction
        // For simplicity in a benchmark, pushing a whole block as a UserMessage or pushing token by token to AgentMessage
        trace.push_agent_token(&text);
        trace.finalize_agent_message(); 
    }
    trace
}
```

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/benches/react_trace.rs
git commit -m "test(spur-tui): add benchmark mock generator"
```

---

### Task 3: Implement Scale Capacity (Cold Render) Benchmark

**Files:**
- Modify: `crates/spur-tui/benches/react_trace.rs`

- [ ] **Step 1: Write the cold render benchmark**

```rust
// In crates/spur-tui/benches/react_trace.rs
#[divan::bench(args = [10, 100, 1000, 5000])]
fn bench_cold_render(bencher: divan::Bencher, n: usize) {
    let trace = generate_mock_trace(n, false);
    let area = Rect::new(0, 0, 100, 40);
    
    bencher.bench_local(
        || {
            let mut buf = Buffer::empty(area);
            // We clone the trace inside the bench closure, but we want to measure *only* the render.
            // divan allows setup -> bench.
            let trace_clone = trace.clone();
            (trace_clone, buf)
        },
        |(mut trace_clone, mut buf)| {
            trace_clone.render(area, &mut buf);
            divan::black_box(buf);
        }
    );
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo bench -p spur-tui --bench react_trace bench_cold_render`
Expected: PASS, showing times and allocations for N=10, 100, 1000, 5000.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/benches/react_trace.rs
git commit -m "bench(spur-tui): add scale capacity cold render benchmark"
```

---

### Task 4: Implement High-Frequency Token Streaming (Hot Append) Benchmark

**Files:**
- Modify: `crates/spur-tui/benches/react_trace.rs`

- [ ] **Step 1: Write the hot append benchmark**

```rust
// In crates/spur-tui/benches/react_trace.rs
#[divan::bench]
fn bench_hot_append(bencher: divan::Bencher) {
    let mut trace = generate_mock_trace(1000, false);
    let area = Rect::new(0, 0, 100, 40);
    let mut buf = Buffer::empty(area);
    
    // First render to warm up caches (like last_total_lines)
    trace.render(area, &mut buf);

    bencher.bench_local(
        || {
            // No setup needed per iteration besides tracking the buffer
            buf.clone()
        },
        |mut cloned_buf| {
            // Simulate receiving a token over the channel
            trace.push_agent_token("a");
            trace.render(area, &mut cloned_buf);
            divan::black_box(cloned_buf);
        }
    );
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo bench -p spur-tui --bench react_trace bench_hot_append`
Expected: PASS, showing fast times and minimal allocations per tick.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/benches/react_trace.rs
git commit -m "bench(spur-tui): add high-frequency token streaming benchmark"
```

---

### Task 5: Implement Rich-Content (Markdown) Benchmark

**Files:**
- Modify: `crates/spur-tui/benches/react_trace.rs`

- [ ] **Step 1: Write the markdown overhead benchmark**

```rust
// In crates/spur-tui/benches/react_trace.rs
#[divan::bench(args = [true, false])]
fn bench_markdown_overhead(bencher: divan::Bencher, use_markdown: bool) {
    let trace = generate_mock_trace(100, use_markdown);
    let area = Rect::new(0, 0, 100, 40);
    
    bencher.bench_local(
        || {
            let mut buf = Buffer::empty(area);
            let trace_clone = trace.clone();
            (trace_clone, buf)
        },
        |(mut trace_clone, mut buf)| {
            trace_clone.render(area, &mut buf);
            divan::black_box(buf);
        }
    );
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo bench -p spur-tui --bench react_trace bench_markdown_overhead`
Expected: PASS, showing a clear delta between `true` (markdown) and `false` (plain text).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/benches/react_trace.rs
git commit -m "bench(spur-tui): add markdown parsing overhead benchmark"
```
