use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use spur_acp::connection::{native::NativeAcpConnection, AgentClientRequestKind, AgentConnection};

#[tokio::test(flavor = "multi_thread")]
async fn agent_logout_request_is_logged_signaled_and_acknowledged() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script_path = format!("{manifest_dir}/tests/fixtures/agent_logout_request.sh");
    assert!(
        std::path::Path::new(&script_path).exists(),
        "fixture missing at {script_path}"
    );

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let response_path =
        std::env::temp_dir().join(format!("spur-acp-agent-logout-response-{unique}.json"));
    let response_path_arg = response_path.to_string_lossy().to_string();

    let mut conn = NativeAcpConnection::new(
        "mock-agent-logout",
        "bash",
        vec![script_path, response_path_arg],
        None,
    );
    let mut request_rx = conn
        .take_agent_client_request_rx()
        .expect("native ACP connection should expose agent client-request events");

    conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("initialize should succeed against mock");

    let payload = tokio::time::timeout(Duration::from_secs(1), request_rx.recv())
        .await
        .expect("logout request should be signaled")
        .expect("agent client-request channel should remain open");
    assert_eq!(payload.kind, AgentClientRequestKind::Logout);

    let response = wait_for_file(&response_path, Duration::from_secs(1)).await;
    let _ = conn.shutdown().await;
    let _ = std::fs::remove_file(&response_path);

    assert!(
        response.contains(r#""result":{}"#),
        "logout should receive an empty success response, got {response}"
    );
    assert!(
        !response.contains(r#""error""#),
        "logout should not fall through to method_not_found, got {response}"
    );
}

async fn wait_for_file(path: &std::path::Path, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match std::fs::read_to_string(path) {
            Ok(contents) => return contents,
            Err(err) if tokio::time::Instant::now() < deadline => {
                let _ = err;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(err) => panic!("timed out waiting for {}: {err}", path.display()),
        }
    }
}
