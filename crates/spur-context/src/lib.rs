//! SPUR Context Engine — DuckDB-backed analytics.
//!
//! This crate provides a unified query interface over agent-generated
//! JSONL session files. It uses DuckDB's `read_json_auto()` to read
//! files in place, with SQL convert views normalizing per-agent schemas.
//!
//! # Architecture
//!
//! ```text
//! Agent JSONL (Claude, Codex, Kiro)
//!     │
//!     ├──► DuckDB: read_json_auto('~/.config/claude/**/*.jsonl')
//!     │    └──► SQL VIEW claude_events: field mapping, type casting
//!     │
//!     ├──► DuckDB: read_json_auto('~/.codex/sessions/**/*.jsonl')
//!     │    └──► SQL VIEW codex_events: window functions for delta
//!     │
//!     └──► DuckDB: read_json_auto('~/.kiro/sessions/**/*.jsonl')
//!          └──► SQL VIEW kiro_events: (stub)
//!     │
//!     ▼
//! DuckDB VIEW all_events: UNION ALL
//!     │
//!     ▼
//! DuckDB VIEW all_events_with_cost: JOIN pricing table
//!     │
//!     ▼
//! AnalyticsEngine::daily_report(), weekly_report(), etc.
//! ```
//!
//! # Example
//!
//! ```no_run
//! use spur_context::AnalyticsEngine;
//! use spur_cost::PricingRegistry;
//!
//! # fn main() -> anyhow::Result<()> {
//! let engine = AnalyticsEngine::open_in_memory()?;
//! engine.initialize()?;
//! engine.load_pricing(&PricingRegistry::with_builtin_prices())?;
//!
//! let report = engine.daily_report(7)?;
//! for row in report {
//!     println!("{} {}: ${:.4}", row.day, row.agent, row.cost_usd);
//! }
//! # Ok(())
//! # }
//! ```

pub mod async_engine;
pub mod engine;
pub mod live;
pub mod reporter;

#[allow(dead_code)]
mod extractors;

pub use async_engine::AsyncEngine;
pub use engine::{
    AgentViewStatus, AnalyticsEngine, DailyRow, LiveBlockRow, LiveSnapshot, ModelRow, MonthlyRow,
    ProjectRow, SessionRow, WeeklyRow,
};
pub use live::{LiveSessionTracker, LiveTrackerPool};
pub use reporter::{
    AgentBreakdown, BurnRate, DailyReport, LiveBlock, LiveReport, ModelReport, ModelTotals,
    MonthlyReport, ProjectReport, ProjectTotals, ReportRange, Reporter, SessionReport, Totals,
    WeeklyReport,
};
