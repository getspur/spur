use rmcp::{
    model::{Implementation, ServerCapabilities, ServerInfo},
    ServerHandler,
};
use spur_mcp::server::{start_streamable_http_server, StreamableHttpTransportConfig};

#[derive(Clone)]
struct EmptyServer;

impl ServerHandler for EmptyServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut implementation = Implementation::default();
        implementation.name = "empty-test-server".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }
}

#[tokio::test]
async fn streamable_http_transport_binds_mcp_path_and_shutdown_signal_finishes() {
    let transport = start_streamable_http_server(
        || Ok(EmptyServer),
        StreamableHttpTransportConfig::default(),
        async {},
    )
    .await
    .expect("transport should bind a loopback listener");

    assert!(transport.url.starts_with("http://127.0.0.1:"));
    assert!(transport.url.ends_with("/mcp"));

    let _ = transport.shutdown_tx.send(());
    transport.root_handle.await.expect("root task should join");
    transport.done_rx.await.expect("done signal should be sent");
}
