use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::time::{sleep, timeout};

async fn read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await.expect("frame length");
    let len = u32::from_be_bytes(len) as usize;
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await.expect("frame body");
    bytes
}

async fn write_frame(stream: &mut tokio::net::UnixStream, bytes: &[u8]) {
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .expect("write frame length");
    stream.write_all(bytes).await.expect("write frame body");
    stream.flush().await.expect("flush frame");
}

#[tokio::test]
async fn send_notebook_command_uses_supplied_socket_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind mock socket");

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept command");
        let request = read_frame(&mut stream).await;
        let request: serde_json::Value = serde_json::from_slice(&request).expect("request json");
        assert_eq!(request["daemon"], "notebook.v1");
        assert_eq!(request["command"], "open");
        assert_eq!(request["path"], json!("chosen.ipynb"));

        let response = serde_json::to_vec(&json!({
            "ok": true,
            "path": null,
            "error": null
        }))
        .expect("response json");
        write_frame(&mut stream, &response).await;
    });

    let response = spur_tui::notebook_daemon::send_notebook_command("chosen.ipynb", &socket_path)
        .await
        .expect("command response");

    assert!(response.ok);
    server.await.expect("server task");
}

#[tokio::test]
async fn send_notebook_command_retries_until_lazy_socket_binds() {
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("control.sock");
    let server_socket_path = socket_path.clone();

    let server = tokio::spawn(async move {
        sleep(Duration::from_millis(300)).await;
        let listener = UnixListener::bind(&server_socket_path).expect("bind mock socket");
        let (mut stream, _) = listener.accept().await.expect("accept command");
        let request = read_frame(&mut stream).await;
        let request: serde_json::Value = serde_json::from_slice(&request).expect("request json");
        assert_eq!(request["daemon"], "notebook.v1");
        assert_eq!(request["command"], "new");
        assert!(request.get("path").is_none());

        let response = serde_json::to_vec(&json!({
            "ok": true,
            "path": null,
            "error": null
        }))
        .expect("response json");
        write_frame(&mut stream, &response).await;
    });

    match spur_tui::notebook_daemon::send_notebook_command("new", &socket_path).await {
        Ok(response) => assert!(response.ok),
        Err(error) => {
            server.abort();
            panic!("command should retry until lazy socket bind: {error}");
        }
    }
    server.await.expect("server task");
}

#[tokio::test]
async fn send_notebook_command_does_not_retry_protocol_error_after_connect() {
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind mock socket");
    let (second_accept_tx, second_accept_rx) = tokio::sync::oneshot::channel();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept command");
        let request = read_frame(&mut stream).await;
        let request: serde_json::Value = serde_json::from_slice(&request).expect("request json");
        assert_eq!(request["command"], "new");
        write_frame(&mut stream, b"not json").await;

        if let Ok((mut stream, _)) = listener.accept().await {
            let _ = second_accept_tx.send(());
            write_frame(&mut stream, b"not json").await;
        }
    });

    let result = timeout(
        Duration::from_millis(250),
        spur_tui::notebook_daemon::send_notebook_command("new", &socket_path),
    )
    .await
    .expect("protocol errors should fail without retry");
    assert!(result.is_err());

    assert!(
        timeout(Duration::from_millis(150), second_accept_rx)
            .await
            .is_err(),
        "protocol error after connect should not trigger a second connection"
    );
    server.abort();
}
