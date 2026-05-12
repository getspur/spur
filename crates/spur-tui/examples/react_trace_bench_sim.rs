//! Production-path simulation harness for the ReactTrace benchmark plan.
//!
//! Run with:
//!
//!     cargo run -p spur-tui --example react_trace_bench_sim -- --quick

use std::hint::black_box;
use std::time::{Duration, Instant};

use ratatui::{backend::TestBackend, Terminal};
use spur_acp::AgentKind;
#[cfg(feature = "markdown")]
use spur_tui::components::markdown_stream::StateLookup;
use spur_tui::components::react_trace::ReactTrace;
#[cfg(feature = "markdown")]
use spur_tui::components::react_trace::RenderContext;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

#[derive(Clone, Copy)]
struct SimConfig {
    plain_entries: usize,
    markdown_entries: usize,
    warm_iters: usize,
    first_draw_iters: usize,
    stream_frames: usize,
}

#[derive(Clone, Copy)]
struct Stats {
    total: Duration,
    min: Duration,
    max: Duration,
}

impl Stats {
    fn new() -> Self {
        Self {
            total: Duration::ZERO,
            min: Duration::MAX,
            max: Duration::ZERO,
        }
    }

    fn record(&mut self, elapsed: Duration) {
        self.total += elapsed;
        self.min = self.min.min(elapsed);
        self.max = self.max.max(elapsed);
    }
}

struct RenderHarness {
    terminal: Terminal<TestBackend>,
    #[cfg(feature = "markdown")]
    mermaid_registry: std::collections::HashMap<
        spur_tui::components::mermaid::MermaidId,
        spur_tui::components::mermaid::MermaidState,
    >,
    #[cfg(feature = "markdown")]
    image_cache: spur_tui::components::image_cache::ImageCache,
}

impl RenderHarness {
    fn new() -> Self {
        let backend = TestBackend::new(WIDTH, HEIGHT);
        let terminal = Terminal::new(backend).expect("test backend must initialize");
        Self {
            terminal,
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            image_cache: spur_tui::components::image_cache::ImageCache::new(),
        }
    }

    fn draw(&mut self, trace: &mut ReactTrace) {
        #[cfg(feature = "markdown")]
        {
            let registry = &self.mermaid_registry;
            let image_cache = &mut self.image_cache;
            self.terminal
                .draw(|frame| {
                    let mut ctx = RenderContext {
                        mermaid_registry: registry,
                        mermaid_registry_version: 0,
                        picker: None,
                        image_cache,
                    };
                    trace.render_with_ctx(frame, frame.area(), &mut ctx, None);
                })
                .expect("draw must succeed");
        }

        #[cfg(not(feature = "markdown"))]
        {
            self.terminal
                .draw(|frame| trace.render(frame, frame.area(), None))
                .expect("draw must succeed");
        }
    }
}

fn main() {
    let quick = std::env::args().any(|arg| arg == "--quick");
    let cfg = if quick {
        SimConfig {
            plain_entries: 200,
            markdown_entries: 40,
            warm_iters: 10,
            first_draw_iters: 5,
            stream_frames: 32,
        }
    } else {
        SimConfig {
            plain_entries: 1_000,
            markdown_entries: 100,
            warm_iters: 50,
            first_draw_iters: 20,
            stream_frames: 128,
        }
    };

    println!("ReactTrace benchmark simulation");
    println!(
        "viewport={}x{} markdown_feature={} mode={}",
        WIDTH,
        HEIGHT,
        cfg!(feature = "markdown"),
        if quick { "quick" } else { "full" }
    );

    simulate_warm_draw_plain(cfg);
    simulate_first_draw_plain(cfg);
    simulate_stream_append_draw(cfg);
    #[cfg(feature = "markdown")]
    simulate_markdown(cfg);
}

fn simulate_warm_draw_plain(cfg: SimConfig) {
    let mut harness = RenderHarness::new();
    let mut trace = build_plain_trace(cfg.plain_entries);
    harness.draw(&mut trace);

    let stats = measure(cfg.warm_iters, || {
        harness.draw(&mut trace);
    });
    print_stats(
        "warm_draw_plain",
        cfg.warm_iters,
        stats,
        format!("entries={}", trace.entry_count()),
    );
}

fn simulate_first_draw_plain(cfg: SimConfig) {
    let mut harness = RenderHarness::new();

    let stats = measure(cfg.first_draw_iters, || {
        let mut trace = build_plain_trace(cfg.plain_entries);
        harness.draw(&mut trace);
        black_box(trace.entry_count());
    });
    print_stats(
        "build_plus_first_draw_plain",
        cfg.first_draw_iters,
        stats,
        format!("entries={}", cfg.plain_entries),
    );
}

fn simulate_stream_append_draw(cfg: SimConfig) {
    let mut harness = RenderHarness::new();
    let mut trace = build_plain_trace(cfg.plain_entries);
    let streaming_agent = "stream-live";
    trace.append_message("stream bootstrap", streaming_agent, timestamp(9_000));
    harness.draw(&mut trace);

    let start = Instant::now();
    for i in 0..cfg.stream_frames {
        trace.append_message(" +token", streaming_agent, timestamp(9_001 + i));
        harness.draw(&mut trace);
    }
    let elapsed = start.elapsed();
    println!(
        "stream_append_draw total={:.3}ms mean/frame={:.3}ms frames={} tail_entries={}",
        elapsed.as_secs_f64() * 1_000.0,
        elapsed.as_secs_f64() * 1_000.0 / cfg.stream_frames as f64,
        cfg.stream_frames,
        trace.entry_count()
    );
}

#[cfg(feature = "markdown")]
fn simulate_markdown(cfg: SimConfig) {
    let finalized = build_markdown_trace(cfg.markdown_entries, true);
    let mut harness = RenderHarness::new();

    let finalize_stats = measure(cfg.first_draw_iters, || {
        let mut trace = build_markdown_trace(cfg.markdown_entries, false);
        let _ = trace.force_flush_all(&StateLookup::empty());
        black_box(trace.entry_count());
    });
    print_stats(
        "markdown_finalize",
        cfg.first_draw_iters,
        finalize_stats,
        format!("entries={}", finalized.entry_count()),
    );

    let first_draw_stats = measure(cfg.first_draw_iters, || {
        let mut trace = build_markdown_trace(cfg.markdown_entries, true);
        harness.draw(&mut trace);
        black_box(trace.entry_count());
    });
    print_stats(
        "build_finalize_draw_markdown",
        cfg.first_draw_iters,
        first_draw_stats,
        format!("entries={}", finalized.entry_count()),
    );

    let mut warm_trace = build_markdown_trace(cfg.markdown_entries, true);
    harness.draw(&mut warm_trace);
    let warm_stats = measure(cfg.warm_iters, || {
        harness.draw(&mut warm_trace);
    });
    print_stats(
        "warm_draw_markdown",
        cfg.warm_iters,
        warm_stats,
        format!("entries={}", warm_trace.entry_count()),
    );
}

fn measure(iterations: usize, mut f: impl FnMut()) -> Stats {
    let mut stats = Stats::new();
    for _ in 0..iterations {
        let start = Instant::now();
        f();
        stats.record(start.elapsed());
    }
    stats
}

fn print_stats(label: &str, iterations: usize, stats: Stats, detail: String) {
    let mean_ms = stats.total.as_secs_f64() * 1_000.0 / iterations as f64;
    let min_ms = stats.min.as_secs_f64() * 1_000.0;
    let max_ms = stats.max.as_secs_f64() * 1_000.0;
    println!(
        "{label} mean={mean_ms:.3}ms min={min_ms:.3}ms max={max_ms:.3}ms iterations={iterations} {detail}"
    );
}

fn build_plain_trace(entries: usize) -> ReactTrace {
    let mut trace = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    for i in 0..entries {
        trace.append_message(&plain_payload(i), alternating_agent(i), timestamp(i));
    }
    trace
}

#[cfg(feature = "markdown")]
fn build_markdown_trace(entries: usize, finalize: bool) -> ReactTrace {
    let mut trace = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    for i in 0..entries {
        trace.append_message(&markdown_payload(i), alternating_agent(i), timestamp(i));
    }
    if finalize {
        let _ = trace.force_flush_all(&StateLookup::empty());
    }
    trace
}

fn alternating_agent(i: usize) -> &'static str {
    if i.is_multiple_of(2) {
        "claude"
    } else {
        "codex"
    }
}

fn timestamp(i: usize) -> String {
    let minute = (i / 60) % 60;
    let second = i % 60;
    format!("10:{minute:02}:{second:02}")
}

fn plain_payload(i: usize) -> String {
    format!(
        "Plain entry {i:04} with enough text to wrap across the viewport. \
         This simulates steady log output and keeps row-building work realistic."
    )
}

#[cfg(feature = "markdown")]
fn markdown_payload(i: usize) -> String {
    format!(
        "# Heading {i}\n\n\
         - first bullet\n\
         - second bullet\n\n\
         ```rust\n\
         fn synthetic_{i}() {{\n\
             println!(\"trace bench\");\n\
         }}\n\
         ```\n\n\
         Paragraph {i} keeps the markdown pipeline busy with wrapping, list handling, \
         and fenced code blocks.\n\n\
         ```mermaid\n\
         graph TD\n\
         A[Start {i}] --> B[Work]\n\
         B --> C[Done]\n\
         ```\n"
    )
}
