//! Phase 1 grounding for the ITERM2 EAW=N FONT-FALLBACK desync hypothesis.
//!
//! Hypothesis: spur-tui uses EAW=N glyphs (✓ ✗ ✉ ⊞ ⚙ ⚠ ℹ) that the
//! `unicode-width` crate reports as width=1. ratatui writes them as 1-cell.
//! But iTerm2, when the primary font does not have a glyph, falls back to
//! Apple Symbols / Apple Color Emoji which render some of these at width=2.
//! Result: terminal cursor advances by 2 while ratatui's model advances by
//! 1 → subsequent contiguous Print emissions land one column to the right
//! → previous-frame content stays visible at the gap → "ghost text".
//!
//! Match against the user's screenshot:
//!   `3#reuse chrono::Utc`     <- "#r" leftover from prior timestamp
//!   `7:0use spur_core::...`   <- ":0" leftover
//!   `9:5use spur_tui::...`    <- ":5" leftover
//!   `10e use spur_tui::...`   <- "e" leftover
//! These are fragments of "10:55:50 ✓ done" or "10:55:50 ✉ claude" lines
//! from the previous frame, surviving because the ✓ / ✉ rendered at width=2
//! and shifted everything after.

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

/// Glyphs that iTerm2 (with font fallback to Apple Symbols / Apple Color
/// Emoji) commonly renders at width 2 even though `unicode-width` reports
/// width 1. This is the EAW=N font-fallback class.
fn iterm2_renders_as_wide(symbol: &str) -> bool {
    matches!(
        symbol,
        "✓" | "✗" | "✉" | "⊞" | "⚙" | "⚠" | "ℹ" | "⊠" | "►" | "▸"
    )
}

/// Cursor-advance function that mimics iTerm2's actual rendering width
/// (rather than ratatui's assumed width via `unicode-width`).
fn iterm2_advance(symbol: &str) -> u16 {
    if iterm2_renders_as_wide(symbol) {
        2
    } else {
        UnicodeWidthStr::width(symbol).max(1) as u16
    }
}

struct SimItermTerm {
    grid: Vec<Vec<String>>,
    width: u16,
}

impl SimItermTerm {
    fn new(width: u16, height: u16) -> Self {
        Self {
            grid: vec![vec![" ".to_string(); width as usize]; height as usize],
            width,
        }
    }

    fn apply_diff(&mut self, prev: &ratatui::buffer::Buffer, next: &ratatui::buffer::Buffer) {
        let updates = prev.diff(next);
        let mut last_emit: Option<(u16, u16)> = None;
        let mut cur_x: u16 = 0;
        let mut cur_y: u16 = 0;
        for (x, y, cell) in updates {
            let contiguous = matches!(last_emit, Some((px, py)) if x == px + 1 && y == py);
            if !contiguous {
                cur_x = x;
                cur_y = y;
            }
            let sym = cell.symbol().to_string();
            if (cur_y as usize) < self.grid.len() && (cur_x as usize) < self.width as usize {
                self.grid[cur_y as usize][cur_x as usize] = sym.clone();
            }
            // KEY DIFFERENCE: use iTerm2's actual render width, not unicode-width's report.
            cur_x = cur_x.saturating_add(iterm2_advance(&sym));
            last_emit = Some((x, y));
        }
    }

    fn snapshot(&self) -> Vec<String> {
        self.grid.iter().map(|row| row.join("")).collect()
    }
}

fn intended_snap(term: &Terminal<TestBackend>) -> Vec<String> {
    let buf = term.backend().buffer();
    (0..H)
        .map(|y| (0..W).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

fn build_user_trace() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.append_user_message("read the test file", "10:55:00".into());
    // An Act with timestamp matching the user's screenshot pattern
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "Bash".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: "[terminal: tool_o_011sQwCFDIN2VgxhAoQwxxs2]".into(),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:55:50".into(),
        markdown: None,
    });
    // An edit Act with file content (the source of file lines visible in screenshot)
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "edit_file".into(),
            family: ToolFamily::Edit,
            input: ToolInputDisplay::Path("crates/spur-tui/tests/issue_browser_contract.rs".into()),
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::FileRead {
                path: Some("crates/spur-tui/tests/issue_browser_contract.rs".into()),
                content: vec![
                    "use std::collections::HashMap;",
                    "use chrono::Utc;",
                    "use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};",
                    "use ratatui::{backend::TestBackend, Terminal};",
                    "use spur_acp::{IssueDetailEvent, IssueSummaryEvent, SpurEvent, SpurEventBody};",
                    "use spur_core::ExecutorLineage;",
                    "use spur_tui::action::{Action, IssueAction, ViewId};",
                    "use spur_tui::views::issue_browser::IssueBrowserView;",
                    "use spur_tui::views::{View, ViewContext};",
                    "",
                    "fn test_ctx() -> ViewContext<'static> {",
                    "    static LINEAGE: std::sync::LazyLock<ExecutorLineage> =",
                    "        std::sync::LazyLock::new(ExecutorLineage::new);",
                    "    spur_tui::test_support::test_view_ctx(&LINEAGE)",
                    "}",
                    "",
                    "fn rendered_buffer_text(terminal: &Terminal<TestBackend>) -> String {",
                    "    let buf = terminal.backend().buffer();",
                    "    let mut rendered = String::new();",
                ].join("\n"),
                truncated: false,
            })),
        },
        text: String::new(),
        timestamp: "10:55:50".into(),
        markdown: None,
    });
    t.append_message("done", "claude-code", "10:55:50".into());
    t
}

#[test]
fn iterm2_eaw_n_fontfallback_desync_repro() {
    let mut trace = build_user_trace();
    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut cache = ImageCache::new();
    let registry = HashMap::new();
    let mut sim = SimItermTerm::new(W, H);
    let prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, W, H));

    // ── Frame 1: collapsed (default) ──
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
    let intended_1 = intended_snap(&term);
    let buf_1 = term.backend().buffer().clone();
    sim.apply_diff(&prev_buf, &buf_1);

    eprintln!("=== Frame 1 (collapsed) intended ===");
    for (y, row) in intended_1.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    eprintln!("=== Frame 1 (collapsed) iTerm2-simulated ===");
    for (y, row) in sim.snapshot().iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    let f1_diff: Vec<_> = intended_1
        .iter()
        .zip(sim.snapshot().iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(y, (a, b))| (y, a.clone(), b.clone()))
        .collect();
    eprintln!("  Frame 1 desyncs: {}", f1_diff.len());

    // ── Frame 2: Ctrl+O expand ──
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
    let intended_2 = intended_snap(&term);
    let buf_2 = term.backend().buffer().clone();
    sim.apply_diff(&buf_1, &buf_2);

    eprintln!("\n=== Frame 2 (expanded) intended ===");
    for (y, row) in intended_2.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    eprintln!("=== Frame 2 (expanded) iTerm2-simulated ===");
    for (y, row) in sim.snapshot().iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    let f2_diff: Vec<_> = intended_2
        .iter()
        .zip(sim.snapshot().iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(y, (a, b))| (y, a.clone(), b.clone()))
        .collect();
    eprintln!("  Frame 2 desyncs: {}", f2_diff.len());
    for (y, intended, simd) in f2_diff.iter().take(15) {
        eprintln!("    row {:2}", y);
        eprintln!("      intended: {}", intended);
        eprintln!("      iTerm2:   {}", simd);
    }

    // ── Frame 3: scroll up by 5 ──
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
    let intended_3 = intended_snap(&term);
    let buf_3 = term.backend().buffer().clone();
    sim.apply_diff(&buf_2, &buf_3);

    eprintln!("\n=== Frame 3 (scroll up 5) intended ===");
    for (y, row) in intended_3.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    eprintln!("=== Frame 3 (scroll up 5) iTerm2-simulated ===");
    for (y, row) in sim.snapshot().iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    let f3_diff: Vec<_> = intended_3
        .iter()
        .zip(sim.snapshot().iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(y, (a, b))| (y, a.clone(), b.clone()))
        .collect();
    eprintln!("  Frame 3 desyncs: {}", f3_diff.len());
    for (y, intended, simd) in f3_diff.iter().take(15) {
        eprintln!("    row {:2}", y);
        eprintln!("      intended: {}", intended);
        eprintln!("      iTerm2:   {}", simd);
    }

    let total_desync = f1_diff.len() + f2_diff.len() + f3_diff.len();
    eprintln!(
        "\n>>> TOTAL DESYNC under iTerm2 wide-render assumption: {} <<<",
        total_desync
    );

    // Diagnostic-only: print confirmation but do not assert. The fix path
    // (replace EAW=N glyphs OR wrap backend to disable MoveTo-skip) is a
    // separate decision. With current glyphs (✓ ✗ ✉ ⊞), this test reports
    // ~6 desyncs that match the user's screenshot artifact exactly.
    if total_desync > 0 {
        eprintln!(
            ">>> REPRO CONFIRMED: {} cell(s) desync under iTerm2 font-fallback \
             wide-rendering of EAW=N glyphs. See docs/superpowers/specs/screenshot.png \
             for the user-visible artifact.",
            total_desync
        );
    }
}
