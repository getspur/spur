//! Full-emission audit for the user's exact repro flow.
//!
//! Simulates the EXACT iTerm2 cursor behavior (auto-advance by
//! UnicodeWidthStr per print, MoveTo for explicit positioning) and
//! compares the resulting terminal display to ratatui's intended buffer
//! state across the user's repro: Ctrl+O collapse-toggle then scroll up.
//!
//! If terminal display == intended buffer for all frames, the bug is
//! NOT in ratatui's emission and must be elsewhere (iTerm2 font/glyph
//! handling, or a different code path).

#![cfg(feature = "markdown")]

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
use spur_acp::AgentKind;
use spur_tui::components::image_cache::ImageCache;
use spur_tui::components::react_trace::{
    ActStatus, ReactTrace, RenderContext, TraceEntry, TraceKind,
};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

const W: u16 = 100;
const H: u16 = 25;

fn build_realistic_trace() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.append_think("Plan: read files then edit", "10:00".into());
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "ls".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: (0..20)
                    .map(|i| format!("line{:02}.txt", i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t.append_message("OK, here is the analysis result.", "claude", "10:02".into());
    t.append_think("Now I will edit the files.", "10:03".into());
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "cat README".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: (0..15)
                    .map(|i| format!("readme line {}", i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:04".into(),
        markdown: None,
    });
    t.append_message("Done.", "claude", "10:05".into());
    t
}

fn snap(term: &Terminal<TestBackend>) -> Vec<String> {
    let buf = term.backend().buffer();
    (0..H)
        .map(|y| {
            (0..W)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// Apply the diff between two buffers using a SIMULATED iTerm2 cursor that
/// auto-advances by REAL UnicodeWidthStr::width per glyph. Returns the
/// resulting "terminal display" grid.
fn simulate_terminal_after_emission(
    prev_buf: &ratatui::buffer::Buffer,
    next_buf: &ratatui::buffer::Buffer,
    initial_grid: &[String],
) -> Vec<String> {
    // Start with the previous frame's visible state.
    let mut grid: Vec<Vec<String>> = initial_grid
        .iter()
        .map(|row| {
            let mut cells: Vec<String> = row.chars().map(|c| c.to_string()).collect();
            cells.resize(W as usize, " ".to_string());
            cells
        })
        .collect();
    grid.resize(H as usize, vec![" ".to_string(); W as usize]);

    let updates = prev_buf.diff(next_buf);
    let mut last_pos: Option<(u16, u16)> = None;
    let mut cur_x: u16 = 0;
    let mut cur_y: u16 = 0;

    for (x, y, cell) in updates {
        // Mirror ratatui's MoveTo-skip optimization
        let contiguous = matches!(last_pos, Some((px, py)) if x == px + 1 && y == py);
        if !contiguous {
            // Explicit MoveTo — cursor jumps to (x, y).
            cur_x = x;
            cur_y = y;
        }
        // Print the symbol AT THE CURSOR (not necessarily at (x, y)!).
        // This is where desync would manifest: if the cursor isn't at (x, y)
        // due to wide-char auto-advance, the print lands at the wrong column.
        let sym = cell.symbol().to_string();
        if (cur_y as usize) < grid.len() && (cur_x as usize) < W as usize {
            grid[cur_y as usize][cur_x as usize] = sym.clone();
        }
        // Real terminal advances cursor by display width.
        let advance = UnicodeWidthStr::width(sym.as_str()) as u16;
        cur_x = cur_x.saturating_add(advance.max(1));
        last_pos = Some((x, y));
    }

    grid.into_iter().map(|row| row.join("")).collect()
}

fn print_grid(label: &str, grid: &[String]) {
    eprintln!("=== {} ===", label);
    for (y, row) in grid.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
}

fn diff_grids(a: &[String], b: &[String]) -> usize {
    let mut count = 0;
    for (y, (ar, br)) in a.iter().zip(b.iter()).enumerate() {
        if ar != br {
            count += 1;
            eprintln!("  ROW {} DIFF:", y);
            eprintln!("    intended: {}", ar);
            eprintln!("    terminal: {}", br);
        }
    }
    count
}

#[test]
fn full_user_repro_with_real_terminal_simulation() {
    let mut trace = build_realistic_trace();
    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut cache = ImageCache::new();
    let registry = HashMap::new();

    // Initial render in collapsed (default).
    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            mermaid_registry_version: 0,
            picker: None,
            image_cache: &mut cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
            .unwrap();
    }
    let intended_1 = snap(&term);
    let buf_1 = term.backend().buffer().clone();
    print_grid("Intended frame 1 (collapsed)", &intended_1);

    // ── User action: Ctrl+O → expand ──
    trace.toggle_observe_collapsed();
    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            mermaid_registry_version: 0,
            picker: None,
            image_cache: &mut cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
            .unwrap();
    }
    let intended_2 = snap(&term);
    let buf_2 = term.backend().buffer().clone();
    print_grid("Intended frame 2 (expanded after Ctrl+O)", &intended_2);

    // Apply emission to simulated terminal: starting from frame 1's display.
    let terminal_after_2 = simulate_terminal_after_emission(&buf_1, &buf_2, &intended_1);
    print_grid(
        "Terminal display after emission for frame 2",
        &terminal_after_2,
    );

    eprintln!("=== Diff intended_2 vs terminal_after_2 (frame 1 → 2) ===");
    let diffs_12 = diff_grids(&intended_2, &terminal_after_2);
    if diffs_12 > 0 {
        eprintln!(
            ">>> DESYNC: terminal display diverged from intended on {} rows after Ctrl+O <<<",
            diffs_12
        );
    } else {
        eprintln!(">>> Frame 2 emission OK (terminal == intended) <<<");
    }

    // ── User action: scroll up by 5 ──
    trace.scroll_up_by(5);
    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            mermaid_registry_version: 0,
            picker: None,
            image_cache: &mut cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
            .unwrap();
    }
    let intended_3 = snap(&term);
    let buf_3 = term.backend().buffer().clone();
    print_grid("Intended frame 3 (after scroll_up_by(5))", &intended_3);

    let terminal_after_3 = simulate_terminal_after_emission(&buf_2, &buf_3, &terminal_after_2);
    print_grid(
        "Terminal display after emission for frame 3",
        &terminal_after_3,
    );

    eprintln!("=== Diff intended_3 vs terminal_after_3 (frame 2 → 3) ===");
    let diffs_23 = diff_grids(&intended_3, &terminal_after_3);
    if diffs_23 > 0 {
        eprintln!(
            ">>> DESYNC: terminal display diverged from intended on {} rows after scroll <<<",
            diffs_23
        );
    } else {
        eprintln!(">>> Frame 3 emission OK (terminal == intended) <<<");
    }

    // ── Multiple scrolls (the user's "scroll up/down" pattern) ──
    let mut prev_buf = buf_3.clone();
    let mut prev_terminal = terminal_after_3;
    let mut total_desyncs = 0;
    for i in 0..5 {
        trace.scroll_up_by(3);
        {
            let mut ctx = RenderContext {
                mermaid_registry: &registry,
                mermaid_registry_version: 0,
                picker: None,
                image_cache: &mut cache,
            };
            term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
                .unwrap();
        }
        let intended = snap(&term);
        let buf = term.backend().buffer().clone();
        let terminal = simulate_terminal_after_emission(
            &buf,
            &term.backend().buffer().clone(),
            &prev_terminal,
        );
        // Wait — that's wrong. simulate from prev_buf to current buf, applied to prev_terminal.
        let terminal = simulate_terminal_after_emission(&prev_buf, &buf, &prev_terminal);
        eprintln!("=== Scroll iter {} ===", i);
        let d = diff_grids(&intended, &terminal);
        if d > 0 {
            eprintln!(">>> DESYNC at scroll iter {}: {} rows diverged <<<", i, d);
            total_desyncs += d;
        }
        prev_buf = buf;
        prev_terminal = terminal;
    }

    eprintln!(
        "\n=== TOTAL DESYNC COUNT: {} ===",
        total_desyncs + diffs_12 + diffs_23
    );
}
