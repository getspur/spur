//! Report type definitions for cross-agent usage reporting.
//!
//! Mirrors ccusage's report shapes (daily, weekly, monthly, session, live/blocks)
//! but adapted to SPUR's orchestration model.

use crate::ingest::TokenEvent;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use std::collections::HashMap;

// ─── Breakdown Types ──────────────────────────────────────────────────

/// Aggregated usage for a single agent within a report window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentBreakdown {
    pub agent: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub session_count: u64,
}

/// Aggregated usage for a single model within a report window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelBreakdown {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub session_count: u64,
}

/// Aggregated usage for a single project within a report window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectBreakdown {
    pub project: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub session_count: u64,
}

// ─── Totals ───────────────────────────────────────────────────────────

/// Summary totals across all entries in a report window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub session_count: u64,
}

impl Totals {
    pub fn from_entries(entries: &[TokenEvent]) -> Self {
        let mut t = Self::default();
        for e in entries {
            t.input_tokens += e.input_tokens;
            t.output_tokens += e.output_tokens;
            t.cache_creation_tokens += e.cache_creation_tokens;
            t.cache_read_tokens += e.cache_read_tokens;
            t.cost_usd += e.cost_usd.unwrap_or(0.0);
        }
        t.total_tokens =
            t.input_tokens + t.output_tokens + t.cache_creation_tokens + t.cache_read_tokens;
        t.session_count = entries.len() as u64;
        t
    }
}

// ─── Report Types ─────────────────────────────────────────────────────

/// Usage aggregated by calendar day.
#[derive(Debug, Clone)]
pub struct DailyReport {
    pub date: NaiveDate,
    pub entries: Vec<TokenEvent>,
    pub agent_breakdowns: Vec<AgentBreakdown>,
    pub model_breakdowns: Vec<ModelBreakdown>,
    pub project_breakdowns: Vec<ProjectBreakdown>,
    pub totals: Totals,
}

/// Usage aggregated by ISO week (Monday-based).
#[derive(Debug, Clone)]
pub struct WeeklyReport {
    pub week_start: NaiveDate,
    pub entries: Vec<TokenEvent>,
    pub agent_breakdowns: Vec<AgentBreakdown>,
    pub model_breakdowns: Vec<ModelBreakdown>,
    pub project_breakdowns: Vec<ProjectBreakdown>,
    pub totals: Totals,
}

/// Usage aggregated by calendar month.
#[derive(Debug, Clone)]
pub struct MonthlyReport {
    pub year_month: String, // "YYYY-MM"
    pub entries: Vec<TokenEvent>,
    pub agent_breakdowns: Vec<AgentBreakdown>,
    pub model_breakdowns: Vec<ModelBreakdown>,
    pub project_breakdowns: Vec<ProjectBreakdown>,
    pub totals: Totals,
}

/// A single session node in the session tree.
#[derive(Debug, Clone)]
pub struct SessionNode {
    pub entry: TokenEvent,
    pub children: Vec<SessionNode>,
    pub depth: u32,
}

impl SessionNode {
    /// Sum of this node's cost plus all descendants.
    pub fn total_cost(&self) -> f64 {
        let child_cost: f64 = self.children.iter().map(|c| c.total_cost()).sum();
        self.entry.cost_usd.unwrap_or(0.0) + child_cost
    }

    /// Sum of this node's tokens plus all descendants.
    pub fn total_tokens(&self) -> u64 {
        let child_tokens: u64 = self.children.iter().map(|c| c.total_tokens()).sum();
        self.entry.total_tokens() + child_tokens
    }
}

/// Usage grouped by session.
#[derive(Debug, Clone)]
pub struct SessionReport {
    pub roots: Vec<SessionNode>,
    pub agent_breakdowns: Vec<AgentBreakdown>,
    pub model_breakdowns: Vec<ModelBreakdown>,
    pub project_breakdowns: Vec<ProjectBreakdown>,
    pub totals: Totals,
}

/// Burn-rate metrics for a live (active) session block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BurnRate {
    /// Tokens per minute based on elapsed time.
    pub tokens_per_minute: f64,
    /// Cost per hour at current rate.
    pub cost_per_hour: f64,
    /// Duration of the observed window in seconds.
    pub observed_seconds: u64,
}

/// A live session block with burn-rate projection.
#[derive(Debug, Clone)]
pub struct LiveBlock {
    pub session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub project: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub is_active: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub burn_rate: Option<BurnRate>,
    /// Projected total cost if the session runs for `projection_minutes`.
    pub projected_cost: Option<f64>,
}

/// Live usage report showing currently active sessions.
#[derive(Debug, Clone)]
pub struct LiveReport {
    pub blocks: Vec<LiveBlock>,
    pub totals: Totals,
}

// ─── Aggregation Helpers ──────────────────────────────────────────────

/// Fold entries into per-agent breakdowns.
pub fn aggregate_by_agent(entries: &[TokenEvent]) -> Vec<AgentBreakdown> {
    let mut map: HashMap<String, AgentBreakdown> = HashMap::new();
    for e in entries {
        let key = e.agent.clone();
        let bd = map.entry(key).or_insert_with(|| AgentBreakdown {
            agent: e.agent.clone(),
            ..Default::default()
        });
        bd.input_tokens += e.input_tokens;
        bd.output_tokens += e.output_tokens;
        bd.cache_creation_tokens += e.cache_creation_tokens;
        bd.cache_read_tokens += e.cache_read_tokens;
        bd.cost_usd += e.cost_usd.unwrap_or(0.0);
        bd.session_count += 1;
    }
    let mut vec: Vec<_> = map.into_values().collect();
    vec.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    vec
}

/// Fold entries into per-model breakdowns.
pub fn aggregate_by_model(entries: &[TokenEvent]) -> Vec<ModelBreakdown> {
    let mut map: HashMap<String, ModelBreakdown> = HashMap::new();
    for e in entries {
        let key = e.model.clone().unwrap_or_else(|| "unknown".to_string());
        if key == "<synthetic>" {
            continue;
        }
        let bd = map.entry(key.clone()).or_insert_with(|| ModelBreakdown {
            model: key,
            ..Default::default()
        });
        bd.input_tokens += e.input_tokens;
        bd.output_tokens += e.output_tokens;
        bd.cache_creation_tokens += e.cache_creation_tokens;
        bd.cache_read_tokens += e.cache_read_tokens;
        bd.cost_usd += e.cost_usd.unwrap_or(0.0);
        bd.session_count += 1;
    }
    let mut vec: Vec<_> = map.into_values().collect();
    vec.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
    vec
}

/// Fold entries into per-project breakdowns.
pub fn aggregate_by_project(entries: &[TokenEvent]) -> Vec<ProjectBreakdown> {
    let mut map: HashMap<String, ProjectBreakdown> = HashMap::new();
    for e in entries {
        let key = e
            .project
            .clone()
            .unwrap_or_else(|| "(unassigned)".to_string());
        let bd = map.entry(key.clone()).or_insert_with(|| ProjectBreakdown {
            project: key,
            ..Default::default()
        });
        bd.input_tokens += e.input_tokens;
        bd.output_tokens += e.output_tokens;
        bd.cache_creation_tokens += e.cache_creation_tokens;
        bd.cache_read_tokens += e.cache_read_tokens;
        bd.cost_usd += e.cost_usd.unwrap_or(0.0);
        bd.session_count += 1;
    }
    let mut vec: Vec<_> = map.into_values().collect();
    vec.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.project.cmp(&b.project))
    });
    vec
}

/// Build a session tree from flat entries grouped by session_id.
///
/// Returns one root per unique `session_id`. For file-based ingestion
/// (agent JSONL logs), there is no `parent_session` link — each session
/// file is independent. This produces a flat forest of sessions.
pub fn build_session_tree(entries: Vec<TokenEvent>) -> Vec<SessionNode> {
    let mut by_session: HashMap<String, Vec<TokenEvent>> = HashMap::new();
    for e in entries {
        let sid = e
            .session_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        by_session.entry(sid).or_default().push(e);
    }

    let mut roots = Vec::new();
    for (_sid, mut session_entries) in by_session {
        session_entries.sort_by_key(|entry| entry.timestamp);
        // Merge all entries for this session into a single aggregated node
        let first = session_entries.first().cloned().unwrap();
        let totals = Totals::from_entries(&session_entries);
        let merged = TokenEvent {
            timestamp: first.timestamp,
            session_id: first.session_id.clone(),
            agent: first.agent.clone(),
            model: first.model.clone(),
            project: first.project.clone(),
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
            cache_creation_tokens: totals.cache_creation_tokens,
            cache_read_tokens: totals.cache_read_tokens,
            cost_usd: Some(totals.cost_usd),
            source_file: first.source_file,
        };
        roots.push(SessionNode {
            entry: merged,
            children: Vec::new(),
            depth: 0,
        });
    }

    // Sort by cost descending
    roots.sort_by(|a, b| {
        b.total_cost()
            .partial_cmp(&a.total_cost())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    roots
}

/// Group entries by calendar day.
pub fn group_by_day(entries: Vec<TokenEvent>) -> HashMap<NaiveDate, Vec<TokenEvent>> {
    let mut map: HashMap<NaiveDate, Vec<TokenEvent>> = HashMap::new();
    for e in entries {
        let day = e.timestamp.date_naive();
        map.entry(day).or_default().push(e);
    }
    map
}

/// Group entries by ISO week (Monday-based).
pub fn group_by_week(entries: Vec<TokenEvent>) -> HashMap<NaiveDate, Vec<TokenEvent>> {
    let mut map: HashMap<NaiveDate, Vec<TokenEvent>> = HashMap::new();
    for e in entries {
        let day = e.timestamp.date_naive();
        let week_start = day - chrono::Days::new(day.weekday().num_days_from_monday() as u64);
        map.entry(week_start).or_default().push(e);
    }
    map
}

/// Group entries by calendar month.
pub fn group_by_month(entries: Vec<TokenEvent>) -> HashMap<String, Vec<TokenEvent>> {
    let mut map: HashMap<String, Vec<TokenEvent>> = HashMap::new();
    for e in entries {
        let key = format!("{:04}-{:02}", e.timestamp.year(), e.timestamp.month());
        map.entry(key).or_default().push(e);
    }
    map
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_event(agent: &str, model: Option<&str>, cost: f64, tokens: u64) -> TokenEvent {
        TokenEvent {
            timestamp: Utc::now(),
            session_id: Some(format!("sess-{}", uuid::Uuid::new_v4())),
            agent: agent.to_string(),
            model: model.map(|s| s.to_string()),
            project: Some("proj-a".to_string()),
            input_tokens: tokens,
            output_tokens: tokens / 2,
            cache_creation_tokens: 0,
            cache_read_tokens: tokens / 10,
            cost_usd: Some(cost),
            source_file: PathBuf::from("/tmp/test.jsonl"),
        }
    }

    #[test]
    fn test_totals_from_entries() {
        let entries = vec![
            sample_event("claude", Some("claude-sonnet"), 1.0, 1000),
            sample_event("codex", Some("gpt-5"), 2.0, 2000),
        ];
        let totals = Totals::from_entries(&entries);
        assert_eq!(totals.session_count, 2);
        assert_eq!(totals.input_tokens, 3000);
        assert!((totals.cost_usd - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_aggregate_by_agent() {
        let entries = vec![
            sample_event("claude", None, 1.0, 100),
            sample_event("claude", None, 2.0, 200),
            sample_event("codex", None, 3.0, 300),
        ];
        let breakdowns = aggregate_by_agent(&entries);
        assert_eq!(breakdowns.len(), 2);
        assert_eq!(breakdowns[0].agent, "claude"); // tie-breaker: alphabetical
        assert_eq!(breakdowns[0].cost_usd, 3.0);
        assert_eq!(breakdowns[1].cost_usd, 3.0);
        assert_eq!(breakdowns[0].session_count, 2);
        assert_eq!(breakdowns[1].session_count, 1);
    }

    #[test]
    fn test_aggregate_by_model_skips_synthetic() {
        let entries = vec![
            TokenEvent {
                model: Some("<synthetic>".to_string()),
                ..sample_event("claude", Some("<synthetic>"), 1.0, 100)
            },
            sample_event("claude", Some("claude-sonnet"), 2.0, 200),
        ];
        let breakdowns = aggregate_by_model(&entries);
        assert_eq!(breakdowns.len(), 1);
        assert_eq!(breakdowns[0].model, "claude-sonnet");
    }

    #[test]
    fn test_build_session_tree() {
        let mut e1 = sample_event("claude", None, 1.0, 100);
        e1.session_id = Some("sess-1".to_string());
        let mut e2 = sample_event("claude", None, 2.0, 200);
        e2.session_id = Some("sess-1".to_string());
        let mut e3 = sample_event("codex", None, 3.0, 300);
        e3.session_id = Some("sess-2".to_string());

        let roots = build_session_tree(vec![e1, e2, e3]);
        assert_eq!(roots.len(), 2);
        // sess-2 has higher total cost (3.0) so comes first
        assert!((roots[0].total_cost() - 3.0).abs() < 0.001);
        assert!((roots[1].total_cost() - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_group_by_day() {
        let mut e1 = sample_event("claude", None, 1.0, 100);
        e1.timestamp = "2026-04-20T10:00:00Z".parse().unwrap();
        let mut e2 = sample_event("claude", None, 2.0, 200);
        e2.timestamp = "2026-04-20T15:00:00Z".parse().unwrap();
        let mut e3 = sample_event("codex", None, 3.0, 300);
        e3.timestamp = "2026-04-21T10:00:00Z".parse().unwrap();

        let grouped = group_by_day(vec![e1, e2, e3]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped
                .get(&NaiveDate::from_ymd_opt(2026, 4, 20).unwrap())
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            grouped
                .get(&NaiveDate::from_ymd_opt(2026, 4, 21).unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_group_by_week() {
        let mut e1 = sample_event("claude", None, 1.0, 100);
        e1.timestamp = "2026-04-20T10:00:00Z".parse().unwrap(); // Monday
        let mut e2 = sample_event("claude", None, 2.0, 200);
        e2.timestamp = "2026-04-22T10:00:00Z".parse().unwrap(); // Wednesday
        let mut e3 = sample_event("codex", None, 3.0, 300);
        e3.timestamp = "2026-04-27T10:00:00Z".parse().unwrap(); // Next Monday

        let grouped = group_by_week(vec![e1, e2, e3]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(
            grouped
                .get(&NaiveDate::from_ymd_opt(2026, 4, 20).unwrap())
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            grouped
                .get(&NaiveDate::from_ymd_opt(2026, 4, 27).unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_group_by_month() {
        let mut e1 = sample_event("claude", None, 1.0, 100);
        e1.timestamp = "2026-04-15T10:00:00Z".parse().unwrap();
        let mut e2 = sample_event("codex", None, 2.0, 200);
        e2.timestamp = "2026-04-20T10:00:00Z".parse().unwrap();
        let mut e3 = sample_event("kiro", None, 3.0, 300);
        e3.timestamp = "2026-05-01T10:00:00Z".parse().unwrap();

        let grouped = group_by_month(vec![e1, e2, e3]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped.get("2026-04").unwrap().len(), 2);
        assert_eq!(grouped.get("2026-05").unwrap().len(), 1);
    }
}
