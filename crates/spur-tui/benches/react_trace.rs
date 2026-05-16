use divan::{counter::ItemsCount, Bencher};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::AgentKind;
#[cfg(feature = "markdown")]
use spur_tui::components::markdown_stream::StateLookup;
use spur_tui::components::react_trace::ReactTrace;
#[cfg(feature = "markdown")]
use spur_tui::components::react_trace::RenderContext;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;

const PLAIN_ENTRY_COUNTS: &[usize] = &[10, 100, 1_000, 5_000];
#[cfg(feature = "markdown")]
const MARKDOWN_ENTRY_COUNTS: &[usize] = &[10, 100, 500];
const STREAM_CASES: &[StreamCase] = &[
    StreamCase {
        entries: 100,
        chunk_tokens: 1,
    },
    StreamCase {
        entries: 100,
        chunk_tokens: 8,
    },
    StreamCase {
        entries: 100,
        chunk_tokens: 32,
    },
    StreamCase {
        entries: 1_000,
        chunk_tokens: 1,
    },
    StreamCase {
        entries: 1_000,
        chunk_tokens: 8,
    },
    StreamCase {
        entries: 1_000,
        chunk_tokens: 32,
    },
];

fn main() {
    divan::main();
}

#[derive(Clone, Copy, Debug)]
struct StreamCase {
    entries: usize,
    chunk_tokens: usize,
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

struct DrawInput {
    harness: RenderHarness,
    trace: ReactTrace,
}

struct StreamInput {
    harness: RenderHarness,
    trace: ReactTrace,
    chunk: String,
}

#[divan::bench(args = PLAIN_ENTRY_COUNTS)]
fn warm_draw_plain(bencher: Bencher, entries: usize) {
    let mut input = warm_plain_input(entries);
    bencher
        .counter(ItemsCount::new(entries))
        .bench_local(move || {
            input.harness.draw(&mut input.trace);
            divan::black_box(input.trace.entry_count());
        });
}

#[divan::bench(args = PLAIN_ENTRY_COUNTS)]
fn first_draw_plain(bencher: Bencher, entries: usize) {
    bencher
        .with_inputs(|| DrawInput {
            harness: RenderHarness::new(),
            trace: build_plain_trace(entries),
        })
        .counter(ItemsCount::new(entries))
        .bench_local_refs(|input| {
            input.harness.draw(&mut input.trace);
            divan::black_box(input.trace.entry_count());
        });
}

#[divan::bench(args = STREAM_CASES)]
fn stream_append_draw(bencher: Bencher, case: StreamCase) {
    bencher
        .with_inputs(|| warm_stream_input(case))
        .counter(ItemsCount::new(case.entries))
        .bench_local_refs(|input| {
            input
                .trace
                .append_message(&input.chunk, "stream-live", timestamp(10_000));
            input.harness.draw(&mut input.trace);
            divan::black_box(input.trace.entry_count());
        });
}

#[cfg(feature = "markdown")]
#[divan::bench(args = MARKDOWN_ENTRY_COUNTS)]
fn markdown_finalize(bencher: Bencher, entries: usize) {
    bencher
        .with_inputs(|| build_markdown_trace(entries, false))
        .counter(ItemsCount::new(entries))
        .bench_local_refs(|trace| {
            let states = StateLookup::empty();
            let fences = trace.force_flush_all(&states);
            divan::black_box(fences.len());
            divan::black_box(trace.entry_count());
        });
}

#[cfg(feature = "markdown")]
#[divan::bench(args = MARKDOWN_ENTRY_COUNTS)]
fn markdown_warm_draw(bencher: Bencher, entries: usize) {
    let mut input = warm_markdown_input(entries);
    bencher
        .counter(ItemsCount::new(entries))
        .bench_local(move || {
            input.harness.draw(&mut input.trace);
            divan::black_box(input.trace.entry_count());
        });
}

fn warm_plain_input(entries: usize) -> DrawInput {
    let mut input = DrawInput {
        harness: RenderHarness::new(),
        trace: build_plain_trace(entries),
    };
    input.harness.draw(&mut input.trace);
    input
}

fn warm_stream_input(case: StreamCase) -> StreamInput {
    let mut trace = build_plain_trace(case.entries);
    trace.append_message("stream bootstrap", "stream-live", timestamp(9_000));
    let mut harness = RenderHarness::new();
    harness.draw(&mut trace);
    StreamInput {
        harness,
        trace,
        chunk: stream_chunk(case.chunk_tokens),
    }
}

#[cfg(feature = "markdown")]
fn warm_markdown_input(entries: usize) -> DrawInput {
    let mut input = DrawInput {
        harness: RenderHarness::new(),
        trace: build_markdown_trace(entries, true),
    };
    input.harness.draw(&mut input.trace);
    input
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
        let states = StateLookup::empty();
        let _ = trace.force_flush_all(&states);
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

fn stream_chunk(chunk_tokens: usize) -> String {
    " +token".repeat(chunk_tokens)
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
