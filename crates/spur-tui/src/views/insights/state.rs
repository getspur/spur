//! Insights view state types.

use chrono::{DateTime, Utc};
use spur_context::{
    AgentViewStatus, DailyRow, LiveBlockRow, ModelRow, MonthlyRow, ProjectRow, WeeklyRow,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct InsightsSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub queries: AtomicQueries,
    pub kpis: Kpis,
    pub agent_status: AgentViewStatus,
    pub engine_meta: EngineMeta,
}

#[derive(Debug, Clone, Default)]
pub struct AtomicQueries {
    pub daily_90: Vec<DailyRow>,
    pub weekly_12: Vec<WeeklyRow>,
    pub monthly_6: Vec<MonthlyRow>,
    pub by_agent_30d: Vec<DailyRow>,
    pub by_model_30d: Vec<ModelRow>,
    pub by_project_30d: Vec<ProjectRow>,
    pub live_30min: Vec<LiveBlockRow>,
}

#[derive(Debug, Clone, Default)]
pub struct Kpis {
    pub today_cost: f64,
    pub last_7d_cost: f64,
    pub last_30d_cost: f64,
    pub mtd_cost: f64,
    pub active_session_count: usize,
    pub cache_hit_pct: f64,
    pub cost_source_split: CostSourceSplit,
    pub top_agent: Option<(String, f64)>,
    pub top_model: Option<(String, f64)>,
}

#[derive(Debug, Clone, Default)]
pub struct CostSourceSplit {
    pub native_pct: f64,
    pub priced_pct: f64,
    pub unpriced_pct: f64,
}

#[derive(Debug, Clone, Default)]
pub struct EngineMeta {
    pub events_cache_rows: i64,
    pub last_refresh: DateTime<Utc>,
    pub agent_view_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightsTab {
    Overview,
    Timeline,
    Breakdown,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Agent,
    Model,
    Project,
}

#[derive(Default)]
pub struct RefreshState {
    pub last_good: Option<InsightsSnapshot>,
    pub last_error: Option<Arc<anyhow::Error>>,
    pub refreshing: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::insights::builder::derive_kpis;

    fn day(date: &str, agent: &str, cost: f64) -> DailyRow {
        DailyRow {
            day: date.to_string(),
            agent: agent.to_string(),
            sessions: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: cost,
        }
    }

    fn day_with_cache(date: &str, input_tokens: i64, cache_read_tokens: i64) -> DailyRow {
        DailyRow {
            input_tokens,
            cache_read_tokens,
            ..day(date, "codex", 0.0)
        }
    }

    fn model(name: &str, cost: f64) -> ModelRow {
        ModelRow {
            model: name.to_string(),
            agent: "codex".to_string(),
            requests: 1,
            input_tokens: 0,
            output_tokens: 0,
            avg_cost: cost,
            total_cost: cost,
        }
    }

    fn live(session_id: &str) -> LiveBlockRow {
        LiveBlockRow {
            session_id: session_id.to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-5.3-codex".to_string()),
            started_at: None,
            last_activity: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.0,
            events: 1,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {actual} to be close to {expected}"
        );
    }

    #[test]
    fn derive_kpis_today_and_7d_sums() {
        let q = AtomicQueries {
            daily_90: vec![
                day("2026-04-28", "codex", 4.21),
                day("2026-04-27", "claude", 5.10),
                day("2026-04-20", "gemini", 2.50),
            ],
            ..Default::default()
        };

        let k = derive_kpis(&q);

        assert_close(k.today_cost, 4.21);
        assert_close(k.last_7d_cost, 9.31);
        assert_close(k.last_30d_cost, 11.81);
        assert_close(k.mtd_cost, 11.81);
    }

    #[test]
    fn derive_kpis_cache_hit_pct() {
        let q = AtomicQueries {
            daily_90: vec![
                day_with_cache("2026-04-28", 100, 100),
                day_with_cache("2026-04-27", 300, 100),
            ],
            ..Default::default()
        };

        let k = derive_kpis(&q);

        assert_close(k.cache_hit_pct, 33.333);
    }

    #[test]
    fn derive_kpis_top_agent_top_model() {
        let q = AtomicQueries {
            by_agent_30d: vec![
                day("2026-04-28", "codex", 4.0),
                day("2026-04-27", "claude", 3.0),
                day("2026-04-26", "codex", 2.0),
            ],
            by_model_30d: vec![
                model("claude-opus-4-5", 8.0),
                model("gpt-5.3-codex", 6.0),
                model("claude-opus-4-5", 3.0),
            ],
            ..Default::default()
        };

        let k = derive_kpis(&q);

        assert_eq!(k.top_agent, Some(("codex".to_string(), 6.0)));
        assert_eq!(k.top_model, Some(("claude-opus-4-5".to_string(), 11.0)));
    }

    #[test]
    fn derive_kpis_active_session_count_from_live() {
        let q = AtomicQueries {
            live_30min: vec![live("session-a"), live("session-b"), live("session-c")],
            ..Default::default()
        };

        let k = derive_kpis(&q);

        assert_eq!(k.active_session_count, 3);
    }

    #[test]
    fn derive_kpis_handles_empty() {
        let k = derive_kpis(&AtomicQueries::default());

        assert_close(k.today_cost, 0.0);
        assert_close(k.last_7d_cost, 0.0);
        assert_close(k.last_30d_cost, 0.0);
        assert_close(k.mtd_cost, 0.0);
        assert_eq!(k.active_session_count, 0);
        assert_close(k.cache_hit_pct, 0.0);
        assert_close(k.cost_source_split.native_pct, 0.0);
        assert_close(k.cost_source_split.priced_pct, 0.0);
        assert_close(k.cost_source_split.unpriced_pct, 0.0);
        assert_eq!(k.top_agent, None);
        assert_eq!(k.top_model, None);
    }
}
