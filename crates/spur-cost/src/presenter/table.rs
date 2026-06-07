//! Human-readable table presenter for usage reports.
//!
//! Uses simple ASCII column alignment. No external table-drawing crates
//! are required, keeping `spur-cost` lightweight.

use crate::presenter::Presenter;
use crate::reports::{
    AgentBreakdown, DailyReport, LiveReport, ModelBreakdown, MonthlyReport, ProjectBreakdown,
    SessionNode, SessionReport, Totals, WeeklyReport,
};

/// Presenter that renders reports as aligned ASCII tables.
#[derive(Default)]
pub struct TablePresenter {
    /// When true, suppress model/project breakdowns for narrow terminals.
    pub compact: bool,
}

impl TablePresenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compact() -> Self {
        Self { compact: true }
    }
}

// ─── Formatting Helpers ───────────────────────────────────────────────

fn fmt_usd(v: f64) -> String {
    format!("${:.4}", v)
}

fn fmt_tokens(v: u64) -> String {
    if v >= 1_000_000 {
        format!("{:.2}M", v as f64 / 1_000_000.0)
    } else if v >= 1_000 {
        format!("{:.1}k", v as f64 / 1_000.0)
    } else {
        format!("{}", v)
    }
}

#[expect(dead_code, reason = "kept for table rendering variants")]
fn fmt_dur(secs: u64) -> String {
    let mins = secs / 60;
    let hours = mins / 60;
    let rem_mins = mins % 60;
    if hours > 0 {
        format!("{}h {}m", hours, rem_mins)
    } else {
        format!("{}m", mins)
    }
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

#[expect(dead_code, reason = "kept for table rendering variants")]
fn pad_left(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - len), s)
    }
}

fn divider(width: usize) -> String {
    "─".repeat(width)
}

fn render_totals(totals: &Totals) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  Tokens:  in={:<10} out={:<10} cache={:<10} read={:<10} total={}\n",
        fmt_tokens(totals.input_tokens),
        fmt_tokens(totals.output_tokens),
        fmt_tokens(totals.cache_creation_tokens),
        fmt_tokens(totals.cache_read_tokens),
        fmt_tokens(totals.total_tokens),
    ));
    out.push_str(&format!(
        "  Cost:    {}    Sessions: {}\n",
        fmt_usd(totals.cost_usd),
        totals.session_count,
    ));
    out
}

fn render_agent_breakdowns(breakdowns: &[AgentBreakdown]) -> String {
    if breakdowns.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("  Agent Breakdown:\n");
    out.push_str(&format!(
        "    {}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}  {:>8}\n",
        pad("Agent", 12),
        "Input",
        "Output",
        "Cache",
        "CacheRd",
        "Cost",
        "Events"
    ));
    for b in breakdowns {
        out.push_str(&format!(
            "    {}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}  {:>8}\n",
            pad(&b.agent, 12),
            fmt_tokens(b.input_tokens),
            fmt_tokens(b.output_tokens),
            fmt_tokens(b.cache_creation_tokens),
            fmt_tokens(b.cache_read_tokens),
            fmt_usd(b.cost_usd),
            b.session_count,
        ));
    }
    out
}

fn render_model_breakdowns(breakdowns: &[ModelBreakdown]) -> String {
    if breakdowns.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("  Model Breakdown:\n");
    out.push_str(&format!(
        "    {}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}  {:>8}\n",
        pad("Model", 20),
        "Input",
        "Output",
        "Cache",
        "CacheRd",
        "Cost",
        "Events"
    ));
    for b in breakdowns {
        out.push_str(&format!(
            "    {}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}  {:>8}\n",
            pad(&b.model, 20),
            fmt_tokens(b.input_tokens),
            fmt_tokens(b.output_tokens),
            fmt_tokens(b.cache_creation_tokens),
            fmt_tokens(b.cache_read_tokens),
            fmt_usd(b.cost_usd),
            b.session_count,
        ));
    }
    out
}

fn render_project_breakdowns(breakdowns: &[ProjectBreakdown]) -> String {
    if breakdowns.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("  Project Breakdown:\n");
    out.push_str(&format!(
        "    {}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}  {:>8}\n",
        pad("Project", 20),
        "Input",
        "Output",
        "Cache",
        "CacheRd",
        "Cost",
        "Events"
    ));
    for b in breakdowns {
        out.push_str(&format!(
            "    {}  {:>10}  {:>10}  {:>10}  {:>10}  {:>12}  {:>8}\n",
            pad(&b.project, 20),
            fmt_tokens(b.input_tokens),
            fmt_tokens(b.output_tokens),
            fmt_tokens(b.cache_creation_tokens),
            fmt_tokens(b.cache_read_tokens),
            fmt_usd(b.cost_usd),
            b.session_count,
        ));
    }
    out
}

fn render_session_node(node: &SessionNode, prefix: &str, out: &mut String) {
    let e = &node.entry;
    let total_cost = node.total_cost();
    let total_tokens = node.total_tokens();
    let indent = "  ".repeat(node.depth as usize);
    let sid = e.session_id.as_deref().unwrap_or("unknown");
    out.push_str(&format!(
        "{}{}[{}] {} | {} | {} tokens\n",
        prefix,
        indent,
        &sid[..sid.len().min(8)],
        e.agent,
        fmt_usd(total_cost),
        fmt_tokens(total_tokens),
    ));
    for child in &node.children {
        render_session_node(child, prefix, out);
    }
}

// ─── Presenter Implementation ─────────────────────────────────────────

impl Presenter for TablePresenter {
    fn render_daily(&self, reports: &[DailyReport]) -> String {
        let mut out = String::new();
        out.push_str("╔══════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                         DAILY USAGE REPORT                           ║\n");
        out.push_str(
            "╚══════════════════════════════════════════════════════════════════════╝\n\n",
        );

        for r in reports {
            out.push_str(&format!("📅 {}\n", r.date));
            out.push_str(&divider(70));
            out.push('\n');
            out.push_str(&render_totals(&r.totals));
            out.push_str(&render_agent_breakdowns(&r.agent_breakdowns));
            if !self.compact {
                out.push_str(&render_model_breakdowns(&r.model_breakdowns));
                out.push_str(&render_project_breakdowns(&r.project_breakdowns));
            }
            out.push('\n');
        }

        if reports.is_empty() {
            out.push_str("No usage data found for the selected period.\n");
        }

        out
    }

    fn render_weekly(&self, reports: &[WeeklyReport]) -> String {
        let mut out = String::new();
        out.push_str("╔══════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                        WEEKLY USAGE REPORT                           ║\n");
        out.push_str(
            "╚══════════════════════════════════════════════════════════════════════╝\n\n",
        );

        for r in reports {
            out.push_str(&format!("🗓️  Week starting {}\n", r.week_start));
            out.push_str(&divider(70));
            out.push('\n');
            out.push_str(&render_totals(&r.totals));
            out.push_str(&render_agent_breakdowns(&r.agent_breakdowns));
            if !self.compact {
                out.push_str(&render_model_breakdowns(&r.model_breakdowns));
                out.push_str(&render_project_breakdowns(&r.project_breakdowns));
            }
            out.push('\n');
        }

        if reports.is_empty() {
            out.push_str("No usage data found for the selected period.\n");
        }

        out
    }

    fn render_monthly(&self, reports: &[MonthlyReport]) -> String {
        let mut out = String::new();
        out.push_str("╔══════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                       MONTHLY USAGE REPORT                           ║\n");
        out.push_str(
            "╚══════════════════════════════════════════════════════════════════════╝\n\n",
        );

        for r in reports {
            out.push_str(&format!("📆 {}\n", r.year_month));
            out.push_str(&divider(70));
            out.push('\n');
            out.push_str(&render_totals(&r.totals));
            out.push_str(&render_agent_breakdowns(&r.agent_breakdowns));
            if !self.compact {
                out.push_str(&render_model_breakdowns(&r.model_breakdowns));
                out.push_str(&render_project_breakdowns(&r.project_breakdowns));
            }
            out.push('\n');
        }

        if reports.is_empty() {
            out.push_str("No usage data found for the selected period.\n");
        }

        out
    }

    fn render_session(&self, report: &SessionReport) -> String {
        let mut out = String::new();
        out.push_str("╔══════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                      SESSION USAGE REPORT                            ║\n");
        out.push_str(
            "╚══════════════════════════════════════════════════════════════════════╝\n\n",
        );

        out.push_str("Delegation Trees:\n");
        out.push_str(&divider(70));
        out.push('\n');
        for root in &report.roots {
            render_session_node(root, "", &mut out);
            out.push('\n');
        }

        out.push_str(&divider(70));
        out.push('\n');
        out.push_str(&render_totals(&report.totals));
        out.push_str(&render_agent_breakdowns(&report.agent_breakdowns));
        if !self.compact {
            out.push_str(&render_model_breakdowns(&report.model_breakdowns));
            out.push_str(&render_project_breakdowns(&report.project_breakdowns));
        }

        if report.roots.is_empty() {
            out.push_str("No sessions found for the selected period.\n");
        }

        out
    }

    fn render_live(&self, report: &LiveReport) -> String {
        let mut out = String::new();
        out.push_str("╔══════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║                        LIVE USAGE REPORT                             ║\n");
        out.push_str(
            "╚══════════════════════════════════════════════════════════════════════╝\n\n",
        );

        if report.blocks.is_empty() {
            out.push_str("No active sessions.\n");
            return out;
        }

        out.push_str(&format!(
            "{}  {:>12}  {:>10}  {:>12}  {:>10}  {:>12}\n",
            pad("Session", 20),
            "Agent",
            "Status",
            "Tokens",
            "Cost",
            "Burn/hr",
        ));
        out.push_str(&divider(90));
        out.push('\n');

        for block in &report.blocks {
            let status = if block.is_active {
                "🔴 active"
            } else {
                "⚪ recent"
            };
            let total_tokens = block.input_tokens
                + block.output_tokens
                + block.cache_creation_tokens
                + block.cache_read_tokens;
            let burn = block
                .burn_rate
                .as_ref()
                .map(|b| fmt_usd(b.cost_per_hour))
                .unwrap_or_else(|| "—".to_string());
            out.push_str(&format!(
                "{}  {:>12}  {:>10}  {:>12}  {:>10}  {:>12}\n",
                pad(&block.session_id[..block.session_id.len().min(20)], 20),
                pad(&block.agent, 12),
                status,
                fmt_tokens(total_tokens),
                fmt_usd(block.cost_usd),
                burn,
            ));

            if let Some(proj) = &block.project {
                out.push_str(&format!("  → project: {}\n", proj));
            }
            if let Some(br) = &block.burn_rate {
                out.push_str(&format!(
                    "  → {:.1} tokens/min  |  projected 1hr: {}\n",
                    br.tokens_per_minute,
                    fmt_usd(block.projected_cost.unwrap_or(0.0)),
                ));
            }
        }

        out.push('\n');
        out.push_str(&divider(70));
        out.push('\n');
        out.push_str(&render_totals(&report.totals));

        out
    }
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::{
        AgentBreakdown, DailyReport, LiveBlock, LiveReport, ModelBreakdown, Totals,
    };
    use chrono::Utc;

    #[test]
    fn test_render_daily() {
        let presenter = TablePresenter::new();
        let report = DailyReport {
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
            entries: vec![],
            agent_breakdowns: vec![AgentBreakdown {
                agent: "claude".to_string(),
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cache_creation_tokens: 0,
                cache_read_tokens: 100_000,
                cost_usd: 5.2345,
                session_count: 3,
            }],
            model_breakdowns: vec![ModelBreakdown {
                model: "claude-sonnet".to_string(),
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cache_creation_tokens: 0,
                cache_read_tokens: 100_000,
                cost_usd: 5.2345,
                session_count: 3,
            }],
            project_breakdowns: vec![],
            totals: Totals {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cache_creation_tokens: 0,
                cache_read_tokens: 100_000,
                total_tokens: 1_600_000,
                cost_usd: 5.2345,
                session_count: 3,
            },
        };
        let out = presenter.render_daily(&[report]);
        assert!(out.contains("DAILY USAGE REPORT"));
        assert!(out.contains("2026-04-20"));
        assert!(out.contains("claude"));
        assert!(out.contains("1.00M"));
        assert!(out.contains("$5.2345"));
    }

    #[test]
    fn test_render_live() {
        let presenter = TablePresenter::new();
        let block = LiveBlock {
            session_id: "sess-abc".to_string(),
            agent: "claude".to_string(),
            model: Some("claude-sonnet".to_string()),
            project: Some("my-project".to_string()),
            started_at: Utc::now(),
            last_activity: Utc::now(),
            is_active: true,
            input_tokens: 10_000,
            output_tokens: 5_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 1_000,
            cost_usd: 0.5,
            burn_rate: Some(crate::reports::BurnRate {
                tokens_per_minute: 100.0,
                cost_per_hour: 6.0,
                observed_seconds: 300,
            }),
            projected_cost: Some(6.0),
        };
        let report = LiveReport {
            blocks: vec![block],
            totals: Totals {
                input_tokens: 10_000,
                output_tokens: 5_000,
                cache_creation_tokens: 0,
                cache_read_tokens: 1_000,
                total_tokens: 16_000,
                cost_usd: 0.5,
                session_count: 1,
            },
        };
        let out = presenter.render_live(&report);
        assert!(out.contains("LIVE USAGE REPORT"));
        assert!(out.contains("active"));
        assert!(out.contains("sess-abc"));
        assert!(out.contains("my-project"));
    }

    #[test]
    fn test_compact_mode() {
        let presenter = TablePresenter::compact();
        let report = DailyReport {
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
            entries: vec![],
            agent_breakdowns: vec![],
            model_breakdowns: vec![ModelBreakdown {
                model: "gpt-5".to_string(),
                input_tokens: 1000,
                output_tokens: 500,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                cost_usd: 0.1,
                session_count: 1,
            }],
            project_breakdowns: vec![],
            totals: Totals::default(),
        };
        let out = presenter.render_daily(&[report]);
        // In compact mode, model breakdown should be suppressed
        assert!(!out.contains("Model Breakdown"));
    }
}
