use rmcp::{model::CallToolRequestParams, ServiceExt};
use spur_notebook::mcp::{start_server, transport::LengthPrefixedJsonTransport};
use tokio::net::UnixStream;

#[tokio::test]
async fn notebook_ping_round_trips_over_unix_socket() {
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-")
        .tempdir()
        .expect("temp dir");
    let socket_path = temp_dir.path().join("notebook.sock");
    let _server = start_server(&socket_path).await.expect("server starts");

    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("client connects");
    let transport = LengthPrefixedJsonTransport::new(stream);
    let client = rmcp::model::ClientInfo::default()
        .serve(transport)
        .await
        .expect("client initializes");

    let result = client
        .call_tool(CallToolRequestParams::new("notebook.ping"))
        .await
        .expect("ping succeeds");

    assert_eq!(result.is_error, Some(false));
    let structured = result
        .structured_content
        .expect("ping returns structured content");
    assert_eq!(structured["ok"], true);
    assert_eq!(structured["tool"], "notebook.ping");

    client.cancel().await.expect("client closes");
}
