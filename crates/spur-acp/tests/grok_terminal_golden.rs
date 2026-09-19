//! Replay fixed wire requests through the real Rust ACP terminal handlers.
#![cfg(unix)]

use std::{path::Path, time::Duration};

use agent_client_protocol::schema::{
    v1::{ContentBlock, InitializeRequest, PromptRequest, SessionId, StopReason, TextContent},
    ProtocolVersion,
};
use serde_json::Value;
use spur_acp::{
    connection::{native::NativeAcpConnection, AgentConnection as _},
    types::AgentKind,
};

async fn prompt(conn: &mut NativeAcpConnection, session: &SessionId, text: &str) {
    let _stream = conn
        .prompt(PromptRequest::new(
            session.clone(),
            vec![ContentBlock::Text(TextContent::new(text))],
        ))
        .await
        .expect("start prompt");
    let response = tokio::time::timeout(Duration::from_secs(60), conn.wait_for_prompt_response())
        .await
        .expect("bounded golden prompt completion")
        .expect("golden peer assertions and callbacks must pass")
        .expect("native prompt response");
    assert_eq!(
        response.stop_reason,
        StopReason::EndTurn,
        "golden prompt must finish normally"
    );
}

async fn replay(kind: AgentKind, action: &str) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().expect("isolated fixture directory");
    let report_path = temp.path().join("report.json");
    let mut conn = NativeAcpConnection::new_with_kind(
        "terminal-golden",
        "python3",
        vec![
            root.join("tests/fixtures/grok_terminal_golden_peer.py")
                .display()
                .to_string(),
            root.join("tests/fixtures/grok_terminal_golden.json")
                .display()
                .to_string(),
            report_path.display().to_string(),
        ],
        kind,
        None,
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST)),
    )
    .await
    .expect("initialize timeout")
    .expect("initialize peer");
    let session = conn
        .new_session(temp.path().to_path_buf(), vec![])
        .await
        .expect("create session");
    prompt(&mut conn, &session.session_id, action).await;
    prompt(&mut conn, &session.session_id, "continue").await;
    conn.shutdown().await.expect("shutdown fixture peer");
    let mut report: Value =
        serde_json::from_slice(&std::fs::read(report_path).expect("fixture report"))
            .expect("valid fixture report");
    if action == "shutdown-cleanup" {
        let case = &report["cleanup"][0];
        let pids = case["pids"].as_array().expect("process IDs");
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let stopped = loop {
            let all_stopped = pids.iter().all(|pid| {
                let output = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .expect("inspect fixture process");
                let state = String::from_utf8_lossy(&output.stdout);
                state.trim().is_empty() || state.trim().starts_with('Z')
            });
            if all_stopped || std::time::Instant::now() >= deadline {
                break all_stopped;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        std::fs::write(case["release_path"].as_str().unwrap(), "").expect("release fixture child");
        report["shutdown_processes_stopped"] = stopped.into();
    }
    report["agent_kind"] = format!("{kind:?}").into();
    println!("GROK_GOLDEN_REPORT={report}");
    assert_eq!(
        report["follow_up"], true,
        "same-session terminal continuation"
    );
    report
}

#[tokio::test(flavor = "multi_thread")]
async fn grok_golden_scripts_execute_and_continue_via_native_acp() {
    let report = replay(AgentKind::Grok, "golden").await;
    let corpus: Value = serde_json::from_str(include_str!("fixtures/grok_terminal_golden.json"))
        .expect("golden corpus");
    let cases = report["cases"].as_array().expect("case results");
    assert_eq!(
        cases.len(),
        corpus["cases"].as_array().unwrap().len(),
        "report must account for every fixture"
    );
    for case in cases {
        assert!(
            case["status"] == "pass" || case["status"] == "skipped",
            "{case}"
        );
        if case["status"] == "skipped" {
            eprintln!("GROK_GOLDEN_SKIP={case}");
        }
    }
    assert!(
        cases
            .iter()
            .any(|case| case["id"] == "python_heredoc" && case["status"] == "pass"),
        "required Python heredoc case must execute successfully"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn native_terminal_inherited_pipe_must_not_delay_command_exit() {
    for kind in [AgentKind::Grok, AgentKind::Generic] {
        let action = if kind == AgentKind::Grok {
            "inherited-pipe"
        } else {
            "inherited-pipe-split"
        };
        let report = replay(kind, action).await;
        let cases = report["lifecycle"]["cases"]
            .as_array()
            .expect("lifecycle cases");
        let expected_cases = if kind == AgentKind::Grok { 2 } else { 1 };
        assert_eq!(cases.len(), expected_cases, "{kind:?}: command forms");
        for case in cases {
            assert_eq!(case["parent_exit_observed"], true, "{case}");
        }
        assert_eq!(report["lifecycle"]["verdict"], "pass",
            "command has exited, but terminal/wait_for_exit waited for the descendant's output pipe; this is a lifecycle failure, not an argv failure");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_terminal_kill_and_release_clean_up_descendants() {
    for kind in [AgentKind::Grok, AgentKind::Generic] {
        let report = replay(kind, "cleanup").await;
        let cases = report["cleanup"].as_array().expect("cleanup cases");
        assert_eq!(
            cases.len(),
            4,
            "kill/release must cover running and exited parents"
        );
        for case in cases {
            assert_eq!(case["processes_stopped"], true, "{case}");
            if case["parent_exits"] == true {
                assert_eq!(case["exit_published_before_cleanup"], true, "{case}");
            }
            if case["method"] == "kill" {
                assert_eq!(case["output_retained"], true, "{case}");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_terminal_shutdown_cleans_up_exited_parent_descendants() {
    for kind in [AgentKind::Grok, AgentKind::Generic] {
        let report = replay(kind, "shutdown-cleanup").await;
        assert_eq!(
            report["cleanup"][0]["exit_published_before_cleanup"], true,
            "{report}"
        );
        assert_eq!(report["shutdown_processes_stopped"], true, "{report}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_terminal_exit_preserves_final_output_and_truncation() {
    for kind in [AgentKind::Grok, AgentKind::Generic] {
        let report = replay(kind, "output-stress").await;
        let cases = report["output_stress"]
            .as_array()
            .expect("output stress cases");
        assert_eq!(cases.len(), 2, "verify full and byte-limited output");
        for case in cases {
            assert_eq!(case["status"], "pass", "{case}");
        }
    }
}
