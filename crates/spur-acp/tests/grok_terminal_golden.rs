//! Replay fixed wire requests through the real Rust ACP terminal handlers.
#![cfg(unix)]

use std::{path::Path, time::Duration};

use agent_client_protocol::schema::{
    v1::{ContentBlock, InitializeRequest, PromptRequest, SessionId, StopReason, TextContent},
    ProtocolVersion,
};
use serde_json::Value;
use spur_acp::{
    connection::{native::NativeAcpConnection, AgentConnection},
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
    assert_eq!(response.stop_reason, StopReason::EndTurn);
}

async fn replay(action: &str) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().expect("isolated fixture directory");
    let report_path = temp.path().join("report.json");
    let mut conn = NativeAcpConnection::new_with_kind(
        "grok-golden",
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
        AgentKind::Grok,
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
    let report: Value =
        serde_json::from_slice(&std::fs::read(report_path).expect("fixture report"))
            .expect("valid fixture report");
    println!("GROK_GOLDEN_REPORT={report}");
    assert_eq!(
        report["follow_up"], true,
        "same-session terminal continuation"
    );
    report
}

#[tokio::test(flavor = "multi_thread")]
async fn grok_golden_scripts_execute_and_continue_via_native_acp() {
    let report = replay("golden").await;
    let corpus: Value = serde_json::from_str(include_str!("fixtures/grok_terminal_golden.json"))
        .expect("golden corpus");
    let cases = report["cases"].as_array().expect("case results");
    assert_eq!(cases.len(), corpus["cases"].as_array().unwrap().len());
    for case in cases {
        assert!(
            case["status"] == "pass" || case["status"] == "skipped",
            "{case}"
        );
        if case["status"] == "skipped" {
            eprintln!("GROK_GOLDEN_SKIP={case}");
        }
    }
    assert!(cases
        .iter()
        .any(|case| case["id"] == "python_heredoc" && case["status"] == "pass"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "bd-3stsw: inherited-pipe delay; run explicitly and retain the failing verdict"]
async fn grok_golden_inherited_pipe_must_not_delay_command_exit() {
    let report = replay("inherited-pipe").await;
    let cases = report["lifecycle"]["cases"]
        .as_array()
        .expect("lifecycle cases");
    assert_eq!(cases.len(), 2, "split control and packed wrapper");
    for case in cases {
        assert_eq!(case["parent_exit_observed"], true, "{case}");
    }
    assert_eq!(report["lifecycle"]["verdict"], "pass",
        "command has exited, but terminal/wait_for_exit waited for the descendant's output pipe; this is a lifecycle failure, not an argv failure");
}
