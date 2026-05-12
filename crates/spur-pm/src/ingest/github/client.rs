//! Octocrab wrapper with the rate-limit governor and `SyncError` mapping
//! described in spec §7.2.
//!
//! This module owns two concerns and nothing else:
//!
//!   R-7.2.1 The [`Governor`] tracks `x-ratelimit-remaining` (REST) and the
//!           GraphQL `rateLimit { remaining resetAt cost }` block, sleeping
//!           via `tokio::time::sleep_until` when below the configured floor.
//!           A shared [`tokio::sync::Notify`] lets concurrent callers share
//!           the same wake.
//!
//!   R-7.2.2 [`GitHubClient::graphql`] wraps `Octocrab::graphql` and maps
//!           `octocrab::Error` to [`SyncError`] per the §7.2 mapping table.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use octocrab::Octocrab;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

use crate::sync::{SyncError, SyncResult};

use super::graphql::{IngestRepoData, IngestRepoVariables, RateLimit, INGEST_REPO_QUERY};

/// Defaults from spec §7.2 (REST floor) and §9 (GraphQL floor).
pub const DEFAULT_REST_FLOOR: u32 = 50;
pub const DEFAULT_GRAPHQL_FLOOR: u32 = 100;

#[derive(Debug, Clone, Copy)]
pub struct GovernorConfig {
    pub rest_floor: u32,
    pub graphql_floor: u32,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            rest_floor: DEFAULT_REST_FLOOR,
            graphql_floor: DEFAULT_GRAPHQL_FLOOR,
        }
    }
}

/// Latest observation, used to decide whether the next request must wait.
#[derive(Debug, Default, Clone, Copy)]
struct GovernorState {
    rest_remaining: Option<u32>,
    rest_reset_at: Option<DateTime<Utc>>,
    graphql_remaining: Option<u32>,
    graphql_reset_at: Option<DateTime<Utc>>,
}

/// Shared rate-limit watcher. Cheap to clone (Arc<Mutex>).
#[derive(Clone)]
pub struct Governor {
    inner: Arc<GovernorInner>,
}

struct GovernorInner {
    config: GovernorConfig,
    state: Mutex<GovernorState>,
    /// Concurrent callers all park on this Notify. Whichever caller's
    /// `sleep_until` fires first calls `notify_waiters()`.
    wake: Notify,
}

impl Governor {
    pub fn new(config: GovernorConfig) -> Self {
        Self {
            inner: Arc::new(GovernorInner {
                config,
                state: Mutex::new(GovernorState::default()),
                wake: Notify::new(),
            }),
        }
    }

    pub fn config(&self) -> GovernorConfig {
        self.inner.config
    }

    /// Record a REST response's rate-limit headers.
    pub async fn observe_rest(&self, remaining: Option<u32>, reset_at: Option<DateTime<Utc>>) {
        let mut s = self.inner.state.lock().await;
        if let Some(r) = remaining {
            s.rest_remaining = Some(r);
        }
        if let Some(rt) = reset_at {
            s.rest_reset_at = Some(rt);
        }
    }

    /// Record a GraphQL response's `rateLimit` block.
    pub async fn observe_graphql(&self, rate: &RateLimit) {
        let mut s = self.inner.state.lock().await;
        s.graphql_remaining = Some(rate.remaining);
        s.graphql_reset_at = Some(rate.reset_at);
    }

    /// Block (asynchronously) if the next request would push us below the
    /// configured floor. Returns immediately if there is budget available.
    pub async fn throttle_rest(&self) {
        let snapshot = { *self.inner.state.lock().await };
        if let (Some(remaining), Some(reset_at)) = (snapshot.rest_remaining, snapshot.rest_reset_at)
        {
            if remaining < self.inner.config.rest_floor {
                self.sleep_until(reset_at).await;
            }
        }
    }

    pub async fn throttle_graphql(&self) {
        let snapshot = { *self.inner.state.lock().await };
        if let (Some(remaining), Some(reset_at)) =
            (snapshot.graphql_remaining, snapshot.graphql_reset_at)
        {
            if remaining < self.inner.config.graphql_floor {
                self.sleep_until(reset_at).await;
            }
        }
    }

    /// Park until `reset_at` (server time, converted to monotonic).
    /// Concurrent callers share the wake — the first one to come out of
    /// `sleep_until` notifies the rest.
    async fn sleep_until(&self, reset_at: DateTime<Utc>) {
        let now_utc = Utc::now();
        let wait = (reset_at - now_utc).num_seconds().max(0) as u64;
        if wait == 0 {
            return;
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(wait);

        // Race the deadline sleep with the shared wake-up notification.
        // Whoever wins notifies the other waiters so we all unpark together.
        let notified = self.inner.wake.notified();
        tokio::pin!(notified);
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                self.inner.wake.notify_waiters();
            }
            _ = &mut notified => {}
        }
    }
}

/// Tagged GitHub client. The Octocrab handle carries the resolved token;
/// the governor sits in front of every wire call.
pub struct GitHubClient {
    octocrab: Octocrab,
    governor: Governor,
}

impl GitHubClient {
    pub fn new(octocrab: Octocrab, governor: Governor) -> Self {
        Self { octocrab, governor }
    }

    pub fn governor(&self) -> &Governor {
        &self.governor
    }

    pub fn octocrab(&self) -> &Octocrab {
        &self.octocrab
    }

    /// Build an `Octocrab` instance from a personal access token (env var
    /// or `gh auth token`). Used by `GitHubSync::new`.
    pub fn build_octocrab(token: &str) -> SyncResult<Octocrab> {
        Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .map_err(map_error)
    }

    /// Execute the `IngestRepo` query (§7.3) and observe the `rateLimit`
    /// block automatically. Returns the deserialized `IngestRepoData` or
    /// a mapped [`SyncError`].
    pub async fn ingest_repo(&self, vars: IngestRepoVariables) -> SyncResult<IngestRepoData> {
        self.governor.throttle_graphql().await;

        let payload = serde_json::json!({
            "query": INGEST_REPO_QUERY,
            "variables": vars,
        });

        let data: IngestRepoData = self.octocrab.graphql(&payload).await.map_err(map_error)?;

        // `rateLimit` is a top-level Query field in GitHub's GraphQL schema,
        // so it's on `IngestRepoData` itself, not on `data.repository`.
        if let Some(rate) = data.rate_limit.as_ref() {
            self.governor.observe_graphql(rate).await;
        }
        Ok(data)
    }
}

/// Map `octocrab::Error` to `SyncError` per the §7.2 mapping table.
///
/// Header inspection limitation: octocrab 0.50.0 does not surface response
/// headers on `GitHubError` (only `status_code`, `message`,
/// `documentation_url`, `errors`). We therefore distinguish the two 403
/// flavors — primary rate-limit (`x-ratelimit-remaining: 0`) vs secondary
/// limit / abuse (`Retry-After` header) — by string-matching the message
/// text GitHub sends. This is best-effort; once Phase 2 wires a custom
/// reqwest middleware we can read the headers directly.
pub fn map_error(err: octocrab::Error) -> SyncError {
    use octocrab::Error as O;
    match err {
        O::GitHub { source, .. } => map_github_error(*source),
        O::Hyper { source, .. } => SyncError::Transient(source.to_string()),
        O::Service { source, .. } => SyncError::Transient(source.to_string()),
        O::Http { source, .. } => SyncError::Transient(source.to_string()),
        O::Serde { source, .. } => SyncError::Malformed(source.to_string()),
        O::Json { source, .. } => SyncError::Malformed(source.to_string()),
        O::SerdeUrlEncoded { source, .. } => SyncError::Malformed(source.to_string()),
        O::Graphql { source, .. } => SyncError::Malformed(source.to_string()),
        other => SyncError::Other(anyhow::anyhow!(other.to_string())),
    }
}

fn map_github_error(err: octocrab::GitHubError) -> SyncError {
    let status = err.status_code.as_u16();
    match status {
        401 => SyncError::NeedsAuth(err.message),
        403 => {
            let msg = err.message.to_lowercase();
            // Heuristic: "rate limit" / "abuse" / "secondary rate limit" all
            // map to RateLimited. Without header access we synthesize a
            // conservative retry-after of 60s (GitHub's documented minimum
            // backoff for secondary limits).
            if msg.contains("rate limit") || msg.contains("abuse") || msg.contains("secondary") {
                SyncError::RateLimited { retry_after_s: 60 }
            } else {
                SyncError::NeedsAuth(err.message)
            }
        }
        404 => SyncError::Gone(err.message),
        500..=599 => SyncError::Transient(err.message),
        _ => SyncError::Other(anyhow::anyhow!(
            "GitHub error {status}: {msg}",
            msg = err.message
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rate-limit governor unit test (spec test plan: "uses tokio::time::pause()
    /// and scripted response headers").
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn governor_sleeps_when_below_floor() {
        let gov = Governor::new(GovernorConfig {
            rest_floor: 50,
            graphql_floor: 100,
        });

        // Scripted observation: only 5 GraphQL points left, resetting in 30s.
        let reset_at = Utc::now() + chrono::Duration::seconds(30);
        gov.observe_graphql(&RateLimit {
            cost: 1,
            remaining: 5,
            reset_at,
            node_count: 1,
        })
        .await;

        // throttle_graphql should park the task until the simulated reset.
        let start = Instant::now();
        gov.throttle_graphql().await;
        let elapsed = Instant::now().duration_since(start);
        assert!(
            elapsed >= std::time::Duration::from_secs(29),
            "expected ~30s sleep, got {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn governor_does_not_sleep_when_above_floor() {
        let gov = Governor::new(GovernorConfig::default());
        let reset_at = Utc::now() + chrono::Duration::seconds(60);
        gov.observe_graphql(&RateLimit {
            cost: 1,
            remaining: 5000,
            reset_at,
            node_count: 1,
        })
        .await;

        let start = Instant::now();
        gov.throttle_graphql().await;
        let elapsed = Instant::now().duration_since(start);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "expected immediate return, got {elapsed:?}"
        );
    }
}
