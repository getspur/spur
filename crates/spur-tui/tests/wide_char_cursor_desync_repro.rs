//! Phase 1 grounding for the wide-character cursor-desync hypothesis.
//!
//! Hypothesis: ratatui's crossterm backend skips `MoveTo` for spatially
//! contiguous diff cells (`backend/crossterm.rs:162-166`). It relies on the
//! terminal auto-advancing cursor by 1 per glyph. For EAW=W chars (🧠, ✉,
//! 📊, ⏳), real terminals advance the cursor by 2 — desyncing ratatui's
//! cursor model from the actual cursor. Subsequent contiguous diff cells
//! land at column +1 from intended.
//!
//! Repro: simulate the terminal as a backend that auto-advances by the
//! grapheme's display width. Compare the resulting "terminal" buffer with
//! the buffer ratatui *intended* to draw.

#![cfg(feature = "markdown")]

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::AgentKind;
use spur_tui::components::image_cache::ImageCache;
use spur_tui::components::react_trace::{ReactTrace, RenderContext};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

const W: u16 = 60;
const H: u16 = 8;

/// Snapshot a TestBackend's buffer as Vec<String> (one row per line, each
/// cell's `.symbol()` joined).
fn snapshot(term: &Terminal<TestBackend>) -> Vec<String> {
    let buf = term.backend().buffer();
    (0..H)
        .map(|y| {
            (0..W)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// Reproduce the ANSI emission ratatui would do for a buffer transition.
///
/// Returns a Vec<(action, x, y, symbol)> where action is "move" (explicit
/// MoveTo) or "print" (Print contiguous).
///
/// Mirrors `ratatui-0.29 src/backend/crossterm.rs:151-191`.
fn simulate_emission(
    prev: &ratatui::buffer::Buffer,
    curr: &ratatui::buffer::Buffer,
) -> Vec<(&'static str, u16, u16, String)> {
    let updates = prev.diff(curr);
    let mut out = Vec::new();
    let mut last_pos: Option<(u16, u16)> = None;
    for (x, y, cell) in updates {
        let contiguous = matches!(last_pos, Some((px, py)) if x == px + 1 && y == py);
        if !contiguous {
            out.push(("move", x, y, String::new()));
        }
        out.push(("print", x, y, cell.symbol().to_string()));
        last_pos = Some((x, y));
    }
    out
}

/// "Apply" a sequence of (action, x, y, symbol) ops to a fake terminal that
/// auto-advances the cursor by `UnicodeWidthStr::width(symbol)` per print —
/// the way a compliant terminal actually behaves.
fn apply_emission_with_real_advance(
    width: u16,
    height: u16,
    initial: &[String],
    ops: &[(&'static str, u16, u16, String)],
) -> Vec<String> {
    let mut grid: Vec<Vec<String>> = initial
        .iter()
        .map(|row| {
            let mut cells: Vec<String> = row.chars().map(|c| c.to_string()).collect();
            cells.resize(width as usize, " ".to_string());
            cells
        })
        .collect();
    grid.resize(height as usize, vec![" ".to_string(); width as usize]);

    let mut cur_x: u16 = 0;
    let mut cur_y: u16 = 0;

    for (action, x, y, sym) in ops {
        match *action {
            "move" => {
                cur_x = *x;
                cur_y = *y;
            }
            "print" => {
                // Real terminals: write the symbol at the current cursor
                // (NOT at (x, y) — ratatui assumes the cursor is at (x, y),
                // but we're testing whether that assumption holds).
                if (cur_y as usize) < grid.len() && (cur_x as usize) < width as usize {
                    grid[cur_y as usize][cur_x as usize] = sym.clone();
                }
                let advance = UnicodeWidthStr::width(sym.as_str()) as u16;
                cur_x = cur_x.saturating_add(advance.max(1));
            }
            _ => {}
        }
    }

    grid.into_iter().map(|row| row.join("")).collect()
}

#[test]
fn wide_char_cursor_desync_simulation() {
    let mut trace = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);

    // Frame 1: just narrow chars in a tight row
    trace.append_message("hello world this is narrow only", "claude", "10:00".into());

    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut cache = ImageCache::new();
    let registry = HashMap::new();

    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: None,
            image_cache: &mut cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
            .unwrap();
    }
    let snap_frame1 = snapshot(&term);
    let buf_frame1 = term.backend().buffer().clone();

    eprintln!("=== Frame 1 (narrow only) ===");
    for (y, row) in snap_frame1.iter().enumerate() {
        eprintln!("  {:1} | {}", y, row);
    }

    // Frame 2: insert a Think entry (🧠 BRAIN, EAW=W) at the top.
    // Mark dirty to force rebuild.
    trace.append_think("brain entry with wide glyph leading", "10:01".into());

    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: None,
            image_cache: &mut cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
            .unwrap();
    }
    let snap_frame2 = snapshot(&term);
    let buf_frame2 = term.backend().buffer().clone();

    eprintln!("=== Frame 2 (with 🧠 wide char) ===");
    for (y, row) in snap_frame2.iter().enumerate() {
        eprintln!("  {:1} | {}", y, row);
    }

    // Now simulate the ANSI emission for the transition Frame 1 → Frame 2,
    // applied to a fake terminal that auto-advances cursor by REAL char width.
    let ops = simulate_emission(&buf_frame1, &buf_frame2);
    eprintln!(
        "=== Emission ops for frame1 → frame2 ({} ops) ===",
        ops.len()
    );
    for (i, (action, x, y, sym)) in ops.iter().enumerate() {
        let display_sym = if sym.is_empty() {
            "<move>".to_string()
        } else {
            format!("{:?} (width {})", sym, UnicodeWidthStr::width(sym.as_str()))
        };
        eprintln!("  {:3} | {:5} ({:2},{:2}) {}", i, action, x, y, display_sym);
    }

    let real_terminal = apply_emission_with_real_advance(W, H, &snap_frame1, &ops);

    eprintln!("=== What real terminal would display after emission ===");
    for (y, row) in real_terminal.iter().enumerate() {
        eprintln!("  {:1} | {}", y, row);
    }

    eprintln!("=== Diff (intended vs real-terminal) ===");
    let mut desyncs = 0;
    for (y, (intended, real)) in snap_frame2.iter().zip(real_terminal.iter()).enumerate() {
        if intended != real {
            eprintln!("  ROW {} DESYNC:", y);
            eprintln!("    intended: {}", intended);
            eprintln!("    real:     {}", real);
            desyncs += 1;
        }
    }

    if desyncs > 0 {
        eprintln!(
            "\n>>> WIDE-CHAR CURSOR DESYNC CONFIRMED: {} rows differ from intended <<<",
            desyncs
        );
    } else {
        eprintln!("\n>>> No desync detected in this scenario <<<");
    }
}
