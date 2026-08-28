use spur_code_eval::content_sha256;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestRun {
    root: PathBuf,
}

impl TestRun {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spur-code-eval-runner-{label}-{}-{nonce}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TestRun {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug)]
struct Invocation {
    argv: Vec<String>,
    output: Output,
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_spur-code-eval")
}

fn invoke(run: &TestRun, command: &str) -> Invocation {
    let argv = vec![
        binary().to_owned(),
        "--run-dir".to_owned(),
        run.path().display().to_string(),
        "--fixture".to_owned(),
        command.to_owned(),
    ];
    let mut process = ProcessCommand::new(binary());
    process.args(&argv[1..]);
    for credential in [
        "ANTHROPIC_API_KEY",
        "GITHUB_TOKEN",
        "GOOGLE_API_KEY",
        "OPENAI_API_KEY",
    ] {
        process.env_remove(credential);
    }
    Invocation {
        argv,
        output: process.output().expect("runner process starts"),
    }
}

fn assert_success(invocation: &Invocation) {
    assert!(
        invocation.output.status.success(),
        "argv={:?}\nstdout={}\nstderr={}",
        invocation.argv,
        String::from_utf8_lossy(&invocation.output.stdout),
        String::from_utf8_lossy(&invocation.output.stderr)
    );
}

fn run_through_score(run: &TestRun) {
    for command in ["validate", "index", "retrieve", "score"] {
        let invocation = invoke(run, command);
        assert_success(&invocation);
    }
}

fn metadata(path: &Path) -> serde_json::Value {
    let bytes = fs::read(path).expect("metadata-bearing artifact exists");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("artifact contains one complete JSON value");
    value.get("reproducibility").cloned().unwrap_or(value)
}

fn assert_complete_metadata(value: &serde_json::Value, invocation: &Invocation, phase: &str) {
    let object = value.as_object().expect("metadata is an object");
    assert_eq!(
        object["command_argv"],
        serde_json::json!(invocation.argv),
        "exact command argv is recorded"
    );
    assert!(
        object["platform"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "platform is recorded"
    );
    assert!(
        object["spur_revision"]
            .as_str()
            .is_some_and(|value| matches!(value.len(), 40 | 64)),
        "revision is a full SHA"
    );
    assert!(object["spur_dirty"].is_boolean(), "dirty bit is recorded");
    assert!(
        object["phase_timings_micros"].get(phase).is_some(),
        "phase timing is recorded"
    );
    assert!(
        object["peak_rss_bytes"].as_u64().is_some_and(|rss| rss > 0),
        "positive peak RSS is recorded"
    );
    assert!(
        object["index_bytes"].as_u64().is_some(),
        "index size is recorded"
    );
    assert!(
        object["source_pins"]
            .as_object()
            .is_some_and(|pins| !pins.is_empty()),
        "source pins are recorded"
    );
    assert!(
        object["repository_pins"]
            .as_object()
            .is_some_and(|pins| !pins.is_empty()),
        "repository pins are recorded"
    );
    assert!(
        object["query_policy_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "query policy hash is recorded"
    );
    assert!(
        object["scorer_versions"]
            .as_object()
            .is_some_and(|versions| !versions.is_empty()),
        "scorer versions are recorded"
    );
    assert!(
        object["adapter_versions"]
            .as_object()
            .is_some_and(|versions| !versions.is_empty()),
        "adapter versions are recorded"
    );
    assert_eq!(
        object["suite_denominators"]
            .as_object()
            .expect("suite denominators are recorded")
            .len(),
        3,
        "all suite denominators are recorded"
    );
    assert!(
        object["artifact_records"].is_object(),
        "artifact identities are recorded"
    );
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = fs::metadata(path)
        .expect("artifact metadata exists")
        .permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions).expect("make test artifact writable");
}

#[cfg(not(unix))]
#[allow(clippy::permissions_set_readonly_false)]
fn make_writable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("artifact metadata exists")
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).expect("make test artifact writable");
}

#[test]
fn clap_exposes_all_commands_and_help_exits_successfully() {
    let output = ProcessCommand::new(binary())
        .arg("--help")
        .output()
        .expect("help process starts");

    assert!(output.status.success(), "help exits successfully");
    let stdout = String::from_utf8(output.stdout).expect("help is UTF-8");
    for command in [
        "validate", "index", "retrieve", "score", "model", "resume", "report",
    ] {
        assert!(
            stdout.contains(command),
            "help omitted {command}:\n{stdout}"
        );
    }
}

#[test]
fn skipped_predecessor_phases_return_contextual_errors_without_later_artifacts() {
    for (command, predecessor, forbidden) in [
        ("index", "validate", "call-graphs.jsonl"),
        ("retrieve", "index", "rankings.jsonl"),
        ("score", "retrieve", "metrics.json"),
        ("report", "score", "report.json"),
    ] {
        let run = TestRun::new(command);
        let invocation = invoke(&run, command);

        assert!(
            !invocation.output.status.success(),
            "{command} skipped a phase"
        );
        let stderr = String::from_utf8_lossy(&invocation.output.stderr);
        assert!(stderr.contains(&format!("phase={command}")), "{stderr}");
        assert!(stderr.contains("case=fixture"), "{stderr}");
        assert!(stderr.contains(predecessor), "{stderr}");
        assert!(
            !run.path().join(forbidden).exists(),
            "{command} did not create {forbidden}"
        );
    }
}

#[test]
fn fixture_pipeline_writes_checksum_verified_reproducible_deterministic_report() {
    let run = TestRun::new("pipeline");
    run_through_score(&run);

    let first = invoke(&run, "report");
    assert_success(&first);
    let report_path = run.path().join("report.json");
    let checksum_path = run.path().join("report.sha256");
    let first_bytes = fs::read(&report_path).expect("report exists");
    let expected = fs::read_to_string(&checksum_path).expect("report checksum exists");
    assert_eq!(
        expected.trim(),
        content_sha256(&first_bytes),
        "report checksum verifies"
    );

    let second = invoke(&run, "report");
    assert_success(&second);
    assert_eq!(
        fs::read(report_path).expect("report remains"),
        first_bytes,
        "report bytes are stable"
    );
    assert_eq!(
        fs::read_to_string(checksum_path).expect("checksum remains"),
        expected,
        "checksum bytes are stable"
    );
}

#[test]
fn model_pending_without_credentials_preserves_passing_deterministic_release() {
    let run = TestRun::new("model-pending");
    run_through_score(&run);
    let metrics_before = fs::read(run.path().join("metrics.json")).expect("metrics exist");

    let model = invoke(&run, "model");
    assert_success(&model);
    let report = invoke(&run, "report");
    assert_success(&report);

    let rendered = fs::read_to_string(run.path().join("report.json")).expect("report exists");
    assert!(
        rendered.contains("\"release_status\": \"publish_deterministic\""),
        "model pending preserves deterministic release"
    );
    assert!(
        !rendered.contains("\"release_status\": \"reject\""),
        "model pending does not reject deterministic release"
    );
    assert_eq!(
        fs::read(run.path().join("metrics.json")).expect("metrics remain"),
        metrics_before,
        "model lane does not rewrite deterministic metrics"
    );
}

#[test]
fn resume_starts_at_first_incomplete_phase_and_never_rewrites_frozen_artifacts() {
    let run = TestRun::new("resume");
    run_through_score(&run);
    let frozen = [
        "validation.json",
        "call-graphs.jsonl",
        "rankings.jsonl",
        "contexts.jsonl",
        "metrics.json",
    ]
    .map(|name| {
        let path = run.path().join(name);
        let metadata = fs::metadata(&path).expect("frozen artifact metadata");
        assert!(
            metadata.permissions().readonly(),
            "scored predecessor is frozen"
        );
        (
            path,
            fs::read(run.path().join(name)).expect("frozen artifact bytes"),
            metadata.modified().expect("frozen artifact modified time"),
        )
    });

    let resumed = invoke(&run, "resume");
    assert_success(&resumed);
    assert!(
        String::from_utf8_lossy(&resumed.output.stdout).contains("resumed_from=model"),
        "resume starts at the first incomplete model phase"
    );
    assert!(
        run.path().join("report.json").exists(),
        "resume publishes the report"
    );
    for (path, bytes, modified) in frozen {
        let metadata = fs::metadata(&path).expect("frozen artifact remains");
        assert!(
            metadata.permissions().readonly(),
            "resumed predecessor remains frozen"
        );
        assert_eq!(
            fs::read(&path).expect("frozen bytes remain"),
            bytes,
            "resume preserves frozen bytes"
        );
        assert_eq!(
            metadata.modified().expect("modified time remains"),
            modified,
            "resume does not rewrite frozen predecessors"
        );
    }
}

#[test]
fn every_command_records_complete_reproducibility_metadata() {
    let run = TestRun::new("metadata");
    let commands = [
        ("validate", "validation.json"),
        ("index", "call-graphs.jsonl"),
        ("retrieve", "rankings.jsonl"),
        ("score", "metrics.json"),
        ("model", "model-records.jsonl"),
        ("report", "report.json"),
    ];
    for (command, artifact) in commands {
        let invocation = invoke(&run, command);
        assert_success(&invocation);
        let value = metadata(&run.path().join(artifact));
        assert_complete_metadata(&value, &invocation, command);
    }

    let resumed_run = TestRun::new("metadata-resume");
    for command in ["validate", "index"] {
        assert_success(&invoke(&resumed_run, command));
    }
    let resume = invoke(&resumed_run, "resume");
    assert_success(&resume);
    let value = metadata(&resumed_run.path().join("report.json"));
    assert_complete_metadata(&value, &resume, "report");
    assert_eq!(
        value["command_argv"],
        serde_json::json!(resume.argv),
        "resumed report records resume argv"
    );
    assert!(
        value["artifact_records"]
            .as_object()
            .is_some_and(|records| !records.is_empty()),
        "resumed report records artifact identities"
    );
    assert!(
        value["index_bytes"].as_u64().is_some_and(|bytes| bytes > 0),
        "resumed report records index bytes"
    );
}

#[test]
fn resume_rejects_tampered_or_non_frozen_predecessor_artifacts_before_reuse() {
    let tampered = TestRun::new("tampered");
    run_through_score(&tampered);
    let rankings = tampered.path().join("rankings.jsonl");
    make_writable(&rankings);
    fs::write(&rankings, b"tampered\n").expect("tamper ranking");

    let resume = invoke(&tampered, "resume");
    assert!(!resume.output.status.success(), "tampering rejects resume");
    let stderr = String::from_utf8_lossy(&resume.output.stderr);
    assert!(stderr.contains("phase=resume"), "{stderr}");
    assert!(stderr.to_ascii_lowercase().contains("checksum"), "{stderr}");
    assert!(
        !tampered.path().join("report.json").exists(),
        "tampering prevents report publication"
    );

    let non_frozen = TestRun::new("non-frozen");
    run_through_score(&non_frozen);
    let validation = non_frozen.path().join("validation.json");
    make_writable(&validation);

    let resume = invoke(&non_frozen, "resume");
    assert!(
        !resume.output.status.success(),
        "non-frozen predecessor rejects resume"
    );
    let stderr = String::from_utf8_lossy(&resume.output.stderr);
    assert!(stderr.contains("phase=resume"), "{stderr}");
    assert!(
        stderr.to_ascii_lowercase().contains("read-only"),
        "{stderr}"
    );
    assert!(
        !non_frozen.path().join("report.json").exists(),
        "non-frozen predecessor prevents report publication"
    );
}
