use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::AgentKind;
use spur_tui::components::react_trace::ReactTrace;

fn bench_append_message(c: &mut Criterion) {
    c.bench_function("append_message_single", |b| {
        let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
        b.iter(|| {
            trace.append_message(black_box("a short streaming chunk"), "bot", "12:00".into());
        });
    });
}

fn bench_render_compact_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_compact_cold");
    for entries in [500usize, 2000, 5000] {
        for width in [40u16, 80, 120] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{}_entries_w{}", entries, width)),
                &(entries, width),
                |b, &(n, w)| {
                    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
                    // Alternate think/message to produce distinct entries
                    // (append_message coalesces same-agent chunks).
                    for i in 0..n {
                        if i % 2 == 0 {
                            trace.append_think(&format!("t-{}", i), "12:00".into());
                        } else {
                            trace.append_message(&format!("m-{}", i), "bot", "12:00".into());
                        }
                    }
                    let mut term = Terminal::new(TestBackend::new(w, 24)).unwrap();
                    b.iter(|| {
                        // Force cold cache on every iter.
                        trace.drop_compact_cache();
                        term.draw(|f| trace.render_compact(f, Rect::new(0, 0, w, 24))).unwrap();
                    });
                },
            );
        }
    }
    group.finish();
}

fn bench_render_compact_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_compact_cache_hit");
    for entries in [500usize, 2000, 5000] {
        group.bench_with_input(BenchmarkId::from_parameter(entries), &entries, |b, &n| {
            let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
            for i in 0..n {
                if i % 2 == 0 {
                    trace.append_think(&format!("t-{}", i), "12:00".into());
                } else {
                    trace.append_message(&format!("m-{}", i), "bot", "12:00".into());
                }
            }
            let mut term = Terminal::new(TestBackend::new(40, 24)).unwrap();
            // Warm the cache once.
            term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            b.iter(|| {
                term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            });
        });
    }
    group.finish();
}

fn bench_render_compact_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_compact_incremental");
    for entries in [500usize, 2000, 5000] {
        group.bench_with_input(BenchmarkId::from_parameter(entries), &entries, |b, &n| {
            let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
            for i in 0..n {
                if i % 2 == 0 {
                    trace.append_think(&format!("t-{}", i), "12:00".into());
                } else {
                    trace.append_message(&format!("m-{}", i), "bot", "12:00".into());
                }
            }
            let mut term = Terminal::new(TestBackend::new(40, 24)).unwrap();
            term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            let mut counter = 0usize;
            b.iter(|| {
                // Alternate kinds per iter so each append creates a new entry.
                if counter.is_multiple_of(2) {
                    trace.append_user_message(&format!("u-{}", counter), "12:01".into());
                } else {
                    trace.append_think(&format!("t-new-{}", counter), "12:01".into());
                }
                counter += 1;
                term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_append_message,
    bench_render_compact_cold,
    bench_render_compact_hit,
    bench_render_compact_incremental
);
criterion_main!(benches);
