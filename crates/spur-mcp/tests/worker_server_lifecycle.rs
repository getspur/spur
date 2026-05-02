use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use spur_acp::SpurEventBody;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{DelegationContext, WorkerMcpDeps, WorkerMcpServer};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tracing::field::{Field, Visit};

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !out.status.success() {
        panic!("br {args:?} failed (exit {})", out.status);
    }
}

async fn test_pm_service_empty(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}

fn test_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

struct NullSink;

impl McpEventSink for NullSink {
    fn emit(&self, _event: SpurEventBody) {}
}

fn test_funnel() -> Arc<dyn McpEventSink> {
    Arc::new(NullSink)
}

struct NullPlanResolver;

#[async_trait]
impl PlanResolver for NullPlanResolver {
    async fn load_or_project_plan(&self, plan_id: &str) -> Result<Arc<Mutex<PlanState>>, String> {
        Err(format!("test resolver: unknown plan_id '{plan_id}'"))
    }
}

fn test_deps(pm: Arc<PmService>) -> WorkerMcpDeps {
    WorkerMcpDeps {
        pm_service: pm,
        feature_gate: test_feature_gate(),
        funnel: test_funnel(),
        plan_resolver: Arc::new(NullPlanResolver),
        reconciler_outcomes: Arc::new(Mutex::new(
            spur_mcp::plan::outcomes::OutcomeStore::default(),
        )),
        outcome_store: Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        repo_root: None,
    }
}

#[tokio::test]
async fn start_binds_listener_and_returns_url() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = test_pm_service_empty(dir.path()).await;
    let server = WorkerMcpServer::start("session-1".into(), test_deps(pm))
        .await
        .expect("start must succeed");

    let url = server.url();
    assert!(url.starts_with("http://127.0.0.1:"), "url: {url}");
    assert!(url.contains("/mcp"), "url: {url}");

    // Short timeout so post-shutdown probe fails fast even if the port were
    // somehow still alive.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("client");

    let resp = client
        .get(&url)
        .send()
        .await
        .expect("GET reaches the listener");
    assert!(
        resp.status().is_client_error() || resp.status() == 200,
        "unexpected status: {}",
        resp.status()
    );

    server.shutdown(Duration::from_secs(5)).await;

    // Cancellation must actually close the listener — a follow-up probe
    // should return Err (connection refused / timeout), not a 4xx response.
    let after_shutdown = client.get(&url).send().await;
    assert!(
        after_shutdown.is_err(),
        "listener still reachable after shutdown: {after_shutdown:?}"
    );
}

// ─── T22: active_delegations counter + atomic shutdown drain ──────────────

/// Sink that records the maximum value of [`WorkerMcpServer::active_count`]
/// observed during dispatch. Holds a `Weak` server reference set after
/// `start()` to avoid the chicken-and-egg problem of `funnel` being part of
/// `WorkerMcpDeps`.
struct ObservingSink {
    server: Arc<OnceLock<Weak<WorkerMcpServer>>>,
    max_seen: Arc<AtomicU32>,
    /// std-thread sleep so concurrent dispatchers overlap long enough for the
    /// counter to genuinely climb above 1. Each dispatcher runs on its own
    /// tokio worker, so a multi_thread runtime with N+ workers can park N
    /// dispatchers simultaneously.
    delay: Duration,
}

impl McpEventSink for ObservingSink {
    fn emit(&self, _event: SpurEventBody) {
        if let Some(weak) = self.server.get() {
            if let Some(s) = weak.upgrade() {
                let observed = s.active_count();
                let mut current = self.max_seen.load(Ordering::SeqCst);
                while observed > current {
                    match self.max_seen.compare_exchange(
                        current,
                        observed,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(actual) => current = actual,
                    }
                }
            }
        }
        // `block_in_place` lets tokio move other tasks off this worker so
        // concurrent dispatchers can actually overlap and our `max_seen`
        // CAS in this very `emit` sees more than 1.
        tokio::task::block_in_place(|| std::thread::sleep(self.delay));
    }

    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        self.emit(event);
        Ok(())
    }
}

/// Sink that holds each dispatcher in `emit` for a fixed wall-clock delay
/// — long enough for the test to spawn `shutdown()` and assert the drain is
/// blocking. Self-unblocks via `std::thread::sleep` so there is no
/// cross-thread synchronization required to release the dispatcher.
struct DelayingSink {
    started: Arc<AtomicU32>,
    delay: Duration,
}

impl McpEventSink for DelayingSink {
    fn emit(&self, _event: SpurEventBody) {
        self.started.fetch_add(1, Ordering::SeqCst);
        // `block_in_place` tells tokio's multi_thread runtime to move other
        // tasks off this worker before we block, so the test's polling task
        // can still run while the dispatcher is held here.
        tokio::task::block_in_place(|| std::thread::sleep(self.delay));
    }

    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        self.emit(event);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn active_count_tracks_concurrent_dispatch_entry_and_exit() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service_empty(dir.path()).await;

    let server_slot: Arc<OnceLock<Weak<WorkerMcpServer>>> = Arc::new(OnceLock::new());
    let max_seen = Arc::new(AtomicU32::new(0));
    let sink = Arc::new(ObservingSink {
        server: Arc::clone(&server_slot),
        max_seen: Arc::clone(&max_seen),
        delay: Duration::from_millis(300),
    });

    let mut deps = test_deps(pm);
    deps.funnel = sink;
    let server = WorkerMcpServer::start("session-active".into(), deps)
        .await
        .expect("start must succeed");
    server_slot
        .set(Arc::downgrade(&server))
        .map_err(|_| "set once")
        .unwrap();

    assert_eq!(server.active_count(), 0, "counter starts at zero");

    server.register_delegation(
        "d-1".into(),
        DelegationContext {
            enable_worker_progress: true,
        },
    );

    let n: u32 = 4;
    let url = server.url();
    let token = server.issue_token("d-1", Duration::from_secs(60));
    let request_url = format!("{url}?token={token}");

    let mut handles = Vec::with_capacity(n as usize);
    for i in 0..n {
        let request_url = request_url.clone();
        handles.push(tokio::spawn(async move {
            reqwest::Client::new()
                .post(&request_url)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": i,
                    "method": "tools/call",
                    "params": {"name": "report_progress", "arguments": {"message": "hi"}},
                }))
                .send()
                .await
        }));
    }

    for h in handles {
        let resp = h.await.expect("task joins").expect("request sends");
        assert!(resp.status().is_success() || resp.status().is_client_error());
    }

    assert_eq!(
        server.active_count(),
        0,
        "counter must return to zero after all dispatchers exit"
    );
    let observed = max_seen.load(Ordering::SeqCst);
    assert!(
        observed >= 2,
        "expected at least 2 concurrent dispatchers in flight, observed max {observed}"
    );

    server.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn shutdown_blocks_until_active_count_reaches_zero() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service_empty(dir.path()).await;

    // Dispatcher will be held inside `emit` for ~3 seconds. While held,
    // `active_count` stays at 1 and `shutdown()` must block in its drain
    // loop. After the sleep returns the dispatcher decrements via
    // `ActiveCallGuard::drop` and shutdown's drain polling completes.
    let started = Arc::new(AtomicU32::new(0));
    let dispatch_hold = Duration::from_millis(3000);
    let sink = Arc::new(DelayingSink {
        started: Arc::clone(&started),
        delay: dispatch_hold,
    });

    let mut deps = test_deps(pm);
    deps.funnel = sink;
    let server = WorkerMcpServer::start("session-drain".into(), deps)
        .await
        .expect("start must succeed");

    server.register_delegation(
        "d-1".into(),
        DelegationContext {
            enable_worker_progress: true,
        },
    );

    let token = server.issue_token("d-1", Duration::from_secs(60));
    let request_url = format!("{}?token={}", server.url(), token);

    let _req_handle = tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(&request_url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "report_progress", "arguments": {"message": "hi"}},
            }))
            .send()
            .await;
    });

    // Wait until the dispatcher's guard has incremented the counter. Polling
    // `active_count` directly is the most direct signal — if it reaches 1
    // we know the guard is alive.
    for _ in 0..400 {
        if server.active_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        server.active_count(),
        1,
        "in-flight dispatcher must increment counter (started={})",
        started.load(Ordering::SeqCst)
    );

    // Shutdown must drain — i.e., not return until the in-flight dispatcher
    // decrements `active_delegations` back to 0. Time it: the elapsed time
    // must be at least a meaningful fraction of `dispatch_hold` (we allow
    // slack so this is not flaky on slow CI).
    let shutdown_start = std::time::Instant::now();
    let outcome = Arc::clone(&server).shutdown(Duration::from_secs(10)).await;
    let elapsed = shutdown_start.elapsed();

    assert!(
        outcome.drained,
        "expected drained=true with a 10s deadline and ~3s dispatcher hold (active_at_deadline={})",
        outcome.active_at_deadline
    );
    assert_eq!(
        server.active_count(),
        0,
        "active_count must be 0 once shutdown returns"
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "shutdown returned in {elapsed:?} — should have waited on the in-flight dispatcher (~{dispatch_hold:?})"
    );
}

// ─── T24: drain-with-timeout shutdown ─────────────────────────────────────

/// Subscriber that captures `tracing::warn!` events. Mirrors the helper in
/// `worker_server::tests` so the test asserts the documented warning fires
/// without coupling to log formatting.
#[derive(Clone, Default)]
struct CapturedWarnings {
    events: Arc<StdMutex<Vec<String>>>,
}

impl CapturedWarnings {
    fn contains(&self, needle: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.contains(needle))
    }
}

impl tracing::Subscriber for CapturedWarnings {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::WARN
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut visitor = WarnVisitor::default();
        event.record(&mut visitor);
        self.events.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[derive(Default)]
struct WarnVisitor(String);

impl Visit for WarnVisitor {
    fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{value:?} "));
    }
    fn record_str(&mut self, _field: &Field, value: &str) {
        self.0.push_str(value);
        self.0.push(' ');
    }
    fn record_u64(&mut self, _field: &Field, value: u64) {
        self.0.push_str(&value.to_string());
        self.0.push(' ');
    }
}

#[tokio::test]
async fn shutdown_returns_drained_quickly_when_no_in_flight_dispatchers() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service_empty(dir.path()).await;
    let server = WorkerMcpServer::start("session-idle".into(), test_deps(pm))
        .await
        .expect("start must succeed");

    assert_eq!(server.active_count(), 0);

    let started = std::time::Instant::now();
    let outcome = Arc::clone(&server).shutdown(Duration::from_secs(5)).await;
    let elapsed = started.elapsed();

    assert!(
        outcome.drained,
        "expected drained=true on idle shutdown (outcome={outcome:?})"
    );
    assert_eq!(outcome.active_at_deadline, 0);
    assert!(
        elapsed < Duration::from_secs(2),
        "idle shutdown took {elapsed:?} — should return well under the 5s deadline"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn shutdown_warns_and_returns_undrained_when_deadline_elapses() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service_empty(dir.path()).await;

    // Hold the dispatcher far longer than the shutdown deadline so the drain
    // loop is forced to bail. 3s hold vs. 200ms deadline gives ~15x safety
    // margin even on slow CI.
    let started = Arc::new(AtomicU32::new(0));
    let dispatch_hold = Duration::from_millis(3000);
    let sink = Arc::new(DelayingSink {
        started: Arc::clone(&started),
        delay: dispatch_hold,
    });

    let mut deps = test_deps(pm);
    deps.funnel = sink;
    let server = WorkerMcpServer::start("session-deadline".into(), deps)
        .await
        .expect("start must succeed");

    server.register_delegation(
        "d-1".into(),
        DelegationContext {
            enable_worker_progress: true,
        },
    );

    let token = server.issue_token("d-1", Duration::from_secs(60));
    let request_url = format!("{}?token={}", server.url(), token);

    let _req_handle = tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(&request_url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "report_progress", "arguments": {"message": "hi"}},
            }))
            .send()
            .await;
    });

    // Wait until the dispatcher's guard has incremented the counter so we
    // know there is genuinely work to drain.
    for _ in 0..400 {
        if server.active_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        server.active_count(),
        1,
        "in-flight dispatcher must increment counter (started={})",
        started.load(Ordering::SeqCst)
    );

    let warnings = CapturedWarnings::default();
    let deadline = Duration::from_millis(200);

    let outcome = {
        let _guard = tracing::subscriber::set_default(warnings.clone());
        let shutdown_start = std::time::Instant::now();
        let outcome = Arc::clone(&server).shutdown(deadline).await;
        let elapsed = shutdown_start.elapsed();
        assert!(
            elapsed < dispatch_hold,
            "shutdown returned in {elapsed:?} — must bail before dispatch_hold ({dispatch_hold:?})"
        );
        outcome
    };

    assert!(
        !outcome.drained,
        "expected drained=false when deadline elapses with in-flight dispatchers (outcome={outcome:?})"
    );
    assert!(
        outcome.active_at_deadline >= 1,
        "expected active_at_deadline>=1 (got {})",
        outcome.active_at_deadline
    );
    assert!(
        warnings.contains("drain deadline elapsed"),
        "expected warning about drain deadline, got: {:?}",
        warnings.events.lock().unwrap()
    );
}
