//! Snapshot builder -- SINGLE AsyncEngine::run pass for all queries.

use super::state::*;
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, Utc};
use spur_context::{AgentViewStatus, AsyncEngine};
use std::collections::BTreeMap;

pub async fn build_snapshot(engine: &AsyncEngine) -> Result<InsightsSnapshot> {
    let t0 = std::time::Instant::now();
    tracing::info!(target: "spur_tui::insights::builder", "build_snapshot: dispatching to spawn_blocking");
    let queries = engine
        .run(|e| -> anyhow::Result<AtomicQueries> {
            tracing::info!(target: "spur_tui::insights::builder", "build_snapshot: inside blocking closure (mutex acquired)");
            let step = std::time::Instant::now();
            let materialized_rows = e.refresh_cache()?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, materialized_rows, "step refresh_cache done");

            let step = std::time::Instant::now();
            e.use_cached_events()?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, "step use_cached_events done");

            let step = std::time::Instant::now();
            let daily_90 = e.daily_report(90)?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = daily_90.len(), "step daily_report(90) done");

            let step = std::time::Instant::now();
            let weekly_12 = e.weekly_report(12)?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = weekly_12.len(), "step weekly_report(12) done");

            let step = std::time::Instant::now();
            let monthly_6 = e.monthly_report(6)?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = monthly_6.len(), "step monthly_report(6) done");

            let step = std::time::Instant::now();
            let by_agent_30d = e.daily_report(30)?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = by_agent_30d.len(), "step daily_report(30) done");

            let step = std::time::Instant::now();
            let by_model_30d = e.model_breakdown()?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = by_model_30d.len(), "step model_breakdown done");

            let step = std::time::Instant::now();
            let by_project_30d = e.project_breakdown()?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = by_project_30d.len(), "step project_breakdown done");

            let step = std::time::Instant::now();
            let live_30min = e.live_recent_sessions(30)?;
            tracing::info!(target: "spur_tui::insights::builder", elapsed_ms = step.elapsed().as_millis() as u64, rows = live_30min.len(), "step live_recent_sessions(30) done");

            Ok(AtomicQueries {
                daily_90,
                weekly_12,
                monthly_6,
                by_agent_30d,
                by_model_30d,
                by_project_30d,
                live_30min,
            })
        })
        .await?;
    let kpis = derive_kpis(&queries);
    tracing::info!(target: "spur_tui::insights::builder", total_ms = t0.elapsed().as_millis() as u64, "build_snapshot complete");
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
