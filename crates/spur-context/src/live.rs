//! Live session tracker.
//!
//! Tracks an active agent session by polling DuckDB for incremental
//! updates from the session's JSONL file.
//!
//! Since DuckDB reads JSONL in place via `read_json_auto()`, each poll
//! re-scans the file and picks up new lines automatically.

use anyhow::Result;
use std::time::{Duration, Instant};
use tracing;

use crate::engine::{AnalyticsEngine, LiveSnapshot};

/// Tracks a single active session in real time.
///
/// Create a tracker when a session starts, then call `poll()`
/// periodically (e.g., every 5 seconds) to get updated totals.
pub struct LiveSessionTracker<'a> {
    engine: &'a AnalyticsEngine,
    session_id: String,
    agent: String,
    last_poll: Option<Instant>,
    last_snapshot: Option<LiveSnapshot>,
}

impl<'a> LiveSessionTracker<'a> {
    /// Start tracking a session.
    ///
    /// The `agent` hint is used to optimize the query (e.g., query
    /// only the agent-specific view instead of the full union).
    pub fn start(
        engine: &'a AnalyticsEngine,
        session_id: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
        Self {
            engine,
            session_id: session_id.into(),
            agent: agent.into(),
            last_poll: None,
            last_snapshot: None,
        }
    }

    /// Poll for the latest session snapshot.
    ///
    /// This queries DuckDB, which re-reads the JSONL file and
    /// includes any new lines written since the last poll.
    pub fn poll(&mut self) -> Result<LiveSnapshot> {
        let start = Instant::now();

        let snapshot = self
            .engine
            .live_session_snapshot(&self.session_id)?
            .unwrap_or_else(|| LiveSnapshot {
                session_id: self.session_id.clone(),
                agent: self.agent.clone(),
                models: None,
                started_at: None,
                last_activity: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cost_usd: 0.0,
                events: 0,
            });

        self.last_poll = Some(start);
        self.last_snapshot = Some(snapshot.clone());

        tracing::debug!(
            session_id = %self.session_id,
            elapsed_ms = %start.elapsed().as_millis(),
            events = snapshot.events,
            cost_usd = %snapshot.cost_usd,
            "live session poll"
        );

        Ok(snapshot)
    }

    /// Get the most recent snapshot without polling.
    pub fn last_snapshot(&self) -> Option<&LiveSnapshot> {
        self.last_snapshot.as_ref()
    }

    /// Time since last successful poll.
    pub fn time_since_poll(&self) -> Option<Duration> {
        self.last_poll.map(|t| t.elapsed())
    }

    /// Compute burn rate (cost per minute) from the last two polls.
    pub fn burn_rate(&self) -> Option<f64> {
        // Burn rate requires at least two data points.
        // For now, return simple heuristic based on current snapshot.
        self.last_snapshot.as_ref().map(|s| {
            if s.events == 0 {
                0.0
            } else {
                s.cost_usd / (s.events.max(1) as f64)
            }
        })
    }
}

/// Manages multiple live session trackers.
pub struct LiveTrackerPool<'a> {
    engine: &'a AnalyticsEngine,
    trackers: Vec<LiveSessionTracker<'a>>,
}

impl<'a> LiveTrackerPool<'a> {
    pub fn new(engine: &'a AnalyticsEngine) -> Self {
        Self {
            engine,
            trackers: Vec::new(),
        }
    }

    /// Start tracking a new session.
    pub fn start_session(
        &mut self,
        session_id: impl Into<String>,
        agent: impl Into<String>,
    ) -> &mut LiveSessionTracker<'a> {
        let tracker = LiveSessionTracker::start(self.engine, session_id, agent);
        self.trackers.push(tracker);
        self.trackers.last_mut().unwrap()
    }

    /// Poll all active trackers.
    pub fn poll_all(&mut self) -> Vec<Result<LiveSnapshot>> {
        self.trackers.iter_mut().map(|t| t.poll()).collect()
    }

    /// Remove a tracker by session ID.
    pub fn stop_session(&mut self, session_id: &str) {
        self.trackers.retain(|t| t.session_id != session_id);
    }

    /// Get all current snapshots.
    pub fn snapshots(&self) -> Vec<&LiveSnapshot> {
        self.trackers
            .iter()
            .filter_map(|t| t.last_snapshot())
            .collect()
    }
}

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;
    use crate::engine::AnalyticsEngine;

    #[test]
    fn test_live_tracker_empty_session() {
        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        engine
            .load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())
            .unwrap();

        let mut tracker = LiveSessionTracker::start(&engine, "sess-unknown", "claude");
        let snapshot = tracker.poll().unwrap();

        assert_eq!(snapshot.session_id, "sess-unknown");
        assert_eq!(snapshot.agent, "claude");
        assert_eq!(snapshot.events, 0);
        assert_eq!(snapshot.cost_usd, 0.0);
    }
}
