//! Machine-readable JSON presenter for usage reports.
//!
//! Emits structured JSON envelopes with `totals` and `breakdowns` arrays,
//! suitable for piping to `jq` or consuming programmatically.

use serde::Serialize;

use crate::presenter::Presenter;
use crate::reports::{
    AgentBreakdown, DailyReport, LiveBlock, LiveReport, ModelBreakdown, MonthlyReport,
    ProjectBreakdown, SessionNode, SessionReport, Totals, WeeklyReport,
};

// ─── Serializable Wrappers ────────────────────────────────────────────

#[derive(Serialize)]
struct JsonTotals {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    session_count: u64,
}

impl From<&Totals> for JsonTotals {
    fn from(t: &Totals) -> Self {
        Self {
            input_tokens: t.input_tokens,
            output_tokens: t.output_tokens,
            cache_creation_tokens: t.cache_creation_tokens,
            cache_read_tokens: t.cache_read_tokens,
            total_tokens: t.total_tokens,
            cost_usd: t.cost_usd,
            session_count: t.session_count,
        }
    }
}

#[derive(Serialize)]
struct JsonAgentBreakdown {
    agent: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,
    session_count: u64,
}

impl From<&AgentBreakdown> for JsonAgentBreakdown {
    fn from(b: &AgentBreakdown) -> Self {
        Self {
            agent: b.agent.clone(),
            input_tokens: b.input_tokens,
            output_tokens: b.output_tokens,
            cache_creation_tokens: b.cache_creation_tokens,
            cache_read_tokens: b.cache_read_tokens,
            cost_usd: b.cost_usd,
            session_count: b.session_count,
        }
    }
}

#[derive(Serialize)]
struct JsonModelBreakdown {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,
    session_count: u64,
}

impl From<&ModelBreakdown> for JsonModelBreakdown {
    fn from(b: &ModelBreakdown) -> Self {
        Self {
            model: b.model.clone(),
            input_tokens: b.input_tokens,
            output_tokens: b.output_tokens,
            cache_creation_tokens: b.cache_creation_tokens,
            cache_read_tokens: b.cache_read_tokens,
            cost_usd: b.cost_usd,
            session_count: b.session_count,
        }
    }
}

#[derive(Serialize)]
struct JsonProjectBreakdown {
    project: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,
    session_count: u64,
}

impl From<&ProjectBreakdown> for JsonProjectBreakdown {
    fn from(b: &ProjectBreakdown) -> Self {
        Self {
            project: b.project.clone(),
            input_tokens: b.input_tokens,
            output_tokens: b.output_tokens,
            cache_creation_tokens: b.cache_creation_tokens,
            cache_read_tokens: b.cache_read_tokens,
            cost_usd: b.cost_usd,
            session_count: b.session_count,
        }
    }
}

#[derive(Serialize)]
struct JsonDailyItem {
    date: String,
    totals: JsonTotals,
    agents: Vec<JsonAgentBreakdown>,
    models: Vec<JsonModelBreakdown>,
    projects: Vec<JsonProjectBreakdown>,
}

#[derive(Serialize)]
struct JsonWeeklyItem {
    week_start: String,
    totals: JsonTotals,
    agents: Vec<JsonAgentBreakdown>,
    models: Vec<JsonModelBreakdown>,
    projects: Vec<JsonProjectBreakdown>,
}

#[derive(Serialize)]
struct JsonMonthlyItem {
    year_month: String,
    totals: JsonTotals,
    agents: Vec<JsonAgentBreakdown>,
    models: Vec<JsonModelBreakdown>,
    projects: Vec<JsonProjectBreakdown>,
}

#[derive(Serialize)]
struct JsonSessionNode {
    session_id: Option<String>,
    agent: String,
    model: Option<String>,
    project: Option<String>,
    cost_usd: Option<f64>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    depth: u32,
    children: Vec<JsonSessionNode>,
}

impl JsonSessionNode {
    fn from_node(node: &SessionNode) -> Self {
        Self {
            session_id: node.entry.session_id.clone(),
            agent: node.entry.agent.clone(),
            model: node.entry.model.clone(),
            project: node.entry.project.clone(),
            cost_usd: node.entry.cost_usd,
            input_tokens: node.entry.input_tokens,
            output_tokens: node.entry.output_tokens,
            cache_creation_tokens: node.entry.cache_creation_tokens,
            cache_read_tokens: node.entry.cache_read_tokens,
            depth: node.depth,
            children: node
                .children
                .iter()
                .map(JsonSessionNode::from_node)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonSessionReport {
    sessions: Vec<JsonSessionNode>,
    totals: JsonTotals,
    agents: Vec<JsonAgentBreakdown>,
    models: Vec<JsonModelBreakdown>,
    projects: Vec<JsonProjectBreakdown>,
}

#[derive(Serialize)]
struct JsonLiveBlock {
    session_id: String,
    agent: String,
    model: Option<String>,
    project: Option<String>,
    started_at: String,
    last_activity: String,
    is_active: bool,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,
    tokens_per_minute: Option<f64>,
    cost_per_hour: Option<f64>,
    projected_cost_1h: Option<f64>,
}

impl JsonLiveBlock {
    fn from_block(block: &LiveBlock) -> Self {
        Self {
            session_id: block.session_id.clone(),
            agent: block.agent.clone(),
            model: block.model.clone(),
            project: block.project.clone(),
            started_at: block.started_at.to_rfc3339(),
            last_activity: block.last_activity.to_rfc3339(),
            is_active: block.is_active,
            input_tokens: block.input_tokens,
            output_tokens: block.output_tokens,
            cache_creation_tokens: block.cache_creation_tokens,
            cache_read_tokens: block.cache_read_tokens,
            cost_usd: block.cost_usd,
            tokens_per_minute: block.burn_rate.as_ref().map(|b| b.tokens_per_minute),
            cost_per_hour: block.burn_rate.as_ref().map(|b| b.cost_per_hour),
            projected_cost_1h: block.projected_cost,
        }
    }
}

#[derive(Serialize)]
struct JsonLiveReport {
    blocks: Vec<JsonLiveBlock>,
    totals: JsonTotals,
}

// ─── Top-level Envelopes ──────────────────────────────────────────────

#[derive(Serialize)]
struct DailyEnvelope {
    daily: Vec<JsonDailyItem>,
    totals: JsonTotals,
}

#[derive(Serialize)]
struct WeeklyEnvelope {
    weekly: Vec<JsonWeeklyItem>,
    totals: JsonTotals,
}

#[derive(Serialize)]
struct MonthlyEnvelope {
    monthly: Vec<JsonMonthlyItem>,
    totals: JsonTotals,
}

// ─── Presenter ────────────────────────────────────────────────────────

/// Presenter that renders reports as structured JSON.
pub struct JsonPresenter;

impl JsonPresenter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JsonPresenter {
    fn default() -> Self {
        Self
    }
}

impl Presenter for JsonPresenter {
    fn render_daily(&self, reports: &[DailyReport]) -> String {
        let items: Vec<JsonDailyItem> = reports
            .iter()
            .map(|r| JsonDailyItem {
                date: r.date.to_string(),
                totals: JsonTotals::from(&r.totals),
                agents: r.agent_breakdowns.iter().map(Into::into).collect(),
                models: r.model_breakdowns.iter().map(Into::into).collect(),
                projects: r.project_breakdowns.iter().map(Into::into).collect(),
            })
            .collect();

        let grand_totals = if reports.len() == 1 {
            JsonTotals::from(&reports[0].totals)
        } else {
            let mut t = Totals::default();
            for r in reports {
                t.input_tokens += r.totals.input_tokens;
                t.output_tokens += r.totals.output_tokens;
                t.cache_creation_tokens += r.totals.cache_creation_tokens;
                t.cache_read_tokens += r.totals.cache_read_tokens;
                t.cost_usd += r.totals.cost_usd;
                t.session_count += r.totals.session_count;
            }
            t.total_tokens =
                t.input_tokens + t.output_tokens + t.cache_creation_tokens + t.cache_read_tokens;
            JsonTotals::from(&t)
        };

        let envelope = DailyEnvelope {
            daily: items,
            totals: grand_totals,
        };
        serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_string())
    }

    fn render_weekly(&self, reports: &[WeeklyReport]) -> String {
        let items: Vec<JsonWeeklyItem> = reports
            .iter()
            .map(|r| JsonWeeklyItem {
                week_start: r.week_start.to_string(),
                totals: JsonTotals::from(&r.totals),
                agents: r.agent_breakdowns.iter().map(Into::into).collect(),
                models: r.model_breakdowns.iter().map(Into::into).collect(),
                projects: r.project_breakdowns.iter().map(Into::into).collect(),
            })
            .collect();

        let grand_totals = if reports.len() == 1 {
            JsonTotals::from(&reports[0].totals)
        } else {
            let mut t = Totals::default();
            for r in reports {
                t.input_tokens += r.totals.input_tokens;
                t.output_tokens += r.totals.output_tokens;
                t.cache_creation_tokens += r.totals.cache_creation_tokens;
                t.cache_read_tokens += r.totals.cache_read_tokens;
                t.cost_usd += r.totals.cost_usd;
                t.session_count += r.totals.session_count;
            }
            t.total_tokens =
                t.input_tokens + t.output_tokens + t.cache_creation_tokens + t.cache_read_tokens;
            JsonTotals::from(&t)
        };

        let envelope = WeeklyEnvelope {
            weekly: items,
            totals: grand_totals,
        };
        serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_string())
    }

    fn render_monthly(&self, reports: &[MonthlyReport]) -> String {
        let items: Vec<JsonMonthlyItem> = reports
            .iter()
            .map(|r| JsonMonthlyItem {
                year_month: r.year_month.clone(),
                totals: JsonTotals::from(&r.totals),
                agents: r.agent_breakdowns.iter().map(Into::into).collect(),
                models: r.model_breakdowns.iter().map(Into::into).collect(),
                projects: r.project_breakdowns.iter().map(Into::into).collect(),
            })
            .collect();

        let grand_totals = if reports.len() == 1 {
            JsonTotals::from(&reports[0].totals)
        } else {
            let mut t = Totals::default();
            for r in reports {
                t.input_tokens += r.totals.input_tokens;
                t.output_tokens += r.totals.output_tokens;
                t.cache_creation_tokens += r.totals.cache_creation_tokens;
                t.cache_read_tokens += r.totals.cache_read_tokens;
                t.cost_usd += r.totals.cost_usd;
                t.session_count += r.totals.session_count;
            }
            t.total_tokens =
                t.input_tokens + t.output_tokens + t.cache_creation_tokens + t.cache_read_tokens;
            JsonTotals::from(&t)
        };

        let envelope = MonthlyEnvelope {
            monthly: items,
            totals: grand_totals,
        };
        serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_string())
    }

    fn render_session(&self, report: &SessionReport) -> String {
        let json_report = JsonSessionReport {
            sessions: report
                .roots
                .iter()
                .map(JsonSessionNode::from_node)
                .collect(),
            totals: JsonTotals::from(&report.totals),
            agents: report.agent_breakdowns.iter().map(Into::into).collect(),
            models: report.model_breakdowns.iter().map(Into::into).collect(),
            projects: report.project_breakdowns.iter().map(Into::into).collect(),
        };
        serde_json::to_string_pretty(&json_report).unwrap_or_else(|_| "{}".to_string())
    }

    fn render_live(&self, report: &LiveReport) -> String {
        let json_report = JsonLiveReport {
            blocks: report
                .blocks
                .iter()
                .map(JsonLiveBlock::from_block)
                .collect(),
            totals: JsonTotals::from(&report.totals),
        };
        serde_json::to_string_pretty(&json_report).unwrap_or_else(|_| "{}".to_string())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::{
        AgentBreakdown, BurnRate, DailyReport, LiveBlock, LiveReport, ModelBreakdown, Totals,
    };
    use chrono::Utc;

    #[test]
    fn test_json_daily() {
        let presenter = JsonPresenter::new();
        let report = DailyReport {
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
            entries: vec![],
            agent_breakdowns: vec![AgentBreakdown {
                agent: "claude".to_string(),
                input_tokens: 1000,
                output_tokens: 500,
                cache_creation_tokens: 0,
                cache_read_tokens: 100,
                cost_usd: 1.5,
                session_count: 2,
            }],
            model_breakdowns: vec![ModelBreakdown {
                model: "claude-sonnet".to_string(),
                input_tokens: 1000,
                output_tokens: 500,
                cache_creation_tokens: 0,
                cache_read_tokens: 100,
                cost_usd: 1.5,
                session_count: 2,
            }],
            project_breakdowns: vec![],
            totals: Totals {
                input_tokens: 1000,
                output_tokens: 500,
                cache_creation_tokens: 0,
                cache_read_tokens: 100,
                total_tokens: 1600,
                cost_usd: 1.5,
                session_count: 2,
            },
        };
        let json = presenter.render_daily(&[report]);
        assert!(json.contains("\"daily\""));
        assert!(json.contains("2026-04-20"));
        assert!(json.contains("\"claude\""));
        assert!(json.contains("\"claude-sonnet\""));
        assert!(json.contains("1.5"));
    }

    #[test]
    fn test_json_live() {
        let presenter = JsonPresenter::new();
        let block = LiveBlock {
            session_id: "sess-live".to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-5".to_string()),
            project: None,
            started_at: Utc::now(),
            last_activity: Utc::now(),
            is_active: true,
            input_tokens: 5000,
            output_tokens: 2000,
            cache_creation_tokens: 0,
            cache_read_tokens: 500,
            cost_usd: 0.25,
            burn_rate: Some(BurnRate {
                tokens_per_minute: 50.0,
                cost_per_hour: 3.0,
                observed_seconds: 300,
            }),
            projected_cost: Some(3.0),
        };
        let report = LiveReport {
            blocks: vec![block],
            totals: Totals {
                input_tokens: 5000,
                output_tokens: 2000,
                cache_creation_tokens: 0,
                cache_read_tokens: 500,
                total_tokens: 7500,
                cost_usd: 0.25,
                session_count: 1,
            },
        };
        let json = presenter.render_live(&report);
        assert!(json.contains("\"blocks\""));
        assert!(json.contains("\"sess-live\""));
        assert!(json.contains("50.0"));
        assert!(json.contains("3.0"));
    }
}
