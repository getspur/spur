//! Stateless renderers for Insights tabs.

pub mod breakdown;
pub mod live;
pub mod overview;
pub mod timeline;

pub use breakdown::BreakdownTab;
pub use live::LiveTab;
pub use overview::OverviewTab;
pub use timeline::TimelineTab;

#[cfg(test)]
pub(super) fn render_to_text(render: impl FnOnce(&mut ratatui::Frame<'_>)) -> String {
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    fn buffer_text(buf: &Buffer) -> String {
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        rendered
    }

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(render).unwrap();
    buffer_text(terminal.backend().buffer())
}

#[cfg(test)]
pub(super) fn synthetic_snapshot() -> crate::views::insights::state::InsightsSnapshot {
    use crate::views::insights::state::{AtomicQueries, CostSourceSplit, InsightsSnapshot, Kpis};
    use chrono::Utc;
    use spur_context::{AgentViewStatus, DailyRow, LiveBlockRow, ModelRow, ProjectRow};

    fn daily(day: &str, agent: &str, cost_usd: f64) -> DailyRow {
        DailyRow {
            day: day.to_string(),
            agent: agent.to_string(),
            sessions: 3,
            input_tokens: 10_000,
            output_tokens: 2_000,
            cache_read_tokens: 1_000,
            cache_creation_tokens: 500,
            cost_usd,
        }
    }

    fn model(model: &str, agent: &str, total_cost: f64) -> ModelRow {
        ModelRow {
            model: model.to_string(),
            agent: agent.to_string(),
            requests: 12,
            input_tokens: 20_000,
            output_tokens: 4_000,
            avg_cost: total_cost / 12.0,
            total_cost,
        }
    }

    fn project(project: &str, agent: &str, cost_usd: f64) -> ProjectRow {
        ProjectRow {
            project: project.to_string(),
            agent: agent.to_string(),
            sessions: 5,
            input_tokens: 30_000,
            output_tokens: 6_000,
            cost_usd,
        }
    }

    fn live(session_id: &str, agent: &str, cost_usd: f64) -> LiveBlockRow {
        LiveBlockRow {
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            model: Some("gpt-5-codex".to_string()),
            started_at: Some("2026-04-28T00:00:00Z".to_string()),
            last_activity: Some("2026-04-28T00:05:00Z".to_string()),
            input_tokens: 16_000,
            output_tokens: 4_000,
            cache_read_tokens: 2_000,
            cache_creation_tokens: 500,
            cost_usd,
            events: 20,
        }
    }

    let daily_90 = (0..90)
        .map(|idx| {
            daily(
                &format!("2026-04-{:02}", (idx % 30) + 1),
                "codex",
                (idx % 9) as f64 + 1.0,
            )
        })
        .collect();

    InsightsSnapshot {
        fetched_at: Utc::now(),
        queries: AtomicQueries {
            daily_90,
            by_agent_30d: vec![
                daily("2026-04-28", "claude-code", 89.12),
                daily("2026-04-27", "codex", 52.40),
                daily("2026-04-26", "opencode", 11.05),
            ],
            by_model_30d: vec![
                model("claude-opus-4-5", "claude-code", 74.50),
                model("gpt-5-codex", "codex", 52.40),
                model("claude-sonnet-4", "claude-code", 14.62),
            ],
            by_project_30d: vec![
                project("spur", "codex", 41.20),
                project("mermaid-v2", "claude-code", 18.30),
                project("(none)", "opencode", 9.40),
            ],
            live_30min: vec![
                live("abc12345-session", "claude-code", 2.05),
                live("def45678-session", "codex", 0.90),
            ],
            ..Default::default()
        },
        kpis: Kpis {
            today_cost: 4.21,
            last_7d_cost: 28.40,
            last_30d_cost: 112.00,
            mtd_cost: 112.00,
            active_session_count: 2,
            cache_hit_pct: 47.8,
            cost_source_split: CostSourceSplit {
                native_pct: 42.0,
                priced_pct: 51.0,
                unpriced_pct: 7.0,
            },
            top_agent: Some(("claude-code".to_string(), 89.12)),
            top_model: Some(("claude-opus-4-5".to_string(), 74.50)),
        },
        agent_status: AgentViewStatus::default(),
        engine_meta: Default::default(),
    }
}
