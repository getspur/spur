//! Async wrapper over `AnalyticsEngine`.
//!
//! DuckDB operations are blocking (file I/O + query execution). This wrapper
//! ensures all queries run in `tokio::task::spawn_blocking` so the async
//! executor is never blocked.
//!
//! ```no_run
//! use spur_context::{AnalyticsEngine, AsyncEngine};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let engine = AnalyticsEngine::open_in_memory()?;
//! let async_engine = AsyncEngine::new(engine);
//!
//! let report = async_engine.daily_report(7).await?;
//! for row in report {
//!     println!("{} {}: ${:.4}", row.day, row.agent, row.cost_usd);
//! }
//! # Ok(())
//! # }
//! ```

use anyhow::Result;
use chrono::NaiveDate;
use std::sync::{Arc, Mutex};

use crate::engine::{
    AgentViewStatus, AnalyticsEngine, DailyRow, LiveBlockRow, LiveSnapshot, ModelRow, MonthlyRow,
    ProjectRow, SessionRow, WeeklyRow,
};

/// Async wrapper over `AnalyticsEngine`.
///
/// Since `duckdb::Connection` is `Send` but not `Sync`, this wrapper uses an
/// `Arc<std::sync::Mutex<AnalyticsEngine>>` to allow shared access from async
/// code. The mutex is never held across `.await` points — it is acquired only
/// inside `spawn_blocking` closures.
#[derive(Clone)]
pub struct AsyncEngine {
    inner: Arc<Mutex<AnalyticsEngine>>,
}

impl AsyncEngine {
    /// Wrap an `AnalyticsEngine` for async use.
    pub fn new(engine: AnalyticsEngine) -> Self {
        Self {
            inner: Arc::new(Mutex::new(engine)),
        }
    }

    /// Recover the underlying engine (requires exclusive access).
    ///
    /// Returns `None` if another clone still holds a reference.
    pub fn into_inner(self) -> Option<AnalyticsEngine> {
        Arc::into_inner(self.inner).map(|m| m.into_inner().unwrap())
    }

    /// Run an arbitrary blocking operation on the engine.
    ///
    /// This is the escape hatch for methods not yet wrapped.
    pub async fn run<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut AnalyticsEngine) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.lock().unwrap();
            f(&mut engine)
        })
        .await?
    }

    // ─── Lifecycle ──────────────────────────────────────────────────────

    /// Initialize base schema.
    pub async fn initialize(&self) -> Result<()> {
        self.run(|e| e.initialize()).await
    }

    /// Create agent convert views.
    pub async fn create_agent_views(&self) -> Result<AgentViewStatus> {
        self.run(|e| e.create_agent_views()).await
    }

    /// Load pricing data from the Rust PricingRegistry into DuckDB.
    pub async fn load_pricing(&self, registry: spur_cost::PricingRegistry) -> Result<()> {
        self.run(move |e| e.load_pricing(&registry)).await
    }

    // ─── Report Queries ─────────────────────────────────────────────────

    /// Daily cost report for the last N days.
    pub async fn daily_report(&self, days: u32) -> Result<Vec<DailyRow>> {
        self.run(move |e| e.daily_report(days)).await
    }

    /// Daily cost report for a specific date range.
    pub async fn daily_report_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<DailyRow>> {
        self.run(move |e| e.daily_report_range(start, end)).await
    }

    /// Weekly cost report for the last N weeks.
    pub async fn weekly_report(&self, weeks: u32) -> Result<Vec<WeeklyRow>> {
        self.run(move |e| e.weekly_report(weeks)).await
    }

    /// Weekly cost report for a specific date range.
    pub async fn weekly_report_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<WeeklyRow>> {
        self.run(move |e| e.weekly_report_range(start, end)).await
    }

    /// Monthly cost report for the last N months.
    pub async fn monthly_report(&self, months: u32) -> Result<Vec<MonthlyRow>> {
        self.run(move |e| e.monthly_report(months)).await
    }

    /// Monthly cost report for a specific date range.
    pub async fn monthly_report_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<MonthlyRow>> {
        self.run(move |e| e.monthly_report_range(start, end)).await
    }

    /// Model cost breakdown.
    pub async fn model_breakdown(&self) -> Result<Vec<ModelRow>> {
        self.run(|e| e.model_breakdown()).await
    }

    /// Project cost breakdown.
    pub async fn project_breakdown(&self) -> Result<Vec<ProjectRow>> {
        self.run(|e| e.project_breakdown()).await
    }

    /// Detail for a single session.
    pub async fn session_detail(&self, session_id: String) -> Result<Option<SessionRow>> {
        self.run(move |e| e.session_detail(&session_id)).await
    }

    /// Live snapshot for an active session.
    pub async fn live_session_snapshot(&self, session_id: String) -> Result<Option<LiveSnapshot>> {
        self.run(move |e| e.live_session_snapshot(&session_id))
            .await
    }

    /// Live recent sessions within the last N minutes.
    pub async fn live_recent_sessions(&self, minutes: u32) -> Result<Vec<LiveBlockRow>> {
        self.run(move |e| e.live_recent_sessions(minutes)).await
    }

    /// Execute a raw SQL query and return results as JSON strings.
    pub async fn query_json(&self, sql: String) -> Result<Vec<serde_json::Value>> {
        self.run(move |e| e.query_json(&sql)).await
    }
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_async_engine_daily_report() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        let async_engine = AsyncEngine::new(engine);

        async_engine.initialize().await.unwrap();
        async_engine
            .load_pricing(spur_cost::PricingRegistry::with_builtin_prices())
            .await
            .unwrap();

        let rows = async_engine.daily_report(7).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_async_engine_run_generic() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        let async_engine = AsyncEngine::new(engine);

        async_engine.initialize().await.unwrap();

        let count: i64 = async_engine
            .run(|e| {
                e.conn()
                    .query_row("SELECT 1 + 1", [], |r| r.get(0))
                    .map_err(|e| e.into())
            })
            .await
            .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_async_engine_clone_and_into_inner() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        let async_engine = AsyncEngine::new(engine);
        let cloned = async_engine.clone();

        // Can't into_inner while clone exists
        assert!(async_engine.into_inner().is_none());

        // Drop clone, then recover
        let engine = cloned.into_inner().expect("should recover engine");
        let _ = engine;
    }
}
