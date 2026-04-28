//! Snapshot builder -- SINGLE AsyncEngine::run pass for all queries.

use super::state::*;
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, Utc};
use spur_context::{AgentViewStatus, AsyncEngine};
use std::collections::BTreeMap;

pub async fn build_snapshot(engine: &AsyncEngine) -> Result<InsightsSnapshot> {
    let queries = engine
        .run(|e| -> anyhow::Result<AtomicQueries> {
            e.refresh_cache()?;
            e.use_cached_events()?;
            Ok(AtomicQueries {
                daily_90: e.daily_report(90)?,
                weekly_12: e.weekly_report(12)?,
                monthly_6: e.monthly_report(6)?,
                by_agent_30d: e.daily_report(30)?,
                by_model_30d: e.model_breakdown()?,
                by_project_30d: e.project_breakdown()?,
                live_30min: e.live_recent_sessions(30)?,
            })
        })
        .await?;
    let kpis = derive_kpis(&queries);
    Ok(InsightsSnapshot {
        fetched_at: Utc::now(),
        queries,
        kpis,
        agent_status: AgentViewStatus::default(),
        engine_meta: EngineMeta {
            last_refresh: Utc::now(),
            ..Default::default()
        },
    })
}

pub(crate) fn derive_kpis(q: &AtomicQueries) -> Kpis {
    let dated_daily = q
        .daily_90
        .iter()
        .filter_map(|row| parse_daily_day(row).map(|day| (day, row)))
        .collect::<Vec<_>>();

    let (today_cost, last_7d_cost, last_30d_cost, mtd_cost) = dated_daily
        .iter()
        .map(|(day, _)| *day)
        .max()
        .map(|today| {
            let seven_day_start = today - Duration::days(6);
            let thirty_day_start = today - Duration::days(29);

            dated_daily.iter().fold(
                (0.0, 0.0, 0.0, 0.0),
                |(today_sum, seven_sum, thirty_sum, month_sum), (day, row)| {
                    (
                        today_sum + if *day == today { row.cost_usd } else { 0.0 },
                        seven_sum
                            + if *day >= seven_day_start {
                                row.cost_usd
                            } else {
                                0.0
                            },
                        thirty_sum
                            + if *day >= thirty_day_start {
                                row.cost_usd
                            } else {
                                0.0
                            },
                        month_sum
                            + if day.year() == today.year() && day.month() == today.month() {
                                row.cost_usd
                            } else {
                                0.0
                            },
                    )
                },
            )
        })
        .unwrap_or_default();

    let (input_tokens, cache_read_tokens) = q.daily_90.iter().fold((0_i64, 0_i64), |acc, row| {
        (acc.0 + row.input_tokens, acc.1 + row.cache_read_tokens)
    });
    let cache_denominator = input_tokens + cache_read_tokens;
    let cache_hit_pct = if cache_denominator > 0 {
        (cache_read_tokens as f64 / cache_denominator as f64) * 100.0
    } else {
        0.0
    };

    Kpis {
        today_cost,
        last_7d_cost,
        last_30d_cost,
        mtd_cost,
        active_session_count: q.live_30min.len(),
        cache_hit_pct,
        cost_source_split: CostSourceSplit::default(),
        top_agent: top_agent(q),
        top_model: top_model(q),
    }
}

fn parse_daily_day(row: &spur_context::DailyRow) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&row.day, "%Y-%m-%d").ok()
}

fn top_agent(q: &AtomicQueries) -> Option<(String, f64)> {
    q.by_agent_30d
        .iter()
        .fold(BTreeMap::<String, f64>::new(), |mut totals, row| {
            *totals.entry(row.agent.clone()).or_default() += row.cost_usd;
            totals
        })
        .into_iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
}

fn top_model(q: &AtomicQueries) -> Option<(String, f64)> {
    q.by_model_30d
        .iter()
        .fold(BTreeMap::<String, f64>::new(), |mut totals, row| {
            *totals.entry(row.model.clone()).or_default() += row.total_cost;
            totals
        })
        .into_iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
}
