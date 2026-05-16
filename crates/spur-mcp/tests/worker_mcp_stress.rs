//! Phase 6 / Tasks 41, 42 — operational-resilience tests for the worker MCP
//! subsystem.
//!
//! Two cases:
//!
//! * `concurrency_n8_dedupes_server_and_summarizes_each_delegation` — N=8
//!   concurrent `ensure_worker_mcp_server` calls in the same `BrainSession`
//!   collapse to a single server (lazy `cache_or_start` dedupe). Each of the
//!   8 simulated workers issues 10 `get_issue` JSON-RPC calls. Asserts: all
//!   80 calls succeed, every delegation summary records `calls_total == 10`,
//!   the audit-sentinel pipeline persists every entry (sum of dedup'd
//!   `ReadAggregate.entries.len()` across delegations >= 80), no panic / no
//!   deadlock within 60 s.
//!
//! * `flush_failure_emits_summary_with_errors` — inject a flush-channel-closed
//!   condition (server shutdown drops the audit flusher's receiver) so
//!   `flush_delegation` returns `FlushDelegationError::ChannelClosed`.
//!   Asserts: (a) `flush_delegation` completes without blocking; (b)
//!   `WorkerMcpDelegationSummary` still fires with `errors > 0` (`mark_error`
//!   bumped the delegation-level counter via the `extra_errors` field).
//!
//! ## Scoping note for Test 2
//!
//! This test verifies the funnel-side outcome (`errors > 0` on the summary
//! event) when `flush_delegation` fails with `ChannelClosed`. It does **not**
//! verify the orchestrator-side production emitter
//! `spur-core::orchestrator::emit_flush_failed_audit_sentinel`, which is
//! private to spur-core and only fires inside the full
//! `flush_then_emit_completed` dispatch path (requires a real ACP-subprocess
//! worker to drive). That orchestrator-side sentinel emission is verified
//! separately at the spur-core integration layer (filed as br-kdt for the
//! real-ACP-subprocess e2e test). The audit-sentinel encode→write→parse
//! contract is already covered by `audit_sentinel_round_trip.rs`, so we do
//! not re-verify it here via a manual sentinel write.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    model::CallToolRequestParams,
    service::ServiceError,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::Value;
use spur_acp::config::SpurConfig;
use spur_acp::types::SessionId;
use spur_acp::{BrainSessionId, SpurEvent, SpurEventBody};
use spur_core::Orchestrator;
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan as LicensePlan};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::worker_server::{DelegationContext, FlushDelegationError};
use spur_mcp::McpCallbackServer;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use tokio::sync::broadcast;

const N_WORKERS: usize = 8;
const CALLS_PER_WORKER: usize = 10;

fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&LicenseState::active_validated(LicensePlan::Pro, features));
    gate
}

fn init_repo(repo: &Path) {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command failed to spawn");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@spur"]);
    git(&["config", "user.name", "spur-test"]);
    std::fs::write(repo.join("README.md"), "seed\n").expect("seed README");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "seed"]);
}

fn attach_test_beads(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create .beads");
    w.copy_db_to(&beads_dir);
}

async fn call_tool_with_bearer(url: &str, token: &str, name: &str, arguments: Value) -> Value {
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(token);
    let client =
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .expect("rmcp client initialize");
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = arguments.as_object().cloned();
    let response = match client.call_tool(request).await {
        Ok(result) => {
            let payload = result
                .structured_content
                .clone()
                .unwrap_or_else(|| serde_json::to_value(result).expect("serialize result"));
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": payload
            })
        }
        Err(error) => service_error_response(error),
    };
    drop(client);
    response
}

fn service_error_response(error: ServiceError) -> Value {
    let (code, message) = match &error {
        ServiceError::McpError(mcp_error) => {
            (i64::from(mcp_error.code.0), mcp_error.message.to_string())
        }
        _ => (-32603, error.to_string()),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

/// Build an `Orchestrator` + `McpCallbackServer` rooted at `repo` with the
/// Pro feature gate active so `emit_read_aggregate` is not gated out.
#[allow(clippy::arc_with_non_send_sync)]
async fn build_orch_and_brain(
    repo: &Path,
    brain_session_id: &BrainSessionId,
) -> (Arc<Orchestrator>, Arc<PmService>, Arc<McpCallbackServer>) {
    let pm = Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new")
            .expect("expected Some(PmService)"),
    );
    let feature_gate = pro_feature_gate();
    let orch = Orchestrator::new(repo.into(), SpurConfig::default(), Some(feature_gate))
        .expect("Orchestrator::new")
        .with_pm_service(Arc::clone(&pm));
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mcp_server, _channel) = McpCallbackServer::new(
        Some(brain_session_id),
        Some(Arc::clone(&pm)),
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        community_feature_gate(),
    );
    (Arc::new(orch), pm, Arc::new(mcp_server))
}

/// Drain `rx` and return all `WorkerMcpDelegationSummary` events whose
/// `delegation_id` starts with `id_prefix`, until either `expected` of them
/// have been seen or `timeout` elapses.
async fn collect_summaries(
    rx: &mut broadcast::Receiver<SpurEvent>,
    id_prefix: &str,
    expected: usize,
    timeout: Duration,
) -> HashMap<String, (u64, u64, HashMap<String, u64>)> {
    let mut found: HashMap<String, (u64, u64, HashMap<String, u64>)> = HashMap::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while found.len() < expected {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if let SpurEventBody::WorkerMcpDelegationSummary {
                    delegation_id,
                    calls_total,
                    calls_by_tool,
                    errors,
                    ..
                } = event.body
                {
                    if delegation_id.starts_with(id_prefix) {
                        let by_tool: HashMap<String, u64> = calls_by_tool.into_iter().collect();
                        found.insert(delegation_id, (calls_total, errors, by_tool));
                    }
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_n8_dedupes_server_and_summarizes_each_delegation() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let dir = TempDir::new().expect("tempdir");
        init_repo(dir.path());
        let beads = TestBeadsWorkspace::init();
        attach_test_beads(dir.path(), &beads);

        let brain_session_id =
            BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()));
        let (orch, pm, mcp_server) = build_orch_and_brain(dir.path(), &brain_session_id).await;
        let mut events_rx = orch.subscribe();

        // ── (a) Concurrency dedupe — N concurrent `ensure_worker_mcp_server`. ──
        // `cache_or_start` must collapse all callers to one boot. We assert
        // dedupe via `Arc::ptr_eq`: every returned `Arc<WorkerMcpServer>`
        // must point to the same allocation, which is equivalent to the
        // private `worker_mcp_servers` DashMap holding exactly one entry
        // for this brain (the only producer of those Arcs is `cache_or_start`).
        //
        // We use `futures::future::join_all` rather than `tokio::spawn` because
        // `Orchestrator` is `!Sync` (internal `RefCell<LruCache<...>>`), so its
        // futures cannot be sent across threads. Cooperative concurrency still
        // exercises the dedupe race: every call awaits inside `start()` (TCP
        // listener bind), so all N futures reach the `map.get(&key)` miss
        // before any of them inserts, then race on `map.entry(key)` — the
        // exact "loser" scenario `cache_or_start`'s `Occupied`-arm contract
        // is designed to handle.
        let ensure_futs = (0..N_WORKERS).map(|_| {
            let brain = brain_session_id.clone();
            let mcp = Arc::clone(&mcp_server);
            let orch = Arc::clone(&orch);
            async move { orch.ensure_worker_mcp_server(&brain, mcp).await }
        });
        let servers: Vec<_> = futures::future::join_all(ensure_futs)
            .await
            .into_iter()
            .map(|r| r.expect("ensure_worker_mcp_server must succeed"))
            .collect();
        for (i, s) in servers.iter().enumerate().skip(1) {
            assert!(
                Arc::ptr_eq(&servers[0], s),
                "server #{i} must point to the same Arc as #0 (concurrent ensure must dedupe)"
            );
        }
        // A second sequential ensure must also return the same Arc — re-verifies
        // the cache after the parallel boot settles.
        let again = orch
            .ensure_worker_mcp_server(&brain_session_id, Arc::clone(&mcp_server))
            .await
            .expect("ensure_worker_mcp_server (sequential)");
        assert!(
            Arc::ptr_eq(&servers[0], &again),
            "sequential ensure after parallel boot must reuse the cached server"
        );

        let server = Arc::clone(&servers[0]);
        let server_url = server.url();

        // ── Register N delegations + seed N issues. ─────────────────────
        let mut delegations: Vec<(String, String, String)> = Vec::with_capacity(N_WORKERS);
        for i in 0..N_WORKERS {
            let did = format!("d-stress-{i}");
            server.register_delegation(
                did.clone(),
                DelegationContext {
                    enable_worker_progress: false,
                },
            );
            let token = server.issue_token(&did, Duration::from_secs(60));

            let issue_id = pm
                .create_issue(IssueCreate {
                    title: format!("stress target {i}"),
                    description: Some(format!("worker {i} body")),
                    issue_type: Some("task".into()),
                    ..Default::default()
                })
                .await
                .expect("create issue");
            delegations.push((did, issue_id, token));
        }

        // ── (b) 8 workers × 10 calls in parallel. ───────────────────────
        let peak_active = Arc::new(AtomicU32::new(0));
        let stop_sampling = Arc::new(AtomicBool::new(false));
        let observer_server = Arc::clone(&server);
        let observer_peak = Arc::clone(&peak_active);
        let observer_stop = Arc::clone(&stop_sampling);
        let observer = tokio::spawn(async move {
            while !observer_stop.load(Ordering::Relaxed) {
                let observed = observer_server.active_count();
                let mut current = observer_peak.load(Ordering::Relaxed);
                while observed > current {
                    match observer_peak.compare_exchange(
                        current,
                        observed,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(actual) => current = actual,
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });

        let mut call_handles = Vec::with_capacity(N_WORKERS);
        for (_, issue_id, token) in &delegations {
            let issue_id = issue_id.clone();
            let token = token.clone();
            let server_url = server_url.clone();
            call_handles.push(tokio::spawn(async move {
                let mut results = Vec::with_capacity(CALLS_PER_WORKER);
                for _ in 0..CALLS_PER_WORKER {
                    let resp = call_tool_with_bearer(
                        &server_url,
                        &token,
                        "get_issue",
                        serde_json::json!({ "id": &issue_id }),
                    )
                    .await;
                    results.push(resp);
                }
                results
            }));
        }

        let mut total_successful = 0usize;
        for handle in call_handles {
            let results = handle.await.expect("calls task joins");
            for r in results {
                assert!(
                    r.get("error").is_none() || r["error"].is_null(),
                    "get_issue must succeed under concurrency, got: {r}"
                );
                assert!(r["result"]["id"].is_string(), "result must include id: {r}");
                total_successful += 1;
            }
        }
        assert_eq!(
            total_successful,
            N_WORKERS * CALLS_PER_WORKER,
            "all 80 calls must succeed"
        );
        stop_sampling.store(true, Ordering::Relaxed);
        observer.await.expect("active-count observer should join");

        let peak = peak_active.load(Ordering::SeqCst);
        assert!(
            peak >= 2,
            "active_count should peak above one under concurrent sessions (observed peak={peak})"
        );
        assert_eq!(
            server.active_count(),
            0,
            "active_count must return to zero after all calls complete"
        );

        // Flush every delegation so per-delegation summaries land and the
        // audit flusher writes one ReadAggregate sentinel per delegation.
        for (did, _, _) in &delegations {
            server
                .flush_delegation(did, "success")
                .await
                .expect("flush_delegation must succeed");
        }

        // ── (d) N summaries with calls_total==10, errors==0. ────────────
        let summaries = collect_summaries(
            &mut events_rx,
            "d-stress-",
            N_WORKERS,
            Duration::from_secs(20),
        )
        .await;
        assert_eq!(
            summaries.len(),
            N_WORKERS,
            "must observe {N_WORKERS} summaries; got: {summaries:?}"
        );
        for (did, (total, errors, by_tool)) in &summaries {
            assert_eq!(
                *total, CALLS_PER_WORKER as u64,
                "delegation {did} must record {CALLS_PER_WORKER} calls (got {total})"
            );
            assert_eq!(
                *errors, 0,
                "delegation {did} must have errors == 0 (got {errors})"
            );
            assert_eq!(
                by_tool.get("get_issue").copied().unwrap_or(0),
                CALLS_PER_WORKER as u64,
                "delegation {did} must attribute all calls to get_issue: {by_tool:?}"
            );
        }

        // ── (c) Sum of dedup'd ReadAggregate entries across delegations >= 80. ─
        //
        // Audit comments are written by the background flusher task asynchronously
        // after `flush_delegation` enqueues a FlushMessage. Concurrent appends on a
        // shared `Arc<ReadAuditBuffer>` plus the periodic idle-scan in
        // `audit_flusher_task` (default `scan_interval = 10s`,
        // `idle_threshold = 30s`) can in pathological scheduling produce
        // duplicate `ReadAggregate` sentinels for the same delegation_id.
        // `emit_read_aggregate` skips empty entries, but a duplicate carrying
        // the same drained payload could still land. To keep this stress test
        // robust on slow CI runners, we dedupe the observed sentinels by
        // `delegation_id`, keeping the one with the largest entry count
        // (i.e. the canonical drain produced by `flush_delegation`'s in-band
        // `take_entries` call), then sum and assert `>= 80`. Equality is the
        // wrong shape for a stress test — see the resubmission feedback.
        let adv = pm.advanced().expect("advanced PM");
        let mut canonical_per_delegation: HashMap<String, usize> = HashMap::new();
        let target_entries = N_WORKERS * CALLS_PER_WORKER;
        let poll_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            canonical_per_delegation.clear();
            for (_, issue_id, _) in &delegations {
                let comments = adv.list_comments(issue_id).await.unwrap_or_default();
                for c in &comments {
                    if let Some(Ok(AuditSentinelKind::ReadAggregate {
                        delegation_id,
                        entries,
                    })) = audit_sentinel::parse_comment(&c.body)
                    {
                        let prev = canonical_per_delegation
                            .get(&delegation_id)
                            .copied()
                            .unwrap_or(0);
                        if entries.len() > prev {
                            canonical_per_delegation.insert(delegation_id, entries.len());
                        }
                    }
                }
            }
            let total: usize = canonical_per_delegation.values().sum();
            if total >= target_entries {
                break;
            }
            if tokio::time::Instant::now() >= poll_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let total_entries: usize = canonical_per_delegation.values().sum();
        assert!(
            total_entries >= target_entries,
            "dedup'd audit-sentinel entry count must be >= {target_entries} \
             (got {total_entries}, per-delegation: {canonical_per_delegation:?})"
        );
        // Sanity: we expect exactly one canonical sentinel per delegation
        // since the test's call/flush sequencing avoids the append-after-drain
        // race that can produce duplicates.
        assert_eq!(
            canonical_per_delegation.len(),
            N_WORKERS,
            "must observe a ReadAggregate sentinel for each of {N_WORKERS} delegations \
             (got: {canonical_per_delegation:?})"
        );

        server.shutdown(Duration::from_secs(5)).await;
    })
    .await
    .expect("test must finish within 60s wall-clock budget");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_failure_emits_summary_with_errors() {
    tokio::time::timeout(Duration::from_secs(60), async {
        const DELEGATION_ID: &str = "d-flush-fail";

        let dir = TempDir::new().expect("tempdir");
        init_repo(dir.path());
        let beads = TestBeadsWorkspace::init();
        attach_test_beads(dir.path(), &beads);

        let brain_session_id =
            BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440001".into()));
        let (orch, pm, mcp_server) = build_orch_and_brain(dir.path(), &brain_session_id).await;
        let mut events_rx = orch.subscribe();

        let server = orch
            .ensure_worker_mcp_server(&brain_session_id, Arc::clone(&mcp_server))
            .await
            .expect("ensure_worker_mcp_server");

        server.register_delegation(
            DELEGATION_ID.into(),
            DelegationContext {
                enable_worker_progress: false,
            },
        );

        let token = server.issue_token(DELEGATION_ID, Duration::from_secs(60));
        let server_url = server.url();

        // Seed a target issue + run one successful read so the audit buffer
        // has work the flusher would emit. Without an entry, `flush_delegation`
        // skips the `flush_tx.send` call entirely and never observes the
        // closed-channel condition.
        let issue_id = pm
            .create_issue(IssueCreate {
                title: "flush-failure target".into(),
                description: Some("worker reads this body".into()),
                issue_type: Some("task".into()),
                ..Default::default()
            })
            .await
            .expect("create issue");

        let response = call_tool_with_bearer(
            &server_url,
            &token,
            "get_issue",
            serde_json::json!({ "id": &issue_id }),
        )
        .await;
        assert!(
            response.get("error").is_none() || response["error"].is_null(),
            "seed get_issue must succeed, got: {response}"
        );

        // ── INJECTION: tear down the audit flusher so its receiver drops. ─
        // After `shutdown` joins the flusher task, `flush_tx.send` returns
        // `Err(SendError(_))` and `flush_delegation` surfaces
        // `FlushDelegationError::ChannelClosed`. The accept loop is also
        // gone — that's fine, no more JSON-RPC calls are needed.
        //
        // Note: this is an artificial injection. Real PM-unreachable scenarios
        // (network partition to a remote PM backend) manifest as reqwest
        // timeouts inside `emit_read_aggregate`, not as `ChannelClosed` from
        // `flush_delegation`. Reqwest-side failures should be covered by a
        // separate test (filed as follow-up).
        let outcome = Arc::clone(&server).shutdown(Duration::from_secs(5)).await;
        assert!(
            outcome.drained,
            "test setup: server must drain cleanly before injection (outcome={outcome:?})"
        );

        // (a) `flush_delegation` returns Err — but does not block. The
        // `tokio::time::timeout` outer wrapper catches a hang; an
        // additional inner timeout is unnecessary because the call has no
        // await points after the channel-closed branch.
        let flush_result = server.flush_delegation(DELEGATION_ID, "success").await;
        assert!(
            matches!(flush_result, Err(FlushDelegationError::ChannelClosed)),
            "flush_delegation must return ChannelClosed under injection, got: {flush_result:?}"
        );

        // (b) `WorkerMcpDelegationSummary` still fires with `errors > 0`
        // because `flush_delegation` invokes `mark_error()` on the
        // delegation guard before dropping it (`worker_server.rs::flush_delegation`
        // channel-closed branch sets `flush_err`, then triggers `guard.mark_error()`
        // which bumps the `extra_errors` AtomicU64 counter on the
        // `DelegationDispatchGuard`). The guard's `Drop` impl emits the
        // summary event with `errors` including the extra count.
        let mut summaries =
            collect_summaries(&mut events_rx, DELEGATION_ID, 1, Duration::from_secs(10)).await;
        let (calls_total, errors, _by_tool) = summaries
            .remove(DELEGATION_ID)
            .expect("WorkerMcpDelegationSummary must be emitted even on flush failure");
        assert!(
            calls_total >= 1,
            "summary calls_total must include the seeded get_issue call (got {calls_total})"
        );
        assert!(
            errors > 0,
            "summary must reflect flush-channel failure via mark_error (errors={errors})"
        );
    })
    .await
    .expect("test must finish within 60s wall-clock budget");
}
