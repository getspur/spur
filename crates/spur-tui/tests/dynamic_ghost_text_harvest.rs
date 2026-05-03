//! Dynamic harvest of ghost-text candidates via diverse adversarial content
//! and dynamic state mutations.
//!
//! Each scenario captures:
//!   - The intended buffer state (what ratatui asks the terminal to show)
//!   - The actual terminal display (after applying ratatui's diff via a
//!     simulated cursor that auto-advances by REAL UnicodeWidthStr per glyph)
//!   - The diff between the two
//!
//! Any non-zero divergence is reported per-scenario. Together this casts a
//! wide net over the realistic-content space the user might hit.

#![cfg(feature = "markdown")]

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::image_cache::ImageCache;
use spur_tui::components::react_trace::{
    ActStatus, ReactTrace, RenderContext, TraceEntry, TraceKind,
};
use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
use spur_acp::AgentKind;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

const W: u16 = 100;
const H: u16 = 30;

/// Simulated terminal that applies ratatui's diff with REAL cursor advance
/// behavior (auto-advance by UnicodeWidthStr per print).
struct SimTerm {
    grid: Vec<Vec<String>>,
    width: u16,
    height: u16,
}

impl SimTerm {
    fn new(width: u16, height: u16) -> Self {
        Self {
            grid: vec![vec![" ".to_string(); width as usize]; height as usize],
            width,
            height,
        }
    }

    /// Apply diff(prev, next), mirroring ratatui's MoveTo-skip optimization
    /// + REAL terminal cursor auto-advance. Track every print + its actual
    /// landing column.
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

fn render(
    trace: &mut ReactTrace,
    term: &mut Terminal<TestBackend>,
    cache: &mut ImageCache,
    w: u16,
    h: u16,
) -> ratatui::buffer::Buffer {
    let registry = HashMap::new();
    {
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: None,
            image_cache: cache,
        };
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, w, h), &mut ctx, None))
            .unwrap();
    }
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

fn run_scenario<F>(name: &str, w: u16, h: u16, build: F) -> usize
where
    F: FnOnce() -> ReactTrace,
{
    eprintln!("\n========== SCENARIO: {} ==========", name);
    let mut trace = build();
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut cache = ImageCache::new();
    let mut sim = SimTerm::new(w, h);
    let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, w, h));

    let mut total_desync = 0;
    let mut steps: Vec<(&str, Box<dyn Fn(&mut ReactTrace)>)> = vec![
        ("init render (collapsed)", Box::new(|_t: &mut ReactTrace| {})),
        ("Ctrl+O toggle (expand)", Box::new(|t: &mut ReactTrace| {
            t.toggle_observe_collapsed();
        })),
        ("scroll_up_by(3)", Box::new(|t: &mut ReactTrace| {
            t.scroll_up_by(3);
        })),
        ("scroll_up_by(5)", Box::new(|t: &mut ReactTrace| {
            t.scroll_up_by(5);
        })),
        ("tick (spinner advance)", Box::new(|t: &mut ReactTrace| {
            t.tick();
        })),
        ("scroll_down_by(2)", Box::new(|t: &mut ReactTrace| {
            t.scroll_down_by(2);
        })),
        ("Ctrl+O toggle (collapse)", Box::new(|t: &mut ReactTrace| {
            t.toggle_observe_collapsed();
        })),
        ("Ctrl+O toggle (re-expand)", Box::new(|t: &mut ReactTrace| {
            t.toggle_observe_collapsed();
        })),
        ("scroll_up_by(10)", Box::new(|t: &mut ReactTrace| {
            t.scroll_up_by(10);
        })),
    ];

    for (i, (label, mutate)) in steps.drain(..).enumerate() {
        mutate(&mut trace);
        let next_buf = render(&mut trace, &mut term, &mut cache, w, h);
        sim.apply_diff(&prev_buf, &next_buf);
        let intended = intended_snap(&term, w, h);
        let terminal_view = sim.snapshot();
        let diffs = diff_grids(&intended, &terminal_view);
        if !diffs.is_empty() {
            eprintln!(
                "  step {} '{}': DESYNC on {} rows",
                i,
                label,
                diffs.len()
            );
            for (y, a, b) in diffs.iter().take(3) {
                eprintln!("    row {} INTENDED: {}", y, a);
                eprintln!("    row {} TERMINAL: {}", y, b);
            }
            total_desync += diffs.len();
        } else {
            eprintln!("  step {} '{}': OK (terminal == intended)", i, label);
        }
        prev_buf = next_buf;
    }

    eprintln!("  >>> TOTAL DESYNC for '{}': {} rows <<<", name, total_desync);
    total_desync
}

// ── Scenario builders with diverse adversarial content ─────────────────────

fn scenario_active_spinner_during_expand() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.append_think("Starting work.", "10:00".into());
    // An ACTIVE Act (Pending) — tick will mark_dirty_from this entry
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "long_running_task".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Pending,
        },
        text: "running...".into(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    // A completed Act with payload
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
                stdout: (0..15)
                    .map(|i| format!("file_{}.txt", i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:02".into(),
        markdown: None,
    });
    t
}

fn scenario_cjk_wide_chars_in_tool_output() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.append_think("Reading Japanese log.", "10:00".into());
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "cat log.txt".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: vec![
                    "2026年04月30日 エラー発生",
                    "ログ出力: ファイルが見つかりません",
                    "INFO: 処理を続行します",
                    "中文字符测试 中文字符测试 中文字符测试",
                    "한국어 텍스트 한국어 텍스트",
                    "混合 mixed ascii と 日本語 contents",
                    "table | header | 列1 | 列2",
                    "row1  | data   | 値1  | 値2",
                ].join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t.append_message("Done.", "claude", "10:02".into());
    t
}

fn scenario_control_chars_in_tool_output() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "build".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: vec![
                    "\x1B[32m   Compiling foo v0.1.0\x1B[0m",
                    "\x1B[31merror[E0001]: something\x1B[0m",
                    "Progress: [###       ]\rProgress: [######    ]\rProgress: [##########]",
                    "Tab\there\tand\there",
                    "\x07Bell ring",
                    "Backspace\x08\x08\x08gone",
                    "Plain line",
                ].join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t.append_message("Build done.", "claude", "10:02".into());
    t
}

fn scenario_combining_diacritics_and_zwj_emoji() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "cat names.txt".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: vec![
                    // Combining diacritics
                    "café crème brûlée — naïve façade",
                    "résumé piñata jalapeño",
                    // ZWJ emoji (composed family glyph, single grapheme but multi-codepoint)
                    "Family: 👨‍👩‍👧‍👦 — looks like one glyph",
                    "Skin tone: 👋🏽 👋🏿",
                    "Flag: 🇯🇵 🇺🇸",
                    // Mixed
                    "mixed: ⌘+⇧+P  ⌥+⇥",
                    // Long line with mix
                    "line that is fairly long and contains éèàê combining marks and emoji 🎯 and arrows ➜ and stuff",
                ].join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t.append_message("Done.", "claude", "10:02".into());
    t
}

fn scenario_super_long_lines_no_wrap_chars() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "minified.js".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: vec![
                    // 500-char line with no spaces
                    format!("a{}", "b".repeat(499)),
                    // 200-char line with spaces every 5 chars (wraps cleanly)
                    (0..40).map(|i| format!("word{}", i)).collect::<Vec<_>>().join(" "),
                    // Pathologically long URL-like
                    format!("https://example.com/{}", "very/deep/path/segment/".repeat(20)),
                ].join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t.append_message("Done.", "claude", "10:02".into());
    t
}

fn scenario_mixed_widths_at_boundary() -> ReactTrace {
    let mut t = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
    // Build entries that put wide chars right at column W-1 boundary
    t.push(TraceEntry {
        kind: TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command { cmd: "x".into(), cwd: None },
            tool_call_id: None,
            status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                // Each line ends with a wide char near the right edge
                stdout: (0..10).map(|i| {
                    let pad = "x".repeat((90 + i) % 95);
                    format!("{}日", pad)  // 日 is EAW=W
                }).collect::<Vec<_>>().join("\n"),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:01".into(),
        markdown: None,
    });
    t
}

#[test]
fn dynamic_harvest() {
    let mut grand_total = 0;

    grand_total += run_scenario("active_spinner_during_expand", W, H, scenario_active_spinner_during_expand);
    grand_total += run_scenario("cjk_wide_chars_in_tool_output", W, H, scenario_cjk_wide_chars_in_tool_output);
    grand_total += run_scenario("control_chars_in_tool_output", W, H, scenario_control_chars_in_tool_output);
    grand_total += run_scenario("combining_diacritics_and_zwj_emoji", W, H, scenario_combining_diacritics_and_zwj_emoji);
    grand_total += run_scenario("super_long_lines_no_wrap_chars", W, H, scenario_super_long_lines_no_wrap_chars);
    grand_total += run_scenario("mixed_widths_at_boundary", W, H, scenario_mixed_widths_at_boundary);

    // Width-change scenarios: render at one width, then resize, then continue
    eprintln!("\n========== SCENARIO: width_change_during_expand ==========");
    {
        let w1 = 100u16;
        let w2 = 80u16;
        let h = 25u16;
        let mut trace = scenario_cjk_wide_chars_in_tool_output();
        let mut term = Terminal::new(TestBackend::new(w1, h)).unwrap();
        let mut cache = ImageCache::new();
        let mut sim = SimTerm::new(w1, h);
        let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, w1, h));
        let buf1 = render(&mut trace, &mut term, &mut cache, w1, h);
        sim.apply_diff(&prev_buf, &buf1);
        prev_buf = buf1;

        trace.toggle_observe_collapsed();
        let buf2 = render(&mut trace, &mut term, &mut cache, w1, h);
        sim.apply_diff(&prev_buf, &buf2);
        prev_buf = buf2;

        // Resize: drop term + sim and rebuild at narrower width
        let mut term = Terminal::new(TestBackend::new(w2, h)).unwrap();
        let mut sim = SimTerm::new(w2, h);
        let mut prev_buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, w2, h));
        let buf3 = render(&mut trace, &mut term, &mut cache, w2, h);
        sim.apply_diff(&prev_buf, &buf3);

        let intended = intended_snap(&term, w2, h);
        let terminal_view = sim.snapshot();
        let diffs = diff_grids(&intended, &terminal_view);
        if !diffs.is_empty() {
            eprintln!("  width_change DESYNC: {} rows", diffs.len());
            for (y, a, b) in diffs.iter().take(3) {
                eprintln!("    row {} INTENDED: {}", y, a);
                eprintln!("    row {} TERMINAL: {}", y, b);
            }
            grand_total += diffs.len();
        } else {
            eprintln!("  width_change: OK");
        }
    }

    eprintln!("\n=================================================");
    eprintln!(">>> GRAND TOTAL DESYNC across all scenarios: {} <<<", grand_total);
    eprintln!("=================================================");
}
