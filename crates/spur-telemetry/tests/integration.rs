use serde_json::Value;
use spur_telemetry::tier1_events::{
    AcpRequestDuration, LlmRequestDuration, McpRequestDuration, ModelName, Outcome, SessionStarted,
    TuiFrameSlow,
};
use spur_telemetry::tier2_events::{
    McpServerName, McpToolCalled, McpToolName, PlanCreated, ReviewCompleted, ReviewOutcome,
    SkillName, TuiViewOpened, ViewName, WorkerDispatched,
};
use spur_telemetry::{emit, init, shutdown_sync, InitConfig, TelemetryConfig, TELEMETRY_COMPILED};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODE_ENV: &str = "SPUR_TELEMETRY_INT_MODE";
const HOME_ENV: &str = "SPUR_TELEMETRY_INT_HOME";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integration_entrypoint() {
    if let Ok(mode) = std::env::var(MODE_ENV) {
        run_mode(&mode).await;
        return;
    }

    run_as_parent();
}

fn run_as_parent() {
    run_child("contract_enabled", true);
    run_child("contract_disabled", true);
    run_child("consent_tier2_off", true);
    run_child("consent_tier2_on", true);
    run_child("panic_roundtrip", true);
    run_child("disable_crash", true);
    run_child("rate_limit", true);
    run_child("network_failure", true);
}

async fn run_mode(mode: &str) {
    std::env::set_var("CI", "false");
    std::env::remove_var("SPUR_TELEMETRY");

    match mode {
        "contract_enabled" => contract_enabled().await,
        "contract_disabled" => contract_disabled().await,
        "consent_tier2_off" => consent_tier2_off().await,
        "consent_tier2_on" => consent_tier2_on().await,
        "panic_roundtrip" => panic_roundtrip().await,
        "panic_child_crash_enabled" => panic_child_crash_enabled().await,
        "panic_child_crash_disabled" => panic_child_crash_disabled().await,
        "disable_crash" => disable_crash().await,
        "rate_limit" => rate_limit().await,
        "network_failure" => network_failure().await,
        other => panic!("unknown mode: {other}"),
    }
}

async fn contract_enabled() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let server = start_ok_server().await;
    let home = tempfile::tempdir().expect("tempdir");
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());
    write_config(true, true, true);

    emit_all_events(true);

    let requests = server.received_requests().await.expect("requests");
    assert!(
        !requests.is_empty(),
        "expected requests with enabled telemetry"
    );
    assert_all_batches_schema(requests.as_slice());
}

async fn contract_disabled() {
    let server = start_ok_server().await;
    let home = tempfile::tempdir().expect("tempdir");
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());

    if TELEMETRY_COMPILED {
        std::env::set_var("SPUR_TELEMETRY", "0");
    }

    write_config(true, true, true);
    emit_all_events(false);

    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests.is_empty(),
        "expected zero requests in disabled mode"
    );
}

async fn consent_tier2_off() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let server = start_ok_server().await;
    let home = tempfile::tempdir().expect("tempdir");
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());
    write_config(true, true, false);

    emit_all_events(true);

    let requests = server.received_requests().await.expect("requests");
    let events = extract_events(requests.as_slice());
    assert!(
        events
            .iter()
            .all(|evt| !tier2_event_names().contains(evt["event"].as_str().unwrap_or_default())),
        "tier2 events should be suppressed"
    );
}

async fn consent_tier2_on() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let server = start_ok_server().await;
    let home = tempfile::tempdir().expect("tempdir");
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());
    write_config(true, true, true);

    emit_all_events(true);

    let requests = server.received_requests().await.expect("requests");
    let events = extract_events(requests.as_slice());
    assert!(
        events
            .iter()
            .any(|evt| tier2_event_names().contains(evt["event"].as_str().unwrap_or_default())),
        "tier2 events should be present"
    );
}

async fn panic_roundtrip() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let home = tempfile::tempdir().expect("home");

    run_child_with_home("panic_child_crash_enabled", home.path(), false);
    assert_eq!(
        count_crash_reports(home.path()),
        1,
        "crash file should be created"
    );

    let server = start_ok_server().await;
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());
    write_config(true, false, false);
    let _guard = init(InitConfig {
        spur_version: "integration",
    });
    tokio::time::sleep(Duration::from_millis(1200)).await;
    shutdown_sync();

    let requests = server.received_requests().await.expect("requests");
    let events = extract_events(requests.as_slice());
    assert!(
        events.iter().any(|evt| evt["event"] == "$exception"),
        "expected panic upload event"
    );
    assert_eq!(
        count_crash_reports(home.path()),
        0,
        "crash file should be deleted"
    );
}

async fn panic_child_crash_enabled() {
    set_home_from_env();
    write_config(true, false, false);
    let _guard = init(InitConfig {
        spur_version: "integration",
    });
    panic!("panic child with crash enabled");
}

async fn panic_child_crash_disabled() {
    set_home_from_env();
    write_config(false, false, false);
    let _guard = init(InitConfig {
        spur_version: "integration",
    });
    panic!("panic child with crash disabled");
}

async fn disable_crash() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let home = tempfile::tempdir().expect("home");
    run_child_with_home("panic_child_crash_disabled", home.path(), false);
    assert_eq!(
        count_crash_reports(home.path()),
        0,
        "no crash file expected"
    );
}

async fn rate_limit() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let server = start_ok_server().await;
    let home = tempfile::tempdir().expect("tempdir");
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());
    write_config(false, true, false);

    let _guard = init(InitConfig {
        spur_version: "integration",
    });
    for i in 0..600u64 {
        emit!(TuiFrameSlow { duration_ms: i });
    }
    shutdown_sync();

    let requests = server.received_requests().await.expect("requests");
    let events = extract_events(requests.as_slice());
    assert!(
        events.len() <= 500,
        "expected <=500 events, got {}",
        events.len()
    );
    assert!(
        600usize.saturating_sub(events.len()) > 0,
        "expected dropped > 0"
    );
}

async fn network_failure() {
    if !TELEMETRY_COMPILED {
        return;
    }

    let server = MockServer::start().await;
    let first_fail = Mock::given(method("POST"))
        .and(path("/batch/"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .expect(1)
        .mount_as_scoped(&server)
        .await;
    let _always_ok = Mock::given(method("POST"))
        .and(path("/batch/"))
        .respond_with(ResponseTemplate::new(200))
        .mount_as_scoped(&server)
        .await;

    let home = tempfile::tempdir().expect("tempdir");
    set_home(home.path());
    std::env::set_var("SPUR_POSTHOG_ENDPOINT", server.uri());
    write_config(false, true, false);

    let _guard = init(InitConfig {
        spur_version: "integration",
    });
    for i in 1..=100u64 {
        emit!(TuiFrameSlow { duration_ms: i });
    }
    shutdown_sync();
    drop(first_fail);

    let requests = server.received_requests().await.expect("requests");
    let events = extract_events(requests.as_slice());
    let durations = events
        .iter()
        .filter(|evt| evt["event"] == "tui_frame_slow")
        .filter_map(|evt| evt["properties"]["duration_ms"].as_u64())
        .collect::<BTreeSet<_>>();

    assert!(
        durations.contains(&51),
        "expected successful post-failure batch"
    );
    assert!(
        !durations.contains(&1),
        "first failed batch should not be retried"
    );
}

async fn start_ok_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/batch/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

fn emit_all_events(expect_active: bool) {
    let _guard = init(InitConfig {
        spur_version: "integration",
    });
    assert_eq!(
        spur_telemetry::telemetry_active(),
        expect_active,
        "unexpected telemetry active state"
    );

    emit!(SessionStarted {
        os: "linux",
        arch: "x86_64",
        spur_version: "integration",
        is_tui: true,
    });
    emit!(LlmRequestDuration {
        model_name: ModelName::Gpt5,
        duration_ms: 12,
        token_count_bucket: 128,
        outcome: Outcome::Ok,
    });
    emit!(McpRequestDuration {
        duration_ms: 20,
        outcome: Outcome::Timeout,
    });
    emit!(AcpRequestDuration {
        duration_ms: 30,
        outcome: Outcome::Error,
    });
    emit!(TuiFrameSlow { duration_ms: 40 });

    emit!(PlanCreated {
        task_count: 3,
        brain_model: ModelName::ClaudeSonnet47,
        duration_ms: 100,
    });
    emit!(WorkerDispatched {
        worker_model: ModelName::Gpt5Codex,
        skill_used: SkillName::PlanTaskDiscipline,
        attempt_num: 1,
    });
    emit!(McpToolCalled {
        server_name: McpServerName::SpurMcp,
        tool_name: McpToolName::SubmitPlan,
        outcome: Outcome::Ok,
    });
    emit!(ReviewCompleted {
        outcome: ReviewOutcome::Accept,
        iteration_count: 2,
    });
    emit!(TuiViewOpened {
        view_name: ViewName::Dashboard,
    });

    std::thread::sleep(Duration::from_millis(50));
    shutdown_sync();
}

fn run_child(mode: &str, expect_success: bool) -> Output {
    let home = tempfile::tempdir().expect("tempdir");
    run_child_with_home(mode, home.path(), expect_success)
}

fn run_child_with_home(mode: &str, home: &Path, expect_success: bool) -> Output {
    run_child_with_home_and_endpoint(mode, home, "http://127.0.0.1:1", expect_success)
}

fn run_child_with_home_and_endpoint(
    mode: &str,
    home: &Path,
    endpoint: &str,
    expect_success: bool,
) -> Output {
    let output = Command::new(std::env::current_exe().expect("current_exe"))
        .arg("--exact")
        .arg("integration_entrypoint")
        .arg("--nocapture")
        .env(MODE_ENV, mode)
        .env(HOME_ENV, home)
        .env("SPUR_POSTHOG_ENDPOINT", endpoint)
        .env("CI", "false")
        .output()
        .expect("spawn child");

    assert_eq!(
        output.status.success(),
        expect_success,
        "child mode {mode} status mismatch: {output:?}"
    );
    output
}

fn set_home(path: &Path) {
    std::env::set_var("HOME", path);
}

fn set_home_from_env() {
    let home = std::env::var(HOME_ENV).expect("home env");
    std::env::set_var("HOME", home);
}

fn write_config(tier1_crash: bool, tier1_perf: bool, tier2_usage: bool) {
    let cfg = TelemetryConfig {
        version: 1,
        anonymous_id: uuid::Uuid::new_v4(),
        tier1_crash,
        tier1_perf,
        tier2_usage,
        last_consent_prompt_at: None,
    };
    spur_telemetry::save_config(&cfg).expect("save telemetry config");
}

fn count_crash_reports(home: &Path) -> usize {
    let dir = home.join(".spur").join("crash-reports");
    std::fs::read_dir(dir)
        .ok()
        .map(|it| {
            it.flatten()
                .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("json"))
                .count()
        })
        .unwrap_or(0)
}

fn extract_events(requests: &[wiremock::Request]) -> Vec<Value> {
    let mut out = Vec::new();
    for req in requests {
        let body: Value = serde_json::from_slice(&req.body).expect("valid json");
        let batch = body
            .get("batch")
            .and_then(Value::as_array)
            .expect("batch array");
        for event in batch {
            out.push(event.clone());
        }
    }
    out
}

fn assert_all_batches_schema(requests: &[wiremock::Request]) {
    let mut seen = BTreeSet::new();

    for req in requests {
        let body: Value = serde_json::from_slice(&req.body).expect("request json");
        let api_key = body
            .get("api_key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(api_key, "loopback");

        let events = body
            .get("batch")
            .and_then(Value::as_array)
            .expect("batch array");
        for event in events {
            validate_event_schema(event);
            let name = event
                .get("event")
                .and_then(Value::as_str)
                .expect("event name");
            seen.insert(name.to_string());
        }
    }

    for required in tier1_event_names().union(&tier2_event_names()) {
        assert!(
            seen.contains(*required),
            "missing emitted event: {required}"
        );
    }
}

fn validate_event_schema(event: &Value) {
    let name = event
        .get("event")
        .and_then(Value::as_str)
        .expect("event name string");
    assert!(
        allowed_event_names().contains(name),
        "unexpected event {name}"
    );

    assert!(event.get("distinct_id").and_then(Value::as_str).is_some());
    assert!(event.get("timestamp").and_then(Value::as_str).is_some());

    let props = event
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties object");
    assert!(props.get("spur_version").and_then(Value::as_str).is_some());

    let allowed = &allowed_properties()[name];
    for key in props.keys() {
        assert!(
            allowed.contains(key.as_str()),
            "event {name} has unexpected property {key}"
        );
    }
}

fn allowed_event_names() -> BTreeSet<&'static str> {
    tier1_event_names()
        .into_iter()
        .chain(tier2_event_names())
        .collect()
}

fn tier1_event_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "session_started",
        "llm_request_duration",
        "mcp_request_duration",
        "acp_request_duration",
        "tui_frame_slow",
    ])
}

fn tier2_event_names() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "plan_created",
        "worker_dispatched",
        "mcp_tool_called",
        "review_completed",
        "tui_view_opened",
    ])
}

fn allowed_properties() -> BTreeMap<&'static str, BTreeSet<&'static str>> {
    BTreeMap::from([
        (
            "session_started",
            BTreeSet::from(["os", "arch", "spur_version", "is_tui"]),
        ),
        (
            "llm_request_duration",
            BTreeSet::from([
                "model_name",
                "duration_ms",
                "token_count_bucket",
                "outcome",
                "spur_version",
            ]),
        ),
        (
            "mcp_request_duration",
            BTreeSet::from(["duration_ms", "outcome", "spur_version"]),
        ),
        (
            "acp_request_duration",
            BTreeSet::from(["duration_ms", "outcome", "spur_version"]),
        ),
        (
            "tui_frame_slow",
            BTreeSet::from(["duration_ms", "spur_version"]),
        ),
        (
            "plan_created",
            BTreeSet::from(["brain_model", "duration_ms", "task_count", "spur_version"]),
        ),
        (
            "worker_dispatched",
            BTreeSet::from(["worker_model", "skill_used", "attempt_num", "spur_version"]),
        ),
        (
            "mcp_tool_called",
            BTreeSet::from(["server_name", "tool_name", "outcome", "spur_version"]),
        ),
        (
            "review_completed",
            BTreeSet::from(["outcome", "iteration_count", "spur_version"]),
        ),
        (
            "tui_view_opened",
            BTreeSet::from(["view_name", "spur_version"]),
        ),
    ])
}
