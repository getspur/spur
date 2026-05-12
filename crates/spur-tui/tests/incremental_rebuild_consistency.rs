//! Phase 1 grounding: verify that the incremental cache rebuild path
//! produces identical buffer output to a full rebuild for the same
//! trace+width+state, after Ctrl+O collapse-toggle + scroll.
//!
//! If they diverge, we've found a code-side bug: the incremental path
//! leaves stale rows in line_cache that survive into the rendered viewport.

#![cfg(feature = "markdown")]

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
use spur_acp::AgentKind;
use spur_tui::components::image_cache::ImageCache;
use spur_tui::components::react_trace::{
    ActStatus, ReactTrace, RenderContext, TraceEntry, TraceKind,
};
use std::collections::HashMap;

const W: u16 = 100;
const H: u16 = 25;

fn build_trace_with_tools() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);

    // Realistic tool output mix: short summary + long stdout that requires expansion.
    t.append_think("Initial planning step.", "10:00".into());

    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "cargo build --release".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                // Realistic cargo output (long, mixed lengths)
                stdout: (0..30).map(|i| {
                    if i % 5 == 0 {
                        format!("   Compiling crate-{} v0.1.{} (/home/user/very/deep/nested/path/that/might/wrap-{})", i, i, i)
                    } else {
                        format!("warning: unused variable: `var_{}`", i)
                    }
                }).collect::<Vec<_>>().join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });

    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "edit_file".into(),
            family: ToolFamily::Edit,
            input: ToolInputDisplay::Path("src/main.rs".into()),
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::FileRead {
                path: Some("src/main.rs".into()),
                content: (0..25)
                    .map(|i| format!("fn line_{}() {{ /* body */ }}", i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                truncated: false,
            })),
        },
        text: String::new(),
        timestamp: "10:02".into(),
        markdown: None,
    });

    t.append_message("Build done. Edited file.", "claude", "10:03".into());

    t
}

/// Run a render and return (intended buffer, line_cache row count).
fn render_and_capture(
    trace: &mut ReactTrace,
    term: &mut Terminal<TestBackend>,
    cache: &mut ImageCache,
) -> (Vec<String>, ratatui::buffer::Buffer) {
    let registry = HashMap::new();
    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            mermaid_registry_version: 0,
            picker: None,
            image_cache: cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
            .unwrap();
    }
    let buf = term.backend().buffer().clone();
    let snap: Vec<String> = (0..H)
        .map(|y| (0..W).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect();
    (snap, buf)
}

#[test]
fn incremental_rebuild_matches_full_rebuild() {
    // ── Sequence A: incremental path (collapsed → toggle → render → scroll → render) ──
    let mut trace_a = build_trace_with_tools();
    let mut term_a = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut cache_a = ImageCache::new();

    let (_, _) = render_and_capture(&mut trace_a, &mut term_a, &mut cache_a); // collapsed
    trace_a.toggle_observe_collapsed();
    let (_, _) = render_and_capture(&mut trace_a, &mut term_a, &mut cache_a); // expanded full rebuild
    trace_a.scroll_up_by(8);
    let (snap_a_after_scroll, buf_a) = render_and_capture(&mut trace_a, &mut term_a, &mut cache_a);

    // ── Sequence B: fresh trace, same final state, full rebuild only ──
    let mut trace_b = build_trace_with_tools();
    let mut term_b = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut cache_b = ImageCache::new();

    trace_b.toggle_observe_collapsed(); // start as expanded directly
                                        // Force the same anchor as trace_a (Following → scroll up by 8 rows from bottom)
    let (_, _) = render_and_capture(&mut trace_b, &mut term_b, &mut cache_b); // populates last_visible_height etc
    trace_b.scroll_up_by(8);
    let (snap_b, buf_b) = render_and_capture(&mut trace_b, &mut term_b, &mut cache_b);

    eprintln!("=== Sequence A (incremental: collapsed → expand → scroll) ===");
    for (y, row) in snap_a_after_scroll.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    eprintln!("=== Sequence B (fresh: expand → scroll) ===");
    for (y, row) in snap_b.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }

    // Diff cell-by-cell.
    let mut diffs: Vec<(usize, usize, String, String)> = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let a_sym = buf_a[(x, y)].symbol().to_string();
            let b_sym = buf_b[(x, y)].symbol().to_string();
            let a_style = buf_a[(x, y)].style();
            let b_style = buf_b[(x, y)].style();
            if a_sym != b_sym || a_style != b_style {
                diffs.push((
                    y as usize,
                    x as usize,
                    format!("{}|{:?}", a_sym, a_style),
                    format!("{}|{:?}", b_sym, b_style),
                ));
            }
        }
    }

    eprintln!("=== Cell-level diffs A vs B ({} cells) ===", diffs.len());
    for (y, x, a, b) in diffs.iter().take(20) {
        eprintln!("  ({},{}) A: {} | B: {}", y, x, a, b);
    }

    if diffs.is_empty() {
        eprintln!(">>> CONSISTENT: incremental and full rebuild produce identical output <<<");
    } else {
        eprintln!(
            ">>> DIVERGENT: incremental and full rebuild differ on {} cells <<<",
            diffs.len()
        );
    }

    assert!(
        diffs.is_empty(),
        "Incremental rebuild diverged from full rebuild on {} cells — see eprintln output for first 20 diffs",
        diffs.len()
    );
}
