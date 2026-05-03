//! T21: Background audit flusher task tests.
//!
//! Verifies that the per-`WorkerMcpServer` background task periodically scans
//! the read-aggregation buffer map and flushes idle entries, emitting
//! `ReadAggregate` sentinel comments.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use spur_acp::SpurEventBody;
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::PlanResolver;
use spur_mcp::plan::PlanState;
use spur_mcp::worker_server::{
    ReadAuditEntry, WorkerMcpDeps, WorkerMcpServer, WorkerMcpServerConfig,
};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;
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
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        panic!("br {args:?} failed (exit {}): {stderr}", out.status);
    }
}

async fn pm_service_fixture(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}

fn test_feature_gate() -> Arc<FeatureGate> {
    use std::collections::BTreeSet;
    let gate = FeatureGate::new(PolicyResolver::embedded());
    let pro_state =
        spur_license::LicenseState::active_validated(spur_license::Plan::Pro, BTreeSet::new());
    gate.update_state(&pro_state);
    Arc::new(gate)
}

struct NullSink;

impl McpEventSink for NullSink {
    fn emit(&self, _event: SpurEventBody) {}
}

struct NullPlanResolver;

#[async_trait]
impl PlanResolver for NullPlanResolver {
    async fn load_or_project_plan(&self, plan_id: &str) -> Result<Arc<TokioMutex<PlanState>>, String> {
        Err(format!("test resolver: unknown plan_id '{plan_id}'"))
    }
}

fn test_deps(pm: Arc<PmService>) -> WorkerMcpDeps {
    WorkerMcpDeps {
        pm_service: pm,
        feature_gate: test_feature_gate(),
        funnel: Arc::new(NullSink),
        plan_resolver: Arc::new(NullPlanResolver),
        reconciler_outcomes: Arc::new(
            TokioMutex::new(spur_mcp::plan::outcomes::OutcomeStore::default()),
        ),
        outcome_store: Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        repo_root: None,
    }
}

// ─── Warning capture helper ───────────────────────────────────────────────

#[derive(Clone, Default)]
struct CapturedWarnings {
    events: Arc<Mutex<Vec<String>>>,
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
        let mut visitor = StringVisitor::default();
        event.record(&mut visitor);
        self.events.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[derive(Default)]
struct StringVisitor(String);

impl Visit for StringVisitor {
    fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{value:?}"));
    }
}

// ─── T21: background flusher ────────────────────────────────────────────────

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flusher_emits_sentinel_for_stale_entry() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = pm
        .create_issue(IssueCreate {
            title: "flush sentinel test".into(),
            ..Default::default()
        })
        .await
        .expect("create issue");

    let config = WorkerMcpServerConfig {
        idle_threshold: Duration::from_millis(100),
        scan_interval: Duration::from_millis(50),
    };

    let server = WorkerMcpServer::start_with_config(
        "session-flush".into(),
        test_deps(Arc::clone(&pm)),
        config,
    )
    .await
    .expect("start must succeed");

    // Inject a stale buffer directly (ts=0 makes it immediately idle).
    let buf = server.inject_read_buffer_for_test("d-1");
    buf.append_for_test(ReadAuditEntry {
        tool_name: "get_issue".into(),
        target_issue_id: Some(issue_id.clone()),
        ts: 0,
    });

    // Advance virtual time past scan_interval + idle_threshold + margin.
    tokio::time::advance(Duration::from_millis(200)).await;
    // Yield so the flusher task wakes and scans.
    tokio::task::yield_now().await;
    // PM I/O is a real subprocess; give it bounded wall-clock time to finish
    // without blocking the current-thread runtime.
    tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(100)))
        .await
        .unwrap();

    // Buffer should have been removed from the map.
    assert!(
        server.peek_read_buffer("d-1").is_none(),
        "idle buffer should have been flushed and removed"
    );

    // Verify a read-aggregate sentinel was written to the target issue.
    let comments = pm
        .advanced()
        .expect("advanced")
        .list_comments(&issue_id)
        .await
        .expect("list_comments");

    let sentinel = comments
        .iter()
        .find_map(|c| spur_mcp::plan::audit_sentinel::parse_comment(&c.body).and_then(|r| r.ok()));

    assert!(
        sentinel.is_some(),
        "expected read-aggregate sentinel comment, found: {comments:?}"
    );

    let sentinel = sentinel.unwrap();
    assert_eq!(sentinel.kind_str(), "read-aggregate");

    server.shutdown(Duration::from_secs(5)).await;
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn flusher_exits_within_1s_of_cancellation() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;

    let config = WorkerMcpServerConfig {
        idle_threshold: Duration::from_secs(30),
        scan_interval: Duration::from_secs(10),
    };

    let server =
        WorkerMcpServer::start_with_config("session-flush-cancel".into(), test_deps(pm), config)
            .await
            .expect("start must succeed");

    let start = tokio::time::Instant::now();
    server.shutdown(Duration::from_secs(5)).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "shutdown (including flusher exit) should complete within 2s, took {elapsed:?}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn flusher_warns_when_all_entries_have_no_target_issue_id() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");

    let warnings = CapturedWarnings::default();
    let _guard = tracing::subscriber::set_default(warnings.clone());

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = pm_service_fixture(dir.path()).await;

    let config = WorkerMcpServerConfig {
        idle_threshold: Duration::from_millis(100),
        scan_interval: Duration::from_millis(50),
    };

    let server = WorkerMcpServer::start_with_config(
        "session-flush-lossy".into(),
        test_deps(pm),
        config,
    )
    .await
    .expect("start must succeed");

    // Inject a buffer where every entry has target_issue_id=None (e.g. list_issues).
    let buf = server.inject_read_buffer_for_test("d-lossy");
    buf.append_for_test(ReadAuditEntry {
        tool_name: "list_issues".into(),
        target_issue_id: None,
        ts: 0,
    });
    buf.append_for_test(ReadAuditEntry {
        tool_name: "list_issues".into(),
        target_issue_id: None,
        ts: 1,
    });

    // Advance virtual time so the flusher considers the buffer idle.
    tokio::time::advance(Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        warnings.contains("ReadAggregate audit dropped"),
        "expected warning about dropped audit due to missing target_issue_id, got: {:?}",
        warnings.events.lock().unwrap()
    );

    server.shutdown(Duration::from_secs(5)).await;
}
