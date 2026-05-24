//! Regression test: `NativeAcpConnection::load_session` MUST propagate
//! agent-side errors. A prior bug replied `Ok(rx)` before awaiting the
//! upstream RPC, silently swallowing `-32002 Resource not found` and
//! causing downstream `session/prompt` calls to fire against dead ids.

use agent_client_protocol::schema::{InitializeRequest, ProtocolVersion};
use spur_acp::{connection::native::NativeAcpConnection, AgentConnection, LoadSessionRequest};

#[tokio::test(flavor = "multi_thread")]
async fn load_session_propagates_agent_error() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let stub = format!("{manifest_dir}/tests/fixtures/load_error_stub.py");

    // NativeAcpConnection::new(agent_name, command, extra_args, permission_tx)
    let mut conn = NativeAcpConnection::new("load-error-stub", "python3", vec![stub], None);

    conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("initialize should succeed against stub");

    let req = LoadSessionRequest::new(
        "nonexistent-uuid".to_string(),
        std::env::current_dir().unwrap(),
    );

    let result = conn.load_session(req).await;
    assert!(
        result.is_err(),
        "load_session MUST return Err when agent replies with -32002; \
         got Ok — error propagation regressed"
    );

    let err_msg = result.err().unwrap().to_string();
    assert!(
        err_msg.to_lowercase().contains("resource not found"),
        "error message should surface the upstream -32002 failure; got: {err_msg}"
    );

    let _ = conn.shutdown().await;
}
