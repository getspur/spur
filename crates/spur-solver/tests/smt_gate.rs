use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use spur_solver::{
    process::{ProcessFuture, ProcessOutcome, ProcessOutput, ProcessRequest, ProcessRunner},
    service::{SolverService, SolverServiceError},
    smt_gate::{validate_smt_script, MAX_RAW_SMT_BYTES},
    types::{ModelValue, SolveSmtRequest, SolveStatus, DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS},
};

const ALLOWED_SCRIPT: &str = r"
; Commands hidden in comments do not count: (exit)
(set-logic ALL)
(set-option :produce-models true)
(declare-sort Item 0)
(declare-const |quoted; () symbol| Int)
(declare-fun score (Int) Int)
(declare-datatype Choice ((first) (second)))
(declare-datatypes ((Pair 0)) (((pair (left Int) (right Int)))))
(assert (and true (= (score |quoted; () symbol|) 42)))
(push 1)
(pop 1)
(check-sat)
(get-model)
(get-value ((score |quoted; () symbol|)))
";

#[test]
fn raw_request_defaults_optional_timeout_and_persistence_fields() {
    let request: SolveSmtRequest = serde_json::from_str(r#"{"smt_lib":"(check-sat)"}"#)
        .expect("optional raw request controls should default");

    assert_eq!(request.timeout_ms, DEFAULT_TIMEOUT_MS);
    assert!(!request.persist);
}

#[test]
fn allows_only_the_supported_top_level_command_family() {
    validate_smt_script(ALLOWED_SCRIPT).expect("supported SMT commands should pass the gate");
}

#[test]
fn rejects_any_disallowed_command_without_stripping_it() {
    let script = "(assert true)\n(exit)\n(check-sat)\n";
    let error = validate_smt_script(script).expect_err("exit must reject the complete script");

    assert!(error.to_string().contains("exit"));
}

#[test]
fn rejects_spoofing_stateful_and_definition_commands() {
    for script in [
        r#"(echo "sat")"#,
        "(reset)",
        "(reset-assertions)",
        "(get-info :version)",
        "(define-fun answer () Int 42)",
    ] {
        assert!(
            validate_smt_script(script).is_err(),
            "script must be rejected: {script}"
        );
    }
}

#[test]
fn restricts_set_option_to_the_model_production_boolean() {
    for script in [
        "(set-option :timeout 60000)",
        r#"(set-option :regular-output-channel "captured.txt")"#,
        "(set-option :print-success true)",
        "(set-option :produce-models 1)",
        "(set-option :produce-models true false)",
    ] {
        assert!(
            validate_smt_script(script).is_err(),
            "script must be rejected: {script}"
        );
    }

    validate_smt_script("(set-option :produce-models false)")
        .expect("the safe option accepts a Boolean value");
}

#[test]
fn comments_strings_and_quoted_symbols_cannot_smuggle_commands() {
    let script = r#"
; (exit)
(declare-const |name with (exit) and ; text| String)
(assert (= |name with (exit) and ; text| "(exit)"))
(check-sat)
"#;

    validate_smt_script(script).expect("non-command lexical content should stay opaque");
}

#[test]
fn malformed_or_non_list_top_level_input_is_rejected() {
    for script in [
        "",
        "assert",
        ")",
        "()",
        "((assert true))",
        "(assert true",
        r#"(assert (= "unterminated))"#,
        "(declare-const |unterminated Int)",
    ] {
        assert!(
            validate_smt_script(script).is_err(),
            "script must be rejected: {script:?}"
        );
    }
}

#[test]
fn enforces_the_256_kib_raw_byte_cap() {
    let check_sat = "(check-sat)";
    let at_limit = format!(
        "{}{check_sat}",
        " ".repeat(MAX_RAW_SMT_BYTES - check_sat.len())
    );
    assert_eq!(at_limit.len(), MAX_RAW_SMT_BYTES);
    validate_smt_script(&at_limit).expect("a script exactly at the byte cap is accepted");

    let over_limit = format!("{at_limit} ");
    let error = validate_smt_script(&over_limit).expect_err("one byte over the cap must fail");
    assert!(error.to_string().contains("262145"));
}

#[derive(Debug)]
struct CapturingRunner {
    calls: AtomicUsize,
    scripts: Mutex<Vec<String>>,
    stdout: &'static [u8],
}

impl CapturingRunner {
    fn new(stdout: &'static [u8]) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            scripts: Mutex::new(Vec::new()),
            stdout,
        }
    }
}

impl ProcessRunner for CapturingRunner {
    fn run(&self, request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.scripts
                .lock()
                .expect("capture mutex should remain usable")
                .push(request.smt().to_owned());
            Ok(ProcessOutcome::Completed(ProcessOutput {
                success: true,
                exit_code: Some(0),
                stdout: self.stdout.to_vec(),
                stderr: Vec::new(),
            }))
        })
    }
}

#[tokio::test]
async fn solve_smt_gates_and_runs_the_original_script_without_z3() {
    let runner = Arc::new(CapturingRunner::new(b"sat\n((answer 42))\n"));
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapturingRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);
    let smt_lib = "(declare-const answer Int)\n(check-sat)\n(get-value (answer))\n";

    let response = service
        .solve_smt(SolveSmtRequest {
            smt_lib: smt_lib.to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        })
        .await
        .expect("allowed raw SMT should run");

    assert_eq!(response.status, SolveStatus::Sat);
    assert_eq!(
        response
            .model
            .as_ref()
            .and_then(|model| model.get("answer")),
        Some(&ModelValue::Int(42))
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        runner
            .scripts
            .lock()
            .expect("capture mutex should remain usable")
            .as_slice(),
        [smt_lib]
    );
}

#[tokio::test]
async fn solve_smt_decodes_get_model_constant_definitions_without_z3() {
    let runner = Arc::new(CapturingRunner::new(
        b"sat\n(\n  (define-fun answer () Int 42)\n  (define-fun enabled () Bool true)\n)\n",
    ));
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapturingRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);

    let response = service
        .solve_smt(SolveSmtRequest {
            smt_lib: concat!(
                "(declare-const answer Int)\n",
                "(declare-const enabled Bool)\n",
                "(check-sat)\n",
                "(get-model)\n",
            )
            .to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        })
        .await
        .expect("allowed get-model script should run");

    assert_eq!(response.status, SolveStatus::Sat);
    let model = response.model.expect("sat must include a model");
    assert_eq!(model.get("answer"), Some(&ModelValue::Int(42)));
    assert_eq!(model.get("enabled"), Some(&ModelValue::Bool(true)));
}

#[tokio::test]
async fn solve_smt_keeps_quoted_get_value_symbols_intact() {
    let runner = Arc::new(CapturingRunner::new(b"sat\n((|answer value| 42))\n"));
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapturingRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);

    let response = service
        .solve_smt(SolveSmtRequest {
            smt_lib: concat!(
                "(declare-const |answer value| Int)\n",
                "(check-sat)\n",
                "(get-value (|answer value|))\n",
            )
            .to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        })
        .await
        .expect("allowed quoted-symbol script should run");

    assert_eq!(response.status, SolveStatus::Sat);
    assert_eq!(
        response
            .model
            .as_ref()
            .and_then(|model| model.get("|answer value|")),
        Some(&ModelValue::Int(42))
    );
}

#[tokio::test]
async fn solve_smt_persists_through_the_shared_artifact_store() {
    let directory = tempfile::tempdir().expect("create raw persistence root");
    let runner = Arc::new(CapturingRunner::new(b"unsat\n"));
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapturingRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner).with_repo_root(directory.path());

    let response = service
        .solve_smt(SolveSmtRequest {
            smt_lib: "(assert false)\n(check-sat)\n".to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: true,
        })
        .await
        .expect("persisted raw solve should return");

    let solve_id = response.solve_id.expect("persisted solve returns an id");
    let stored = service
        .get_solve_result(&solve_id)
        .expect("persisted raw solve should reload");
    assert_eq!(stored.result.status, SolveStatus::Unsat);
}

#[tokio::test]
async fn solve_smt_rejects_before_invoking_the_runner() {
    let runner = Arc::new(CapturingRunner::new(b"sat\n"));
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapturingRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);

    let error = service
        .solve_smt(SolveSmtRequest {
            smt_lib: "(exit)".to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        })
        .await
        .expect_err("disallowed SMT should be invalid params");

    assert!(matches!(error, SolverServiceError::InvalidParams { .. }));
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn solve_smt_rejects_timeout_over_the_shared_cap_before_running() {
    let runner = Arc::new(CapturingRunner::new(b"sat\n"));
    let service_runner: Arc<dyn ProcessRunner> = Arc::<CapturingRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);

    let error = service
        .solve_smt(SolveSmtRequest {
            smt_lib: "(check-sat)".to_owned(),
            timeout_ms: MAX_TIMEOUT_MS + 1,
            persist: false,
        })
        .await
        .expect_err("oversized timeout should be invalid params");

    assert!(matches!(error, SolverServiceError::InvalidParams { .. }));
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
}
