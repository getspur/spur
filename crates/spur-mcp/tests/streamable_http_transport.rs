use rmcp::{
    model::{Implementation, ServerCapabilities, ServerInfo},
    ServerHandler,
};
use spur_mcp::server::{start_streamable_http_server, StreamableHttpTransportConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::task::AbortOnDropHandle;

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

#[tokio::test]
async fn forced_shutdown_joins_an_accepted_half_read_connection() {
    let callback_joined = Arc::new(AtomicBool::new(false));
    let callback_joined_from_task = Arc::clone(&callback_joined);
    let callback_child = AbortOnDropHandle::new(tokio::spawn(async {
        std::future::pending::<()>().await;
    }));
    let mut transport = start_streamable_http_server(
        || Ok(EmptyServer),
        StreamableHttpTransportConfig::default(),
        async move {
            callback_child.abort();
            let _ = callback_child.await;
            callback_joined_from_task.store(true, Ordering::SeqCst);
        },
    )
    .await
    .expect("transport should bind a loopback listener");

    let authority = transport
        .url
        .strip_prefix("http://")
        .expect("loopback HTTP URL")
        .split('/')
        .next()
        .expect("URL authority");
    let mut client = tokio::net::TcpStream::connect(authority)
        .await
        .expect("client should connect");
    client
        .write_all(b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: 128\r\n\r\n{")
        .await
        .expect("partial request should be admitted");

    tokio::time::timeout(Duration::from_secs(1), async {
        while transport.connection_tasks.len() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the connection should be admitted before shutdown");

    transport.force_shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), &mut transport.root_handle)
        .await
        .expect("forced root shutdown should be bounded")
        .expect("root task should join");
    transport.connection_tasks.close();
    tokio::time::timeout(Duration::from_secs(1), transport.connection_tasks.wait())
        .await
        .expect("forced shutdown should join every accepted connection");
    assert!(
        callback_joined.load(Ordering::SeqCst),
        "forced root shutdown must join its server-specific child callback"
    );
}
