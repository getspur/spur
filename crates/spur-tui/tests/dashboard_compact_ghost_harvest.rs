//! Compact-render ghost-text harvest. The dashboard view uses `render_compact`
//! (Surface::Compact), which is a DIFFERENT code path from `render_with_ctx`
//! (Surface::Full / SessionDetailView).
//!
//! User runs `spur tui --brain claude-code` which lands on the dashboard.
//! Ctrl+O on dashboard triggers the same observe_collapsed toggle but the
//! render path is compact, not full.

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::react_trace::{
    ActStatus, ReactTrace, TraceEntry, TraceKind,
};
use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
use spur_acp::AgentKind;
use unicode_width::UnicodeWidthStr;

const W: u16 = 100;
const H: u16 = 20;

struct SimTerm {
    grid: Vec<Vec<String>>,
    width: u16,
}

impl SimTerm {
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
            let advance = UnicodeWidthStr::width(sym.as_str()) as u16;
            cur_x = cur_x.saturating_add(advance.max(1));
            last_emit = Some((x, y));
        }
    }

    fn snapshot(&self) -> Vec<String> {
        self.grid.iter().map(|row| row.join("")).collect()
    }
}

fn intended_snap(term: &Terminal<TestBackend>, w: u16, h: u16) -> Vec<String> {
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

fn render_compact(
    trace: &mut ReactTrace,
    term: &mut Terminal<TestBackend>,
    w: u16,
    h: u16,
) -> ratatui::buffer::Buffer {
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, w, h)))
        .unwrap();
    term.backend().buffer().clone()
}

fn diff_grids(intended: &[String], terminal: &[String]) -> Vec<(usize, String, String)> {
    intended
        .iter()
        .zip(terminal.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(y, (a, b))| (y, a.clone(), b.clone()))
        .collect()
}

fn build_dashboard_trace() -> ReactTrace {
    // Dashboard uses with_kind_compact for the compact stream pane
    let mut t = ReactTrace::with_kind_compact(AgentKind::ClaudeCodeAcp);
    // Mix of agent messages, think, and acts with payloads (the dashboard
    // shows the same trace data as session-detail, just with render_compact)
    t.append_user_message("can you check the worktree status", "10:00".into());
    t.append_think("checking worktree", "10:00".into());
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "git worktree list".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: (0..15)
                    .map(|i| format!("/path/to/worktree-{}    abc{:04}d  [branch-{}]", i, i, i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t.append_message("Here are the worktrees.", "claude", "10:02".into());
    t.append_user_message("show file contents", "10:03".into());
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "edit_file".into(),
            family: ToolFamily::Edit,
            input: ToolInputDisplay::Path("Cargo.toml".into()),
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::FileRead {
                path: Some("Cargo.toml".into()),
                content: (0..20)
                    .map(|i| format!("dep_{} = \"1.0.{}\"", i, i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                truncated: false,
            })),
        },
        text: String::new(),
        timestamp: "10:04".into(),
        markdown: None,
    });
    t.append_message("Done.", "claude", "10:05".into());
    t
}

#[test]
fn dashboard_compact_user_repro_dynamic() {
    let mut trace = build_dashboard_trace();
    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut sim = SimTerm::new(W, H);
    let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, W, H));

    let mut total_desync = 0;
    let steps: Vec<(&str, Box<dyn Fn(&mut ReactTrace)>)> = vec![
        ("init compact (collapsed)", Box::new(|_t| {})),
        ("Ctrl+O toggle (expand)", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll_up_by(2)", Box::new(|t| { t.scroll_up_by(2); })),
        ("scroll_up_by(3)", Box::new(|t| { t.scroll_up_by(3); })),
        ("scroll_up_by(5)", Box::new(|t| { t.scroll_up_by(5); })),
        ("scroll_down_by(2)", Box::new(|t| { t.scroll_down_by(2); })),
        ("Ctrl+O collapse", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll_up_by(3)", Box::new(|t| { t.scroll_up_by(3); })),
        ("Ctrl+O re-expand", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll_up_by(10)", Box::new(|t| { t.scroll_up_by(10); })),
        ("scroll_to_bottom", Box::new(|t| { t.scroll_to_bottom(); })),
        ("scroll_up_by(7)", Box::new(|t| { t.scroll_up_by(7); })),
    ];

    for (i, (label, mutate)) in steps.iter().enumerate() {
        mutate(&mut trace);
        let next_buf = render_compact(&mut trace, &mut term, W, H);
        sim.apply_diff(&prev_buf, &next_buf);
        let intended = intended_snap(&term, W, H);
        let terminal_view = sim.snapshot();
        let diffs = diff_grids(&intended, &terminal_view);
        if !diffs.is_empty() {
            eprintln!(
                "  step {} '{}': DESYNC on {} rows",
                i, label, diffs.len()
            );
            for (y, a, b) in diffs.iter().take(5) {
                eprintln!("    row {} INTENDED: {}", y, a);
                eprintln!("    row {} TERMINAL: {}", y, b);
            }
            total_desync += diffs.len();
        } else {
            eprintln!("  step {} '{}': OK", i, label);
        }
        prev_buf = next_buf;
    }

    eprintln!("\n=== Final intended buffer ===");
    let final_intended = intended_snap(&term, W, H);
    for (y, row) in final_intended.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }

    eprintln!("\n=== Final simulated terminal ===");
    let final_terminal = sim.snapshot();
    for (y, row) in final_terminal.iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }

    eprintln!("\n>>> COMPACT-PATH TOTAL DESYNC: {} <<<", total_desync);
}

#[test]
fn dashboard_compact_with_cjk_and_control_chars() {
    let mut trace = ReactTrace::with_kind_compact(AgentKind::ClaudeCodeAcp);
    trace.append_user_message("show log", "10:00".into());
    trace.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "cat log".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: vec![
                    "2026年04月30日 エラー発生 で何かが起きました",
                    "中文测试 内容很长 包含很多字符",
                    "한국어 텍스트 한국어 텍스트 추가",
                    "\x1B[31m red ANSI \x1B[0m text mixed",
                    "Tab\there\tand\there",
                    "Plain narrow line that is somewhat long but readable",
                    "café résumé naïve façade",
                    "👨‍👩‍👧‍👦 family emoji + 👋🏽 wave with skin tone",
                    "🇯🇵 🇺🇸 🇨🇳 flags",
                ].join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    trace.append_message("Done reading log.", "claude", "10:02".into());

    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut sim = SimTerm::new(W, H);
    let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, W, H));
    let mut total_desync = 0;

    let actions: Vec<(&str, Box<dyn Fn(&mut ReactTrace)>)> = vec![
        ("init", Box::new(|_t| {})),
        ("expand", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll up 3", Box::new(|t| { t.scroll_up_by(3); })),
        ("scroll up 5", Box::new(|t| { t.scroll_up_by(5); })),
        ("collapse", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("re-expand", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll down 4", Box::new(|t| { t.scroll_down_by(4); })),
    ];

    for (i, (label, mutate)) in actions.iter().enumerate() {
        mutate(&mut trace);
        let next_buf = render_compact(&mut trace, &mut term, W, H);
        sim.apply_diff(&prev_buf, &next_buf);
        let intended = intended_snap(&term, W, H);
        let terminal_view = sim.snapshot();
        let diffs = diff_grids(&intended, &terminal_view);
        if !diffs.is_empty() {
            eprintln!("  step {} '{}': DESYNC on {} rows", i, label, diffs.len());
            for (y, a, b) in diffs.iter().take(5) {
                eprintln!("    row {} INTENDED: {}", y, a);
                eprintln!("    row {} TERMINAL: {}", y, b);
            }
            total_desync += diffs.len();
        } else {
            eprintln!("  step {} '{}': OK", i, label);
        }
        prev_buf = next_buf;
    }

    eprintln!("\n=== Final intended ===");
    for (y, row) in intended_snap(&term, W, H).iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    eprintln!("\n=== Final simulated terminal ===");
    for (y, row) in sim.snapshot().iter().enumerate() {
        eprintln!("  {:2} | {}", y, row);
    }
    eprintln!("\n>>> COMPACT+CJK TOTAL DESYNC: {} <<<", total_desync);
}

/// Test the EXACT method that the dashboard view uses for the compact
/// stream pane, including any wrapper / context.
#[test]
fn dashboard_compact_long_acp_stream_simulation() {
    use std::collections::HashMap;
    use spur_tui::components::image_cache::ImageCache;
    use spur_tui::components::react_trace::RenderContext;

    // Use SessionDetailView's render path via render_with_ctx, since
    // dashboard MAY actually use that for its expanded session preview.
    // (My earlier audit showed dashboard uses render_compact, but worth
    // double-checking with this combined test.)
    let mut trace = ReactTrace::with_kind_compact(AgentKind::ClaudeCodeAcp);

    // Long realistic session
    for round in 0..5 {
        trace.append_user_message(&format!("round {} input", round), format!("10:{:02}", round * 2));
        trace.append_think(&format!("thinking round {}", round), format!("10:{:02}", round * 2));
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: format!("cmd-{}", round),
                    cwd: None,
                },
                tool_call_id: None,
                status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                    exit_code: Some(0),
                    stdout: (0..(8 + round * 2))
                        .map(|i| format!("round-{}-line-{} content", round, i))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    stderr: String::new(),
                })),
            },
            text: String::new(),
            timestamp: format!("10:{:02}", round * 2 + 1),
            markdown: None,
        });
        trace.append_message(&format!("response for round {}", round), "claude", format!("10:{:02}", round * 2 + 1));
    }

    // Test with BOTH render paths
    eprintln!("\n=== A: render_compact path ===");
    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    let mut sim = SimTerm::new(W, H);
    let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, W, H));
    let mut total = 0;

    let actions: Vec<(&str, Box<dyn Fn(&mut ReactTrace)>)> = vec![
        ("init", Box::new(|_t| {})),
        ("expand", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll up 3", Box::new(|t| { t.scroll_up_by(3); })),
        ("scroll up 5", Box::new(|t| { t.scroll_up_by(5); })),
        ("scroll up 10", Box::new(|t| { t.scroll_up_by(10); })),
        ("scroll down 5", Box::new(|t| { t.scroll_down_by(5); })),
        ("collapse", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll up 8", Box::new(|t| { t.scroll_up_by(8); })),
        ("re-expand", Box::new(|t| { t.toggle_observe_collapsed(); })),
        ("scroll up 15", Box::new(|t| { t.scroll_up_by(15); })),
    ];

    for (i, (label, mutate)) in actions.iter().enumerate() {
        mutate(&mut trace);
        let next_buf = render_compact(&mut trace, &mut term, W, H);
        sim.apply_diff(&prev_buf, &next_buf);
        let diffs = diff_grids(&intended_snap(&term, W, H), &sim.snapshot());
        if !diffs.is_empty() {
            eprintln!("  step {} '{}': DESYNC {} rows", i, label, diffs.len());
            for (y, a, b) in diffs.iter().take(3) {
                eprintln!("    row {} INTENDED: {}", y, a);
                eprintln!("    row {} TERMINAL: {}", y, b);
            }
            total += diffs.len();
        } else {
            eprintln!("  step {} '{}': OK", i, label);
        }
        prev_buf = next_buf;
    }
    eprintln!(">>> COMPACT total desync: {}", total);

    // Now reset and test with render_with_ctx (the SessionDetailView path)
    #[cfg(feature = "markdown")]
    {
        eprintln!("\n=== B: render_with_ctx path ===");
        let mut trace2 = trace; // continues from same state
        let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
        let mut sim = SimTerm::new(W, H);
        let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, W, H));
        let mut cache = ImageCache::new();
        let registry = HashMap::new();
        let mut total2 = 0;

        let actions2: Vec<(&str, Box<dyn Fn(&mut ReactTrace)>)> = vec![
            ("init full", Box::new(|_t| {})),
            ("scroll up 5", Box::new(|t| { t.scroll_up_by(5); })),
            ("collapse", Box::new(|t| { t.toggle_observe_collapsed(); })),
            ("re-expand", Box::new(|t| { t.toggle_observe_collapsed(); })),
            ("scroll up 5", Box::new(|t| { t.scroll_up_by(5); })),
        ];

        for (i, (label, mutate)) in actions2.iter().enumerate() {
            mutate(&mut trace2);
            {
                let mut ctx = RenderContext {
                    mermaid_registry: &registry,
                    picker: None,
                    image_cache: &mut cache,
                };
                term.draw(|f| trace2.render_with_ctx(f, Rect::new(0, 0, W, H), &mut ctx, None))
                    .unwrap();
            }
            let next_buf = term.backend().buffer().clone();
            sim.apply_diff(&prev_buf, &next_buf);
            let diffs = diff_grids(&intended_snap(&term, W, H), &sim.snapshot());
            if !diffs.is_empty() {
                eprintln!("  step {} '{}': DESYNC {} rows", i, label, diffs.len());
                total2 += diffs.len();
            } else {
                eprintln!("  step {} '{}': OK", i, label);
            }
            prev_buf = next_buf;
        }
        eprintln!(">>> FULL total desync: {}", total2);
    }
}
