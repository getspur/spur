//! Contract tests for the on-demand MCP server probe.

mod probe_fixture;

use std::time::Duration;

use spur_acp::config::{McpServerEntry, McpServerTransport};
use spur_mcp::{probe_server, probe_server_with_timeout, ProbeOutcome, DEFAULT_PROBE_TIMEOUT};

use probe_fixture::FIXTURE_MARKER;

fn fixture_entry(mode: &str) -> McpServerEntry {
    let test_executable = std::env::current_exe()
        .expect("resolve probe test executable")
        .display()
        .to_string();
    McpServerEntry {
        name: "fixture".into(),
        enabled: true,
        transport: McpServerTransport::Stdio {
            // libtest writes status lines to stdout before it calls the selected
            // test. Filter those lines so the child exposes a clean JSON-RPC
            // stdio channel while still re-executing this test binary.
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                r#""$1" --nocapture --exact probe_fixture::probe_fixture_server |
while IFS= read -r line; do
    case "$line" in
        \{*) printf '%s\n' "$line" ;;
    esac
done"#
                    .into(),
                "probe-fixture-wrapper".into(),
                test_executable,
            ],
            env: std::iter::once((FIXTURE_MARKER.into(), mode.into())).collect(),
        },
    }
}

#[tokio::test]
async fn stdio_probe_lists_fixture_tools() {
    assert_eq!(DEFAULT_PROBE_TIMEOUT, Duration::from_secs(10));

    let report = probe_server(&fixture_entry("normal")).await;
    assert_eq!(report.server_name, "fixture");

    let ProbeOutcome::ToolsListed(tools) = report.outcome else {
        panic!("expected ToolsListed, got {:?}", report.outcome);
    };
    assert_eq!(tools.len(), 2);

    let echo = tools
        .iter()
        .find(|tool| tool.name == "echo")
        .expect("echo tool is listed");
    assert_eq!(echo.description.as_deref(), Some("echo a string"));
    assert_eq!(
        echo.input_schema_json,
        serde_json::to_string(&probe_fixture::echo_schema()).expect("serialize expected schema")
    );
    assert!(tools.iter().any(|tool| tool.name == "add"));
}

#[tokio::test]
async fn missing_command_is_connect_error() {
    let entry = McpServerEntry {
        name: "missing".into(),
        enabled: true,
        transport: McpServerTransport::Stdio {
            command: "/nonexistent/spur-probe-missing".into(),
            args: Vec::new(),
            env: Default::default(),
        },
    };

    let report = probe_server_with_timeout(&entry, Duration::from_secs(5)).await;
    assert_eq!(report.server_name, "missing");
    let ProbeOutcome::ConnectError(message) = report.outcome else {
        panic!("expected ConnectError, got {:?}", report.outcome);
    };
    assert!(!message.is_empty());
}

#[tokio::test]
async fn closed_http_port_is_connect_error() {
    let entry = McpServerEntry {
        name: "closed".into(),
        enabled: true,
        transport: McpServerTransport::Http {
            url: "http://127.0.0.1:1/mcp".into(),
            headers: Default::default(),
        },
    };

    let report = probe_server_with_timeout(&entry, Duration::from_secs(5)).await;
    let ProbeOutcome::ConnectError(message) = report.outcome else {
        panic!("expected ConnectError, got {:?}", report.outcome);
    };
    assert!(!message.is_empty());
}

#[tokio::test]
async fn sleeping_fixture_times_out() {
    let report =
        probe_server_with_timeout(&fixture_entry("sleep"), Duration::from_millis(150)).await;

    assert!(matches!(report.outcome, ProbeOutcome::Timeout));
}
