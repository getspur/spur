use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::{McpServer, McpServerHttp};
use dashmap::DashMap;
use spur_acp::DelegationDispatchError;
use spur_blob_store::OutcomeStore;
use spur_mcp::worker_server::{WorkerMcpDeps, WorkerMcpServer};
use spur_mcp::McpCallbackServer;
use spur_pm::PmService;

/// Phase 5 / Task 26 — clonable bundle of orchestrator state needed to
/// lazily ensure (and mint a token against) the per-`BrainSession`
/// [`WorkerMcpServer`]. Captured by `handle_delegations` and threaded
/// through `execute_delegation` so the static dispatch path can call
/// back into the orchestrator's cache without holding `&self`.
#[derive(Clone)]
pub(crate) struct WorkerMcpFetcher {
    pub(crate) cache: Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    pub(crate) pm_service: Option<Arc<PmService>>,
    pub(super) feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
    pub(super) funnel: crate::event_funnel::FunnelHandle,
    /// Per-`BrainSession` brain MCP server. Doubles as `PlanResolver`
    /// (via its `impl crate::handlers::PlanResolver`) and supplies the
    /// reconciler outcome handle that worker `get_plan_status` reads.
    pub(super) mcp_server: Arc<McpCallbackServer>,
    pub(super) outcome_store: Arc<dyn OutcomeStore>,
    pub(super) repo_root: Option<PathBuf>,
}

impl WorkerMcpFetcher {
    /// Same body as [`Orchestrator::ensure_worker_mcp_server`] — kept here
    /// so the orchestrator method can delegate, preserving a single
    /// source of truth for the lazy-start / cache contract.
    pub(crate) async fn ensure(
        &self,
        brain: &spur_acp::BrainSessionId,
    ) -> Result<Arc<WorkerMcpServer>, DelegationDispatchError> {
        cache_or_start(
            &self.cache,
            brain.clone(),
            || async {
                let pm = self.pm_service.clone().ok_or_else(|| {
                    DelegationDispatchError::WorkerMcpUnavailable {
                        reason: "pm_service not configured on orchestrator".into(),
                    }
                })?;
                let gate = self.feature_gate.clone().ok_or_else(|| {
                    DelegationDispatchError::WorkerMcpUnavailable {
                        reason: "feature_gate not configured on orchestrator".into(),
                    }
                })?;
                let funnel: Arc<dyn spur_mcp::McpEventSink> = Arc::new(self.funnel.clone());
                let plan_resolver: Arc<dyn spur_mcp::handlers::PlanResolver> =
                    Arc::clone(&self.mcp_server) as Arc<dyn spur_mcp::handlers::PlanResolver>;
                let reconciler_outcomes = self.mcp_server.reconciler_outcomes_handle();
                let deps = WorkerMcpDeps {
                    pm_service: pm,
                    feature_gate: gate,
                    funnel,
                    plan_resolver,
                    reconciler_outcomes,
                    outcome_store: Arc::clone(&self.outcome_store),
                    repo_root: self.repo_root.clone(),
                };
                let server = WorkerMcpServer::start(brain.to_string(), deps)
                    .await
                    .map_err(|e| DelegationDispatchError::WorkerMcpUnavailable {
                        reason: format!("listener bind failed: {e}"),
                    })?;
                tracing::info!(
                    brain_session_id = %brain,
                    url = %server.url(),
                    "WorkerMcpServer started"
                );
                Ok(server)
            },
            |server: &WorkerMcpServer| server.is_running(),
        )
        .await
    }

    /// Convenience: ensure the per-`BrainSession` server is up and mint
    /// a 1-hour HMAC token bound to `(brain, delegation_id)`. Returns
    /// the server's URL and the freshly minted token; the caller
    /// assembles the final `?token=` URL.
    pub(crate) async fn fetch_url_token(
        &self,
        brain: &spur_acp::BrainSessionId,
        delegation_id: &str,
    ) -> Result<(String, String), DelegationDispatchError> {
        let server = self.ensure(brain).await?;
        let token = server.issue_token(delegation_id, std::time::Duration::from_secs(3600));
        Ok((server.url(), token))
    }
}

/// Phase 5 / Task 26 — pure decision helper for the worker dispatch site.
///
/// Returns `Vec::new()` only when `enable_worker_mcp` is `Some(false)`
/// (explicit opt-out). When `None` (omitted) or `Some(true)`, awaits
/// `fetch` to obtain `(url, token)`, assembles the `?token=` URL, and
/// returns a single `McpServer::Http` entry named `spur-worker-mcp`.
/// Any error from `fetch` is propagated.
pub(super) async fn build_worker_mcp_servers_with<F, Fut>(
    enable_worker_mcp: Option<bool>,
    fetch: F,
) -> Result<Vec<McpServer>, DelegationDispatchError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(String, String), DelegationDispatchError>>,
{
    if !enable_worker_mcp.unwrap_or(true) {
        return Ok(Vec::new());
    }
    let (url, token) = fetch().await?;
    let url_with_token = format!("{}?token={}", url, token);
    Ok(vec![McpServer::Http(McpServerHttp::new(
        "spur-worker-mcp",
        &url_with_token,
    ))])
}

/// Compute-once cache helper for [`Orchestrator::ensure_worker_mcp_server`].
///
/// Generic over the value/error types so the cache contract (`first miss
/// runs the starter; subsequent calls — including racing concurrent
/// callers — return the same `Arc`) can be unit-tested without booting a
/// real `WorkerMcpServer`. The starter runs *outside* any DashMap shard
/// lock; if two callers race the miss, both run the starter but only one
/// `Arc` wins the insert and the loser is dropped.
///
/// `is_alive` is consulted on cache hit to decide whether the cached
/// value is still usable. A `false` return evicts the entry and the
/// starter runs to mint a fresh value — protecting against handing out
/// a stale `Arc<WorkerMcpServer>` whose accept loop has been aborted by
/// `retire_brain_session` or other shutdown paths.
async fn cache_or_start<K, V, F, Fut, E, A>(
    map: &DashMap<K, Arc<V>>,
    key: K,
    start: F,
    is_alive: A,
) -> Result<Arc<V>, E>
where
    K: Eq + std::hash::Hash + Clone,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Arc<V>, E>>,
    A: Fn(&V) -> bool,
{
    if let Some(existing) = map.get(&key) {
        if is_alive(existing.value().as_ref()) {
            return Ok(Arc::clone(existing.value()));
        }
        // Drop the read guard before mutating.
        drop(existing);
        map.remove(&key);
    }
    let candidate = start().await?;
    use dashmap::mapref::entry::Entry;
    match map.entry(key) {
        Entry::Occupied(mut slot) => {
            // A racing caller already inserted. Reuse if alive; else
            // overwrite with the freshly minted candidate.
            if is_alive(slot.get().as_ref()) {
                Ok(Arc::clone(slot.get()))
            } else {
                slot.insert(Arc::clone(&candidate));
                Ok(candidate)
            }
        }
        Entry::Vacant(slot) => {
            slot.insert(Arc::clone(&candidate));
            Ok(candidate)
        }
    }
}

#[cfg(test)]
mod worker_mcp_cache_tests {
    //! Phase 5 / Task 25/26 — cache contract for
    //! [`Orchestrator::ensure_worker_mcp_server`]. The full helper boots a
    //! `WorkerMcpServer` whose construction needs production-grade deps
    //! (PmService, FeatureGate, PlanResolver, …); rather than wire all of
    //! that here we exercise the underlying [`cache_or_start`] generic that
    //! `ensure_worker_mcp_server` delegates to.

    use super::cache_or_start;
    use dashmap::DashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// `is_alive` is a no-op (always true) — exercises the steady-state
    /// "everything's healthy" branch.
    fn always_alive<V>(_: &V) -> bool {
        true
    }

    #[tokio::test]
    async fn double_call_returns_same_arc_and_starts_once() {
        let map: DashMap<String, Arc<u32>> = DashMap::new();
        let starts = Arc::new(AtomicUsize::new(0));

        let starts_a = Arc::clone(&starts);
        let first = cache_or_start::<_, _, _, _, (), _>(
            &map,
            "brain-1".into(),
            || async move {
                starts_a.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(42u32))
            },
            always_alive,
        )
        .await
        .expect("first ensure must succeed");

        let starts_b = Arc::clone(&starts);
        let second = cache_or_start::<_, _, _, _, (), _>(
            &map,
            "brain-1".into(),
            || async move {
                starts_b.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(99u32))
            },
            always_alive,
        )
        .await
        .expect("second ensure must succeed");

        assert!(
            Arc::ptr_eq(&first, &second),
            "double-call must return the same Arc (no duplicate boot)"
        );
        assert_eq!(*first, 42, "cached value must be the first-boot value");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "starter must run exactly once across the two calls"
        );
        assert_eq!(map.len(), 1, "exactly one entry per brain session");
    }

    #[tokio::test]
    async fn distinct_keys_get_distinct_values() {
        let map: DashMap<String, Arc<u32>> = DashMap::new();

        let a = cache_or_start::<_, _, _, _, (), _>(
            &map,
            "brain-a".into(),
            || async { Ok(Arc::new(1u32)) },
            always_alive,
        )
        .await
        .expect("brain-a");
        let b = cache_or_start::<_, _, _, _, (), _>(
            &map,
            "brain-b".into(),
            || async { Ok(Arc::new(2u32)) },
            always_alive,
        )
        .await
        .expect("brain-b");

        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(map.len(), 2);
    }

    #[tokio::test]
    async fn start_failure_does_not_populate_cache() {
        let map: DashMap<String, Arc<u32>> = DashMap::new();
        let err = cache_or_start::<_, u32, _, _, &'static str, _>(
            &map,
            "brain-1".into(),
            || async { Err("boom") },
            always_alive,
        )
        .await
        .expect_err("must propagate starter error");
        assert_eq!(err, "boom");
        assert!(
            map.is_empty(),
            "failed start must leave the cache untouched"
        );
    }

    /// Phase 5 / Task 26 — cache liveness check. A cached entry whose
    /// `is_alive` returns `false` (modeling a `WorkerMcpServer` whose
    /// accept loop has been aborted) MUST be evicted and the starter
    /// rerun. Otherwise retries hand back a stale URL → 502.
    #[tokio::test]
    async fn dead_cached_entry_evicted_and_rebooted() {
        struct Probe {
            id: u32,
            alive: AtomicBool,
        }

        let map: DashMap<String, Arc<Probe>> = DashMap::new();
        let starts = Arc::new(AtomicUsize::new(0));

        let starts_a = Arc::clone(&starts);
        let first = cache_or_start::<_, _, _, _, (), _>(
            &map,
            "brain-1".into(),
            || async move {
                starts_a.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(Probe {
                    id: 1,
                    alive: AtomicBool::new(true),
                }))
            },
            |p: &Probe| p.alive.load(Ordering::SeqCst),
        )
        .await
        .expect("first start");
        assert_eq!(first.id, 1);
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        // Simulate the accept loop being aborted (e.g. retire_brain_session)
        // — the cached Arc is still in the map but no longer functional.
        first.alive.store(false, Ordering::SeqCst);

        let starts_b = Arc::clone(&starts);
        let second = cache_or_start::<_, _, _, _, (), _>(
            &map,
            "brain-1".into(),
            || async move {
                starts_b.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(Probe {
                    id: 2,
                    alive: AtomicBool::new(true),
                }))
            },
            |p: &Probe| p.alive.load(Ordering::SeqCst),
        )
        .await
        .expect("reboot after cache eviction");

        assert_eq!(
            second.id, 2,
            "dead cache entry must be evicted and starter re-run"
        );
        assert!(
            !Arc::ptr_eq(&first, &second),
            "must not return the dead Arc"
        );
        assert_eq!(
            starts.load(Ordering::SeqCst),
            2,
            "starter must run a second time after eviction"
        );
        assert_eq!(map.len(), 1, "exactly one live entry remains");
    }
}

#[cfg(test)]
mod worker_mcp_dispatch_tests {
    //! Phase 5 / Task 26 — dispatch-site gating for worker `mcp_servers`
    //! injection. Locks the contract that drives `execute_delegation`'s
    //! one-shot worker MCP resolution: the helper either returns an
    //! empty vec (preserving the historical "Workers get no MCP servers"
    //! contract) or a single `spur-worker-mcp` entry whose URL embeds
    //! the per-delegation HMAC token.
    //!
    //! The fetch closure is stubbed so the test never touches a real
    //! `WorkerMcpServer` (which would need PmService / FeatureGate /
    //! PlanResolver to boot). The contract under test is the gating
    //! logic and URL assembly.

    use super::build_worker_mcp_servers_with;
    use agent_client_protocol::schema::McpServer;
    use spur_acp::DelegationDispatchError;

    #[tokio::test]
    async fn flag_none_defaults_on_and_runs_fetch() {
        let mut fetch_called = false;
        let result = build_worker_mcp_servers_with(None, || {
            fetch_called = true;
            async {
                Ok::<_, DelegationDispatchError>((
                    "http://127.0.0.1:54321/mcp".into(),
                    "tok-default".into(),
                ))
            }
        })
        .await
        .expect("None flag must succeed");
        assert_eq!(
            result.len(),
            1,
            "None flag must default-on and produce exactly 1 entry"
        );
        assert!(
            fetch_called,
            "fetch closure MUST run when flag is None (default-on)"
        );
    }

    #[tokio::test]
    async fn flag_some_false_emits_zero_entries_and_skips_fetch() {
        let mut fetch_called = false;
        let result = build_worker_mcp_servers_with(Some(false), || {
            fetch_called = true;
            async {
                Ok::<_, DelegationDispatchError>(("http://127.0.0.1:1/mcp".into(), "tok".into()))
            }
        })
        .await
        .expect("Some(false) flag must succeed");
        assert!(
            result.is_empty(),
            "Some(false) flag must produce zero entries"
        );
        assert!(
            !fetch_called,
            "fetch closure must NOT run when flag is Some(false)"
        );
    }

    #[tokio::test]
    async fn flag_some_true_emits_one_entry_with_token_in_url() {
        let result = build_worker_mcp_servers_with(Some(true), || async {
            Ok::<_, DelegationDispatchError>((
                "http://127.0.0.1:54321/mcp".into(),
                "tok-abc-123".into(),
            ))
        })
        .await
        .expect("Some(true) flag must succeed");
        assert_eq!(result.len(), 1, "flag-true must produce exactly 1 entry");
        match &result[0] {
            McpServer::Http(http) => {
                assert_eq!(
                    http.name, "spur-worker-mcp",
                    "entry must be named spur-worker-mcp"
                );
                assert!(
                    http.url.contains("?token=tok-abc-123"),
                    "URL must embed token: {}",
                    http.url
                );
                assert!(
                    http.url.starts_with("http://127.0.0.1:54321/mcp"),
                    "URL must start with the server URL: {}",
                    http.url
                );
            }
            other => panic!("expected McpServer::Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn flag_some_true_propagates_fetch_error() {
        let result = build_worker_mcp_servers_with::<_, _>(Some(true), || async {
            Err::<(String, String), _>(DelegationDispatchError::WorkerMcpUnavailable {
                reason: "stub: deps not configured".into(),
            })
        })
        .await;
        match result {
            Err(DelegationDispatchError::WorkerMcpUnavailable { reason }) => {
                assert!(reason.contains("stub: deps not configured"));
            }
            other => panic!("expected WorkerMcpUnavailable, got {other:?}"),
        }
    }
}
