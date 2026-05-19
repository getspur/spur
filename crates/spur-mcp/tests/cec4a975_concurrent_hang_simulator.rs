mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rand::{rngs::StdRng, Rng, SeedableRng};
use rusqlite::{params, Connection};
use serde_json::Value;
use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus, SessionId, SpurEventBody};
use spur_mcp::outcome_materializer::OutcomeMaterializer;
use spur_mcp::plan::outcomes::OutcomeStore;
use spur_mcp::plan::reconciler::{
    PreviewStrategy, Reconciler, ReconcilerConfig, ReconcilerDispatchCtx,
};
use spur_mcp::plan::{projector, PlanTaskEntry};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::{McpCallbackServer, McpEventSink};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};
use tokio_util::task::TaskTracker;

const SOURCE_BEADS_DIR: &str = "/Volumes/Projects/otobank/.beads";
const PLAN_ID: &str = "cec4a975-0614-4536-9fab-b2e0499c0f2a";
const EPIC_ISSUE_ID: &str = "bd-1ll";
const VERIFY_ISSUE_ID: &str = "bd-qs3";
const VERIFY_COMMENT_ID: i64 = 1356;
const VERIFY_DELEGATION_ID: &str = "0aefd1aa7bae4f02";
const COMPLETION_TASKS: [&str; 4] = ["m1-15", "m1-16", "m1-24", "m1-25"];
const BASE_RUNS_PER_VARIANT: usize = 3;
const COMBINED_RUNS_PER_VARIANT: usize = 10;
const BACKGROUND_STOP_TIMEOUT: Duration = Duration::from_secs(6);

static TEST_SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static TRACE_INIT: Once = Once::new();
static TRACE_SINK: OnceLock<Mutex<Option<TraceLines>>> = OnceLock::new();

type TraceLines = Arc<Mutex<Vec<String>>>;

#[derive(Clone, Copy, Debug)]
struct Variant {
    name: &'static str,
    reads: bool,
    writes: bool,
    reconciler_tick: bool,
}

#[derive(Debug)]
struct VariantRun {
    run: usize,
    observations: Vec<CompletionObservation>,
    background_hangs: Vec<String>,
    trace: Vec<String>,
}

#[derive(Debug)]
struct CompletionObservation {
    task_id: String,
    issue_id: String,
    delegation_id: String,
    elapsed_ms: u128,
    outcome: CompletionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionOutcome {
    Completed,
    Errored(String),
    Panicked(String),
    Hung,
}

#[derive(Clone, Debug)]
struct CompletionTarget {
    task_id: String,
    issue_id: String,
    attempt: u32,
    delegation_id: String,
}

struct BackgroundThread {
    name: String,
    done: mpsc::Receiver<()>,
}

#[derive(Clone)]
struct RecordingSink {
    events: Arc<Mutex<Vec<SpurEventBody>>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl McpEventSink for RecordingSink {
    fn emit(&self, event: SpurEventBody) {
        self.events.lock().expect("recording sink lock").push(event);
    }
}

#[derive(Clone)]
struct TraceMakeWriter;

struct TraceWriter {
    sink: Option<TraceLines>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceMakeWriter {
    type Writer = TraceWriter;

    fn make_writer(&'a self) -> Self::Writer {
        let sink = TRACE_SINK
            .get()
            .and_then(|slot| slot.lock().expect("trace sink lock").clone());
        TraceWriter { sink }
    }
}

impl Write for TraceWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(sink) = &self.sink {
            let line = String::from_utf8_lossy(buf).to_string();
            if line.contains("stage_")
                || line.contains("spur.pm.lock")
                || line.contains("completion_collector")
            {
                sink.lock().expect("trace lines lock").push(line);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct TraceCapture {
    lines: TraceLines,
}

impl TraceCapture {
    fn start() -> Self {
        install_trace_subscriber();
        let lines = Arc::new(Mutex::new(Vec::new()));
        *TRACE_SINK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("trace sink lock") = Some(Arc::clone(&lines));
        Self { lines }
    }

    fn finish(self) -> Vec<String> {
        *TRACE_SINK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("trace sink lock") = None;
        self.lines.lock().expect("trace lines lock").clone()
    }
}

fn install_trace_subscriber() {
    TRACE_INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(true)
            .with_level(true)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(TraceMakeWriter)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

async fn run_variant_test(variant: Variant) -> Result<()> {
    let _serial = TEST_SERIAL
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let runs = timeout(Duration::from_secs(60), async {
        let mut runs = Vec::new();
        for run in 0..runs_for_variant(variant) {
            let variant_run = run_variant_once(variant, run).await?;
            let stop_after_background_hang = !variant_run.background_hangs.is_empty();
            runs.push(variant_run);
            if stop_after_background_hang {
                break;
            }
        }
        Ok::<_, anyhow::Error>(runs)
    })
    .await
    .with_context(|| format!("variant {} exceeded 60s wall-clock", variant.name))??;

    report_variant(&variant, &runs);
    assert_stable(&variant, &runs);

    if let Some((run, hung)) = first_hang(&runs) {
        eprintln!(
            "CEC4A975 HANG REPRODUCED: variant={} run={} task={} exceeded 5s in completion collector body. Captured spur.reconciler.completion_collector trace: {:?}",
            variant.name, run, hung.task_id, runs[run].trace
        );
        // Reproduced hangs leave detached workers holding DB/PM locks, so fail fast
        // before another variant runs in a contaminated process.
        std::process::exit(101);
    }

    if let Some(run) = runs.iter().find(|run| !run.background_hangs.is_empty()) {
        eprintln!(
            "CEC4A975 BACKGROUND HANG REPRODUCED: variant={} run={} workers={:?}. Captured spur.reconciler.completion_collector trace: {:?}",
            variant.name, run.run, run.background_hangs, run.trace
        );
        // Reproduced hangs leave detached workers holding DB/PM locks, so fail fast
        // before another variant runs in a contaminated process.
        std::process::exit(101);
    }

    Ok(())
}

fn runs_for_variant(variant: Variant) -> usize {
    if variant.reads && variant.writes && variant.reconciler_tick {
        COMBINED_RUNS_PER_VARIANT
    } else {
        BASE_RUNS_PER_VARIANT
    }
}

async fn run_variant_once(variant: Variant, run: usize) -> Result<VariantRun> {
    let trace = TraceCapture::start();
    let fixture = TestFixture::new(run).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let mut background_threads = Vec::new();

    if variant.reads {
        background_threads.extend(spawn_read_workers(&fixture, Arc::clone(&stop), run));
    }
    if variant.writes {
        background_threads.extend(spawn_write_workers(&fixture, Arc::clone(&stop), run));
    }
    if variant.reconciler_tick {
        background_threads.push(spawn_reconciler_tick_worker(&fixture, Arc::clone(&stop)));
    }

    sleep(Duration::from_millis(75)).await;
    let observations = drive_completion_collectors(&fixture).await;
    let hung_detected = observations
        .iter()
        .any(|obs| obs.outcome == CompletionOutcome::Hung);
    sleep(Duration::from_millis(250)).await;
    stop.store(true, Ordering::SeqCst);
    sleep(Duration::from_millis(100)).await;
    let mut background_hung = false;
    let mut background_hangs = Vec::new();
    for thread in background_threads {
        if thread.done.recv_timeout(BACKGROUND_STOP_TIMEOUT).is_err() {
            background_hung = true;
            background_hangs.push(thread.name.clone());
            eprintln!(
                "[cec4a975] variant={} run={} BACKGROUND-HANG worker={} did not stop within {:?}",
                variant.name, run, thread.name, BACKGROUND_STOP_TIMEOUT
            );
        }
    }

    if hung_detected || background_hung {
        std::mem::forget(fixture);
    }

    Ok(VariantRun {
        run,
        observations,
        background_hangs,
        trace: trace.finish(),
    })
}

struct TestFixture {
    _repo: TempDir,
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    fast_forward: Arc<Notify>,
    event_sink: Arc<RecordingSink>,
    continuation_ctx: Arc<DetachedContinuationCtx>,
    materializer: Arc<OutcomeMaterializer>,
    outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>,
    brain_session_id: BrainSessionId,
    targets: Vec<CompletionTarget>,
}

impl TestFixture {
    async fn new(run: usize) -> Result<Self> {
        let repo = copy_frozen_otobank_beads_db()?;
        let pm = build_pm(repo.path(), run).await?;
        let feature_gate = common::server_builder::pro_feature_gate();
        let brain_session_id = BrainSessionId::new(SessionId::new());
        let event_sink = Arc::new(RecordingSink::new());
        let continuation_records = Arc::new(Mutex::new(Vec::<String>::new()));
        let continuation_ctx = recording_continuation_ctx(Arc::clone(&continuation_records));
        let outcome_store: Arc<dyn spur_blob_store::OutcomeStore> = Arc::new(
            spur_blob_store::FsOutcomeStore::new(repo.path().join(".spur-test-outcomes")),
        );
        let materializer = Arc::new(OutcomeMaterializer::new(Arc::clone(&outcome_store)));

        let (_server, _channel) = McpCallbackServer::new(
            Some(&brain_session_id),
            Some(Arc::clone(&pm)),
            Some(event_sink.clone() as Arc<dyn McpEventSink>),
            recording_continuation_ctx(continuation_records),
            outcome_store,
            Arc::clone(&feature_gate),
        );

        let projected = timeout(
            Duration::from_secs(5),
            projector::project_plan_from_beads(pm.as_ref(), PLAN_ID, feature_gate.as_ref()),
        )
        .await
        .context("initial project_plan_from_beads shape check timed out")??;
        verify_project_shape(&projected)?;
        let targets = completion_targets_from_projected(&projected, run)?;

        Ok(Self {
            _repo: repo,
            pm,
            feature_gate,
            fast_forward: Arc::new(Notify::new()),
            event_sink,
            continuation_ctx: Arc::new(continuation_ctx),
            materializer,
            outcomes: Arc::new(tokio::sync::Mutex::new(OutcomeStore::default())),
            brain_session_id,
            targets,
        })
    }
}

fn recording_continuation_ctx(records: Arc<Mutex<Vec<String>>>) -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(move |_, delegation_id| {
            let records = Arc::clone(&records);
            Box::pin(async move {
                records
                    .lock()
                    .expect("continuation records lock")
                    .push(delegation_id);
            })
        }),
    }
}

async fn drive_completion_collectors(fixture: &TestFixture) -> Vec<CompletionObservation> {
    let mut receivers = Vec::new();
    for target in fixture.targets.clone() {
        let timeout_target = target.clone();
        let pm = Arc::clone(&fixture.pm);
        let feature_gate = Arc::clone(&fixture.feature_gate);
        let fast_forward = Arc::clone(&fixture.fast_forward);
        let event_sink = Arc::clone(&fixture.event_sink);
        let continuation_ctx = Arc::clone(&fixture.continuation_ctx);
        let materializer = Arc::clone(&fixture.materializer);
        let outcomes = Arc::clone(&fixture.outcomes);
        let brain_session_id = fixture.brain_session_id.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("completion collector runtime");
            let observation = runtime.block_on(run_completion_collector_body(
                pm,
                feature_gate,
                fast_forward,
                event_sink,
                continuation_ctx,
                materializer,
                outcomes,
                brain_session_id,
                target,
            ));
            let _ = tx.send(observation);
        });
        receivers.push((timeout_target, rx));
    }

    let mut observations = Vec::new();
    for (target, rx) in receivers {
        let started = Instant::now();
        match tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5))).await {
            Ok(Ok(mut observation)) => {
                observation.elapsed_ms = started.elapsed().as_millis();
                observations.push(observation);
            }
            Ok(Err(mpsc::RecvTimeoutError::Timeout)) => {
                observations.push(CompletionObservation {
                    task_id: target.task_id,
                    issue_id: target.issue_id,
                    delegation_id: target.delegation_id,
                    elapsed_ms: started.elapsed().as_millis(),
                    outcome: CompletionOutcome::Hung,
                });
            }
            Ok(Err(mpsc::RecvTimeoutError::Disconnected)) => {
                observations.push(CompletionObservation {
                    task_id: target.task_id,
                    issue_id: target.issue_id,
                    delegation_id: target.delegation_id,
                    elapsed_ms: started.elapsed().as_millis(),
                    outcome: CompletionOutcome::Panicked(
                        "completion collector thread disconnected before send".to_string(),
                    ),
                });
            }
            Err(error) => {
                observations.push(CompletionObservation {
                    task_id: target.task_id,
                    issue_id: target.issue_id,
                    delegation_id: target.delegation_id,
                    elapsed_ms: started.elapsed().as_millis(),
                    outcome: CompletionOutcome::Errored(format!(
                        "recv_timeout join failed: {error}"
                    )),
                });
            }
        }
    }
    observations
}

#[allow(clippy::too_many_arguments)]
async fn run_completion_collector_body(
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    fast_forward: Arc<Notify>,
    event_sink: Arc<RecordingSink>,
    continuation_ctx: Arc<DetachedContinuationCtx>,
    materializer: Arc<OutcomeMaterializer>,
    outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>,
    brain_session_id: BrainSessionId,
    target: CompletionTarget,
) -> CompletionObservation {
    let started = Instant::now();
    let result = DelegationResult {
        status: DelegationStatus::Success,
        diff: Some(format!("simulated diff for {}", target.task_id)),
        diff_summary: None,
        summary: Some(format!(
            "cec4a975 concurrent simulator completed {}",
            target.task_id
        )),
        estimated_cost_usd: 0.0,
        worker_branch: Some(format!(
            "spur/worker/simulator/{}/{}",
            target.delegation_id, target.task_id
        )),
        artifact: None,
    };
    let fast_forward = Some(fast_forward);

    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        "stage_entered_persist"
    );
    let deferred = match spur_mcp::test_support::persist_worker_completion_and_notify_with_task_id(
        pm.as_ref(),
        &target.issue_id,
        feature_gate.as_ref(),
        PLAN_ID,
        &target.delegation_id,
        &fast_forward,
        &result,
        &brain_session_id,
        target.attempt,
        &materializer,
        None,
        Some(&target.task_id),
    )
    .await
    {
        Ok(deferred) => deferred,
        Err(error) => {
            return CompletionObservation {
                task_id: target.task_id,
                issue_id: target.issue_id,
                delegation_id: target.delegation_id,
                elapsed_ms: started.elapsed().as_millis(),
                outcome: CompletionOutcome::Errored(format!(
                    "persist_worker_completion_and_notify failed: {error:?}"
                )),
            };
        }
    };
    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        deferred_is_some = deferred.is_some(),
        "stage_completed_persist"
    );

    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        "stage_entered_project"
    );
    let projected =
        match projector::project_plan_from_beads(pm.as_ref(), PLAN_ID, feature_gate.as_ref()).await
        {
            Ok(projected) => projected,
            Err(error) => {
                return CompletionObservation {
                    task_id: target.task_id,
                    issue_id: target.issue_id,
                    delegation_id: target.delegation_id,
                    elapsed_ms: started.elapsed().as_millis(),
                    outcome: CompletionOutcome::Errored(format!(
                        "project_plan_from_beads failed: {error:?}"
                    )),
                };
            }
        };
    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        projected_task_count = projected.tasks.len(),
        "stage_completed_project"
    );

    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        "stage_entered_prune"
    );
    spur_mcp::test_support::prune_projected_terminal_task_outcomes_for_test(
        &outcomes,
        PLAN_ID,
        &projected.tasks,
    )
    .await;
    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        "stage_completed_prune"
    );

    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        "stage_entered_snapshot"
    );
    spur_mcp::plan::snapshot::emit_plan_snapshot(Some(event_sink.as_ref()), &projected);
    tracing::info!(
        target: "spur.reconciler.completion_collector",
        plan_id = %PLAN_ID,
        task_id = %target.task_id,
        delegation_id = %target.delegation_id,
        brain_session_id = %brain_session_id,
        attempt = target.attempt,
        "stage_completed_snapshot"
    );

    if let Some(deferred) = deferred {
        tracing::info!(
            target: "spur.reconciler.completion_collector",
            plan_id = %PLAN_ID,
            task_id = %target.task_id,
            delegation_id = %target.delegation_id,
            brain_session_id = %brain_session_id,
            attempt = target.attempt,
            "stage_entered_deliver"
        );
        deferred
            .deliver(Some(event_sink.as_ref()), continuation_ctx.as_ref())
            .await;
        tracing::info!(
            target: "spur.reconciler.completion_collector",
            plan_id = %PLAN_ID,
            task_id = %target.task_id,
            delegation_id = %target.delegation_id,
            brain_session_id = %brain_session_id,
            attempt = target.attempt,
            "stage_completed_deliver"
        );
    }

    CompletionObservation {
        task_id: target.task_id,
        issue_id: target.issue_id,
        delegation_id: target.delegation_id,
        elapsed_ms: started.elapsed().as_millis(),
        outcome: CompletionOutcome::Completed,
    }
}

fn spawn_read_workers(
    fixture: &TestFixture,
    stop: Arc<AtomicBool>,
    run: usize,
) -> Vec<BackgroundThread> {
    let mut rng = StdRng::seed_from_u64(0xcec4_a975_1000 + run as u64);
    (0..4)
        .map(|worker| {
            let pm = Arc::clone(&fixture.pm);
            let feature_gate = Arc::clone(&fixture.feature_gate);
            let stop = Arc::clone(&stop);
            let phase_ms = rng.gen_range(0..25);
            let (done_tx, done) = mpsc::channel();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("read worker runtime");
                std::thread::sleep(Duration::from_millis(phase_ms));
                while !stop.load(Ordering::SeqCst) {
                    let _ = runtime.block_on(projector::project_plan_from_beads(
                        pm.as_ref(),
                        PLAN_ID,
                        feature_gate.as_ref(),
                    ));
                    std::thread::sleep(Duration::from_millis(10 + worker * 3));
                }
                let _ = done_tx.send(());
            });
            BackgroundThread {
                name: format!("read-{worker}"),
                done,
            }
        })
        .collect()
}

fn spawn_write_workers(
    fixture: &TestFixture,
    stop: Arc<AtomicBool>,
    run: usize,
) -> Vec<BackgroundThread> {
    let mut rng = StdRng::seed_from_u64(0xcec4_a975_2000 + run as u64);
    (0..2)
        .map(|worker| {
            let pm = Arc::clone(&fixture.pm);
            let stop = Arc::clone(&stop);
            let issue_id = fixture.targets[worker % fixture.targets.len()].issue_id.clone();
            let phase_ms = rng.gen_range(0..35);
            let (done_tx, done) = mpsc::channel();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("write worker runtime");
                std::thread::sleep(Duration::from_millis(phase_ms));
                let mut iteration = 0_u64;
                while !stop.load(Ordering::SeqCst) {
                    let body = format!(
                        "[[spur-audit v1]]\n{{\"kind\":\"dispatch\",\"delegation_id\":\"{:016x}\",\"worker\":\"sim-writer-{worker}\",\"attempt\":{}}}",
                        0xcec4_0000_0000_0000_u64 + (worker as u64 * 1000) + iteration,
                        iteration + 1
                    );
                    if let Some(adv) = pm.advanced() {
                        let _ = runtime.block_on(adv.add_comment(&issue_id, &body));
                    }
                    iteration += 1;
                    std::thread::sleep(Duration::from_millis((20 + worker * 7) as u64));
                }
                let _ = done_tx.send(());
            });
            BackgroundThread {
                name: format!("write-{worker}"),
                done,
            }
        })
        .collect()
}

fn spawn_reconciler_tick_worker(fixture: &TestFixture, stop: Arc<AtomicBool>) -> BackgroundThread {
    let pm = Arc::clone(&fixture.pm);
    let feature_gate = Arc::clone(&fixture.feature_gate);
    let fast_forward = Arc::clone(&fixture.fast_forward);
    let materializer = Arc::clone(&fixture.materializer);
    let continuation_ctx = Arc::clone(&fixture.continuation_ctx);
    let event_sink = Arc::clone(&fixture.event_sink);
    let brain_session_id = fixture.brain_session_id.clone();
    let repo_root = fixture._repo.path().to_path_buf();
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("reconciler tick runtime");
        runtime.block_on(async move {
            let stop_for_drain = Arc::clone(&stop);
            let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(32);
            tokio::spawn(async move {
                while !stop_for_drain.load(Ordering::SeqCst) {
                    if timeout(Duration::from_millis(100), delegation_rx.recv())
                        .await
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        sleep(Duration::from_millis(25)).await;
                    }
                }
            });

            let dispatch = ReconcilerDispatchCtx {
                delegation_tx,
                task_tracker: TaskTracker::new(),
                brain_session_id,
                event_sink: Some(event_sink as Arc<dyn McpEventSink>),
                materializer,
                continuation_ctx,
            };
            let reconciler = Reconciler::new(
                ReconcilerConfig {
                    repo_root,
                    predispatch_preview: PreviewStrategy::AlwaysClean,
                    ..ReconcilerConfig::default()
                },
                pm,
                fast_forward,
                Some(dispatch),
                Some(PLAN_ID.to_string()),
                feature_gate,
            );
            while !stop.load(Ordering::SeqCst) {
                let _ = timeout(Duration::from_secs(5), reconciler.tick_once()).await;
                sleep(Duration::from_millis(100)).await;
            }
        });
        let _ = done_tx.send(());
    });
    BackgroundThread {
        name: "reconciler-tick".to_string(),
        done,
    }
}

fn copy_frozen_otobank_beads_db() -> Result<TempDir> {
    let source_dir = Path::new(SOURCE_BEADS_DIR);
    anyhow::ensure!(
        source_dir.join("beads.db").is_file(),
        "frozen otobank beads.db missing at {}",
        source_dir.display()
    );
    let temp = tempfile::tempdir().context("create temp repo")?;
    let beads_dir = temp.path().join(".beads");
    fs::create_dir_all(&beads_dir).context("create temp .beads directory")?;

    for name in ["beads.db", "beads.db-shm", "beads.db-wal"] {
        let source = source_dir.join(name);
        if source.exists() {
            fs::copy(&source, beads_dir.join(name))
                .with_context(|| format!("copy {} to temp .beads", source.display()))?;
        }
    }

    verify_frozen_copy(temp.path())?;
    Ok(temp)
}

fn verify_frozen_copy(repo: &Path) -> Result<()> {
    let db = repo.join(".beads/beads.db");
    let conn = Connection::open(&db).with_context(|| format!("open copied DB {}", db.display()))?;
    let (issue_id, body): (String, String) = conn
        .query_row(
            "SELECT issue_id, text FROM comments WHERE id = ?1",
            params![VERIFY_COMMENT_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .with_context(|| format!("find verification comment {VERIFY_COMMENT_ID}"))?;

    assert_eq!(issue_id, VERIFY_ISSUE_ID, "verification comment issue id");
    let json = body
        .strip_prefix("[[spur-audit v1]]")
        .context("verification comment missing spur-audit sentinel")?
        .trim();
    let audit: Value = serde_json::from_str(json).context("parse verification audit json")?;
    assert_eq!(audit["kind"], "completion");
    assert_eq!(audit["delegation_id"], VERIFY_DELEGATION_ID);
    assert_eq!(audit["completion_state"], "awaiting_review");

    let plan_issue_count: i64 = conn.query_row(
        "SELECT count(*) FROM issues i JOIN labels l ON i.id = l.issue_id WHERE l.label = ?1",
        params![spur_mcp::plan::labels::plan_id(PLAN_ID)],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        plan_issue_count >= 12,
        "plan {PLAN_ID} must have 12+ labeled issues including the epic"
    );
    let epic_count: i64 = conn.query_row(
        "SELECT count(*) FROM issues i JOIN labels l ON i.id = l.issue_id WHERE i.id = ?1 AND i.issue_type = 'epic' AND l.label = ?2",
        params![EPIC_ISSUE_ID, spur_mcp::plan::labels::plan_id(PLAN_ID)],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        epic_count == 1,
        "plan {PLAN_ID} epic {EPIC_ISSUE_ID} missing"
    );
    Ok(())
}

async fn build_pm(repo: &Path, run: usize) -> Result<Arc<spur_pm::PmService>> {
    let service = spur_pm::PmService::try_new_with_actor(
        None,
        true,
        false,
        repo,
        None,
        Some(format!("cec4a975-sim-{run}")),
    )
    .await
    .context("PmService::try_new_with_actor failed")?
    .context("expected beads-backed PmService")?;
    Ok(Arc::new(service))
}

fn verify_project_shape(projected: &spur_mcp::plan::PlanState) -> Result<()> {
    assert_eq!(projected.epic_id.as_deref(), Some(EPIC_ISSUE_ID));
    assert!(
        projected.tasks.len() >= 11,
        "plan {PLAN_ID} should have at least 11 tasks in this frozen fixture, got {}",
        projected.tasks.len()
    );
    for task_id in COMPLETION_TASKS {
        let found = projected
            .tasks
            .iter()
            .any(|task| task.spec.task_id == task_id);
        assert!(found, "projected plan missing task {task_id}");
    }
    Ok(())
}

fn completion_targets_from_projected(
    projected: &spur_mcp::plan::PlanState,
    run: usize,
) -> Result<Vec<CompletionTarget>> {
    let by_task: BTreeMap<&str, &PlanTaskEntry> = projected
        .tasks
        .iter()
        .map(|entry| (entry.spec.task_id.as_str(), entry))
        .collect();
    COMPLETION_TASKS
        .iter()
        .enumerate()
        .map(|(index, task_id)| {
            let entry = by_task
                .get(task_id)
                .with_context(|| format!("projected plan missing {task_id}"))?;
            let issue_id = entry
                .spec
                .issue_id
                .clone()
                .with_context(|| format!("task {task_id} missing issue id"))?;
            Ok(CompletionTarget {
                task_id: (*task_id).to_string(),
                issue_id,
                attempt: entry.attempt.saturating_add(1),
                delegation_id: format!(
                    "{:016x}",
                    0xcec4_0000_0000_0000_u64 + (run as u64 * 256) + index as u64
                ),
            })
        })
        .collect()
}

fn report_variant(variant: &Variant, runs: &[VariantRun]) {
    for run in runs {
        let hung: Vec<_> = run
            .observations
            .iter()
            .filter(|obs| obs.outcome == CompletionOutcome::Hung)
            .map(|obs| obs.task_id.as_str())
            .collect();
        if hung.is_empty() {
            eprintln!(
                "[cec4a975] variant={} run={} NO-HANG-REPRODUCED observations={} background_hangs={:?}",
                variant.name,
                run.run,
                format_observations(&run.observations),
                run.background_hangs
            );
            eprintln!(
                "[cec4a975] variant={} run={} captured_trace={:?}",
                variant.name,
                run.run,
                compact_trace(&run.trace)
            );
        } else {
            eprintln!(
                "[cec4a975] variant={} run={} HANG REPRODUCED tasks={hung:?} observations={} background_hangs={:?}",
                variant.name,
                run.run,
                format_observations(&run.observations),
                run.background_hangs
            );
            eprintln!(
                "[cec4a975] variant={} run={} captured_trace={:?}",
                variant.name,
                run.run,
                compact_trace(&run.trace)
            );
        }
    }
}

fn compact_trace(trace: &[String]) -> Vec<String> {
    trace
        .iter()
        .filter_map(|line| {
            let stage = line
                .split_whitespace()
                .find(|part| part.starts_with("stage_"))?;
            let task = line
                .split_whitespace()
                .find(|part| part.starts_with("task_id="))
                .unwrap_or("task_id=?");
            let delegation = line
                .split_whitespace()
                .find(|part| part.starts_with("delegation_id="))
                .unwrap_or("delegation_id=?");
            Some(format!("{task} {delegation} {stage}"))
        })
        .collect()
}

fn format_observations(observations: &[CompletionObservation]) -> String {
    observations
        .iter()
        .map(|obs| {
            format!(
                "{} issue={} delegation={} elapsed_ms={} outcome={:?}",
                obs.task_id, obs.issue_id, obs.delegation_id, obs.elapsed_ms, obs.outcome
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn first_hang(runs: &[VariantRun]) -> Option<(usize, &CompletionObservation)> {
    runs.iter().find_map(|run| {
        run.observations
            .iter()
            .find(|obs| obs.outcome == CompletionOutcome::Hung)
            .map(|obs| (run.run, obs))
    })
}

fn assert_stable(variant: &Variant, runs: &[VariantRun]) {
    if runs.iter().any(|run| !run.background_hangs.is_empty()) {
        eprintln!(
            "[cec4a975] variant={} stopped before {} repetitions because background load hung: {:?}",
            variant.name,
            runs_for_variant(*variant),
            runs.iter()
                .map(|run| (run.run, run.background_hangs.clone()))
                .collect::<Vec<_>>()
        );
        return;
    }
    let hang_flags: Vec<bool> = runs
        .iter()
        .map(|run| {
            run.observations
                .iter()
                .any(|obs| obs.outcome == CompletionOutcome::Hung)
        })
        .collect();
    let stable = hang_flags.windows(2).all(|pair| pair[0] == pair[1]);
    assert!(
        stable,
        "CEC4A975 simulator was flaky for variant {}: hang flags per run = {:?}",
        variant.name, hang_flags
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "diagnostic reproducer; run explicitly while investigating cec4a975"]
async fn cec4a975_concurrent_completions_alone_does_not_hang_or_hangs() -> Result<()> {
    run_variant_test(Variant {
        name: "A",
        reads: false,
        writes: false,
        reconciler_tick: false,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "diagnostic reproducer; run explicitly while investigating cec4a975"]
async fn cec4a975_concurrent_completions_plus_reads_does_not_hang_or_hangs() -> Result<()> {
    run_variant_test(Variant {
        name: "A+B",
        reads: true,
        writes: false,
        reconciler_tick: false,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "diagnostic reproducer; run explicitly while investigating cec4a975"]
async fn cec4a975_concurrent_completions_plus_writes_does_not_hang_or_hangs() -> Result<()> {
    run_variant_test(Variant {
        name: "A+C",
        reads: false,
        writes: true,
        reconciler_tick: false,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "diagnostic reproducer; run explicitly while investigating cec4a975"]
async fn cec4a975_concurrent_completions_plus_reconciler_tick_does_not_hang_or_hangs() -> Result<()>
{
    run_variant_test(Variant {
        name: "A+B+C+D",
        reads: true,
        writes: true,
        reconciler_tick: true,
    })
    .await
}
