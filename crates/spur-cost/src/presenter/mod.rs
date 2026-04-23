//! Output formatters for usage reports.
//!
//! Supports two presentation modes:
//! - **Table** (`TablePresenter`): human-readable aligned columns for terminal output
//! - **JSON** (`JsonPresenter`): machine-readable structured output
//!
//! Both implement the `Presenter` trait so callers can swap formatting
//! without changing report generation logic.

pub mod json;
pub mod table;

use crate::reports::{DailyReport, LiveReport, MonthlyReport, SessionReport, WeeklyReport};

/// Trait for rendering a report to a string.
pub trait Presenter {
    /// Render a daily report.
    fn render_daily(&self, reports: &[DailyReport]) -> String;
    /// Render a weekly report.
    fn render_weekly(&self, reports: &[WeeklyReport]) -> String;
    /// Render a monthly report.
    fn render_monthly(&self, reports: &[MonthlyReport]) -> String;
    /// Render a session report.
    fn render_session(&self, report: &SessionReport) -> String;
    /// Render a live report.
    fn render_live(&self, report: &LiveReport) -> String;
}
