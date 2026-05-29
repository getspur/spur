use agent_client_protocol::schema::McpServer;
use spur_acp::{BrainSessionId, SessionId, SpurConfig, SpurEventBody};
use spur_core::Orchestrator;

#[test]
fn control_socket_path_maps_nonce_under_home() {
    let path = spur_core::notebook::control_socket_path("abc");

    assert!(path.ends_with(".spur/notebooks/sessions/abc.sock"));
}

#[test]
fn control_socket_path_distinguishes_socket_nonces() {
    let first = spur_core::notebook::control_socket_path("first");
    let second = spur_core::notebook::control_socket_path("second");

    assert_ne!(first, second);
}

#[test]
fn brain_mcp_servers_preinclude_notebook_stdio_proxy_on_nonce_socket() {
    let servers =
        spur_core::notebook::brain_mcp_servers("http://127.0.0.1:3939/mcp", "fixture-nonce");

    assert!(servers.iter().any(|server| match server {
        McpServer::Http(http) => http.name == "spur-mcp",
        _ => false,
    }));

    let notebook = servers
        .iter()
        .find_map(|server| match server {
            McpServer::Stdio(stdio) if stdio.name == "notebook" => Some(stdio),
            _ => None,
        })
        .expect("notebook MCP server should be preconfigured");

    assert_eq!(notebook.args[0], "--mcp-proxy");
    assert!(notebook.args[1].ends_with("/.spur/notebooks/sessions/fixture-nonce.sock"));
}

#[tokio::test]
async fn orchestrator_emits_one_notebook_socket_nonce_for_multiple_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = SpurConfig::default();
    config.cost.db_path = tmp.path().join("cost.db").display().to_string();
    let orchestrator =
        Orchestrator::new(tmp.path().to_path_buf(), config, None).expect("orchestrator");
    let mut events = orchestrator.subscribe();

    orchestrator.register_notebook_socket(BrainSessionId::from(SessionId("brain-one".to_string())));
    orchestrator.register_notebook_socket(BrainSessionId::from(SessionId("brain-two".to_string())));

    let first = next_notebook_socket_ready(&mut events).await;
    let second = next_notebook_socket_ready(&mut events).await;

    assert_ne!(first.0, second.0);
    assert_eq!(first.1, second.1);
    assert!(!first.1.is_empty());
}

async fn next_notebook_socket_ready(
    events: &mut tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>,
) -> (SessionId, String) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for NotebookSocketReady");
        let event = tokio::time::timeout(remaining, events.recv())
            .await
            .expect("timed out waiting for NotebookSocketReady")
            .expect("event broadcast should stay open");
        if let SpurEventBody::NotebookSocketReady {
            session,
            socket_nonce,
        } = event.body
        {
            return (session, socket_nonce);
        }
    }
}
