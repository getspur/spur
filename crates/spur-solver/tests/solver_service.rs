use std::{
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use spur_solver::{
    process::{ProcessFuture, ProcessOutcome, ProcessOutput, ProcessRequest, ProcessRunner},
    service::SolverService,
    types::{SolveConstraintsRequest, SolveStatus},
};
use tokio::sync::{Notify, Semaphore};

#[derive(Debug, Default)]
struct BlockingRunner {
    calls: AtomicUsize,
    release: Notify,
    started: Notify,
}

impl ProcessRunner for BlockingRunner {
    fn run(&self, _request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Ok(ProcessOutcome::Completed(ProcessOutput {
                success: true,
                exit_code: Some(0),
                stdout: b"unsat\n".to_vec(),
                stderr: Vec::new(),
            }))
        })
    }
}

#[derive(Debug)]
struct CapacityRunner {
    current: AtomicUsize,
    peak: AtomicUsize,
    release: Semaphore,
    started: AtomicUsize,
    started_notify: Notify,
}

impl Default for CapacityRunner {
    fn default() -> Self {
        Self {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            release: Semaphore::new(0),
            started: AtomicUsize::new(0),
            started_notify: Notify::new(),
        }
    }
}

impl ProcessRunner for CapacityRunner {
    fn run(&self, _request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(async move {
            let current = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(current, Ordering::SeqCst);
            self.started.fetch_add(1, Ordering::SeqCst);
            self.started_notify.notify_waiters();
            let permit = self
                .release
                .acquire()
                .await
                .expect("capacity runner release semaphore stays open");
            permit.forget();
            self.current.fetch_sub(1, Ordering::SeqCst);
            Ok(ProcessOutcome::Completed(ProcessOutput {
                success: true,
                exit_code: Some(0),
                stdout: b"unsat\n".to_vec(),
                stderr: Vec::new(),
            }))
        })
    }
}

#[tokio::test]
async fn semaphore_wait_consumes_the_solve_budget() {
    let runner = Arc::new(BlockingRunner::default());
    let service_runner: Arc<dyn ProcessRunner> = Arc::<BlockingRunner>::clone(&runner);
    let service = Arc::new(SolverService::with_runner_and_concurrency(
        service_runner,
        NonZeroUsize::MIN,
    ));

    let first_service = Arc::clone(&service);
    let first =
        tokio::spawn(async move { first_service.solve_constraints(empty_request(2_000)).await });
    tokio::time::timeout(Duration::from_secs(1), runner.started.notified())
        .await
        .expect("first solve should enter the runner");

    let queued = service
        .solve_constraints(empty_request(40))
        .await
        .expect("queue timeout is a solver result");

    assert_eq!(queued.status, SolveStatus::Timeout);
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);

    runner.release.notify_one();
    let first = first
        .await
        .expect("first solve task should join")
        .expect("first solve should return a result");
    assert_eq!(first.status, SolveStatus::Unsat);
}

#[tokio::test]
async fn cloned_default_service_shares_exactly_four_runner_permits() {
    let runner = Arc::new(CapacityRunner::default());
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapacityRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);
    assert_eq!(service.max_concurrent_solves(), 4);

    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let service = service.clone();
            tokio::spawn(async move { service.solve_constraints(empty_request(2_000)).await })
        })
        .collect();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let notified = runner.started_notify.notified();
            if runner.started.load(Ordering::SeqCst) >= 4 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("four solves should enter the runner");
    tokio::time::sleep(Duration::from_millis(40)).await;

    assert_eq!(runner.started.load(Ordering::SeqCst), 4);
    assert_eq!(runner.peak.load(Ordering::SeqCst), 4);

    runner.release.add_permits(5);
    for task in tasks {
        let response = task
            .await
            .expect("capacity solve task should join")
            .expect("capacity solve should return");
        assert_eq!(response.status, SolveStatus::Unsat);
    }
    assert_eq!(runner.started.load(Ordering::SeqCst), 5);
    assert_eq!(runner.peak.load(Ordering::SeqCst), 4);
}

fn empty_request(timeout_ms: u64) -> SolveConstraintsRequest {
    SolveConstraintsRequest {
        vars: Vec::new(),
        constraints: Vec::new(),
        timeout_ms,
        persist: false,
    }
}

#[cfg(unix)]
mod unix {
    use std::{
        fs,
        os::unix::fs::PermissionsExt as _,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    };

    use spur_solver::{
        process::{Z3Process, MAX_STDERR_BYTES, MAX_STDOUT_BYTES},
        service::{SolverService, SolverServiceError},
        types::{ModelValue, SolveConstraintsRequest, SolveStatus, Variable, DEFAULT_TIMEOUT_MS},
    };
    use tempfile::TempDir;
    use tokio::process::Command;

    static FAKE_SOLVER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct FakeSolver {
        _directory: TempDir,
        args_path: PathBuf,
        binary: PathBuf,
        pids_path: PathBuf,
    }

    impl FakeSolver {
        fn new(body: &str) -> Self {
            let directory = tempfile::tempdir().expect("create fake solver directory");
            let binary = directory.path().join("fake-z3");
            let args_path = directory.path().join("args");
            let pids_path = directory.path().join("pids");
            let body = body.replace("__PIDS_PATH__", &pids_path.display().to_string());
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\n{body}\n",
                args_path.display()
            );
            fs::write(&binary, script).expect("write fake solver");
            let mut permissions = fs::metadata(&binary)
                .expect("stat fake solver")
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&binary, permissions).expect("make fake solver executable");
            Self {
                _directory: directory,
                args_path,
                binary,
                pids_path,
            }
        }

        fn service(&self) -> SolverService {
            SolverService::with_runner(Arc::new(Z3Process::with_binary(self.binary.clone())))
        }
    }

    #[tokio::test]
    async fn fake_solver_sat_model_is_decoded_and_fixed_argv_is_used() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new(
            "cat >/dev/null\nprintf 'sat\\n((v_workers 4) (v_enabled true) (v_mode 1))\\n'",
        );
        let response = fake
            .service()
            .solve_constraints(model_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect("fake sat solve should return");

        assert_eq!(response.status, SolveStatus::Sat);
        let model = response.model.expect("sat must include a model");
        assert_eq!(model.get("workers"), Some(&ModelValue::Int(4)));
        assert_eq!(model.get("enabled"), Some(&ModelValue::Bool(true)));
        assert_eq!(
            model.get("mode"),
            Some(&ModelValue::Enum("safe".to_owned()))
        );
        assert_eq!(
            fs::read_to_string(&fake.args_path).expect("read captured argv"),
            "-in\n-memory:1024\n-T:30\n"
        );
    }

    #[tokio::test]
    async fn fake_solver_unsat_has_no_model() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new("cat >/dev/null\nprintf 'unsat\\n'");
        let response = fake
            .service()
            .solve_constraints(empty_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect("fake unsat solve should return");

        assert_eq!(response.status, SolveStatus::Unsat);
        assert!(response.model.is_none());
    }

    #[tokio::test]
    async fn fake_solver_unsat_accepts_runner_owned_get_value_error() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new(
            "cat >/dev/null\nprintf 'unsat\\n(error \"line 6 column 16: model is not available\")\\n'\nexit 1",
        );
        let response = fake
            .service()
            .solve_constraints(model_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect("fake unsat solve should return");

        assert_eq!(response.status, SolveStatus::Unsat);
        assert!(response.model.is_none());
    }

    #[tokio::test]
    async fn fake_solver_unknown_is_not_collapsed_into_unsat() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new("cat >/dev/null\nprintf 'unknown\\n'");
        let response = fake
            .service()
            .solve_constraints(empty_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect("fake unknown solve should return");

        assert_eq!(response.status, SolveStatus::Unknown);
        assert!(response.model.is_none());
    }

    #[tokio::test]
    async fn timeout_kills_the_fake_solver_process_group() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new(
            "sleep 30 &\nchild=$!\nprintf '%s %s\\n' \"$$\" \"$child\" > \"__PIDS_PATH__\"\nwait \"$child\"",
        );
        let response = fake
            .service()
            .solve_constraints(empty_request(2_000))
            .await
            .expect("timeout is a solver result");

        assert_eq!(response.status, SolveStatus::Timeout);
        let pids = fs::read_to_string(&fake.pids_path).unwrap_or_else(|error| {
            panic!(
                "fake solver should record pids: {error}; script={}; args={}",
                fs::read_to_string(&fake.binary).expect("read fake script"),
                fs::read_to_string(&fake.args_path)
                    .unwrap_or_else(|args_error| { format!("<unavailable: {args_error}>") })
            )
        });
        for pid in pids.split_whitespace() {
            let pid = pid.parse::<u32>().expect("recorded pid should be numeric");
            wait_until_dead(pid).await;
        }
    }

    #[tokio::test]
    async fn cancellation_kills_the_fake_solver_process_group() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new(
            "sleep 30 &\nchild=$!\nprintf '%s %s\\n' \"$$\" \"$child\" > \"__PIDS_PATH__\"\nwait \"$child\"",
        );
        let service = fake.service();
        let solve = tokio::spawn(async move {
            service
                .solve_constraints(empty_request(DEFAULT_TIMEOUT_MS))
                .await
        });
        wait_for_file(&fake.pids_path).await;
        let pids = fs::read_to_string(&fake.pids_path).expect("fake solver should record pids");

        solve.abort();
        let join_error = solve.await.expect_err("aborted solve should be cancelled");
        assert!(join_error.is_cancelled());
        for pid in pids.split_whitespace() {
            let pid = pid.parse::<u32>().expect("recorded pid should be numeric");
            wait_until_dead(pid).await;
        }
    }

    #[tokio::test]
    async fn stdout_over_the_cap_is_an_error_result() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new(&format!(
            "cat >/dev/null\ndd if=/dev/zero bs={} count=1 2>/dev/null | tr '\\000' x",
            MAX_STDOUT_BYTES + 1
        ));
        let response = fake
            .service()
            .solve_constraints(empty_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect("output overflow is a solver result");

        assert_eq!(response.status, SolveStatus::Error);
        assert!(response
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("stdout")));
    }

    #[tokio::test]
    async fn stderr_over_the_cap_is_an_error_result() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let fake = FakeSolver::new(&format!(
            "cat >/dev/null\ndd if=/dev/zero bs={} count=1 2>/dev/null | tr '\\000' x >&2",
            MAX_STDERR_BYTES + 1
        ));
        let response = fake
            .service()
            .solve_constraints(empty_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect("output overflow is a solver result");

        assert_eq!(response.status, SolveStatus::Error);
        assert!(response
            .reason
            .as_deref()
            .is_some_and(|reason| { reason.contains("stderr") && reason.contains("exceeded") }));
    }

    #[tokio::test]
    async fn missing_configured_binary_is_solver_unavailable() {
        let _test_guard = FAKE_SOLVER_TEST_LOCK.lock().await;
        let binary = Path::new("/definitely/not/a/spur/fake-z3");
        let service =
            SolverService::with_runner(Arc::new(Z3Process::with_binary(binary.to_owned())));
        let error = service
            .solve_constraints(empty_request(DEFAULT_TIMEOUT_MS))
            .await
            .expect_err("missing binary should be a transport-facing error");

        assert!(matches!(
            error,
            SolverServiceError::SolverUnavailable { .. }
        ));
    }

    fn model_request(timeout_ms: u64) -> SolveConstraintsRequest {
        SolveConstraintsRequest {
            vars: vec![
                Variable::IntRange {
                    name: "workers".to_owned(),
                    min: 1,
                    max: 16,
                },
                Variable::Bool {
                    name: "enabled".to_owned(),
                },
                Variable::Enum {
                    name: "mode".to_owned(),
                    values: vec!["safe".to_owned(), "fast".to_owned()],
                },
            ],
            constraints: Vec::new(),
            timeout_ms,
            persist: false,
        }
    }

    fn empty_request(timeout_ms: u64) -> SolveConstraintsRequest {
        SolveConstraintsRequest {
            vars: Vec::new(),
            constraints: Vec::new(),
            timeout_ms,
            persist: false,
        }
    }

    async fn wait_until_dead(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !pid_is_alive(pid).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("fake solver process {pid} survived process-group kill");
    }

    async fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if path.is_file() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "fake solver did not create readiness file {}",
            path.display()
        );
    }

    async fn pid_is_alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .await
            .is_ok_and(|output| output.status.success())
    }
}
