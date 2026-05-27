//! Integration tests for the `LicenseSeat` admin API client.

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test]
async fn create_license_sends_post_with_bearer_auth() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);

        assert!(
            request.contains("POST /products/test-product/licenses HTTP/1.1"),
            "expected POST to licenses endpoint, got: {request}"
        );
        assert!(
            request.contains("authorization: Bearer sk_test_xxx"),
            "expected Bearer auth"
        );
        assert!(
            request.contains("application/json"),
            "expected JSON content type"
        );
        assert!(
            request.contains("\"plan_key\":\"pro\""),
            "expected plan_key in body"
        );

        let body = r#"{"license_key":"NEW-KEY-9999","plan_key":"pro","seats":1}"#;
        let response = format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let client = spur_license_admin::api::AdminClient::new(
        "sk_test_xxx",
        "test-product",
        &format!("http://127.0.0.1:{port}"),
    );

    let result = client.create_license("pro", None, None).await;

    server.await.unwrap();
    let value = result.expect("create_license should succeed");
    assert_eq!(
        value.get("license_key").and_then(|v| v.as_str()),
        Some("NEW-KEY-9999"),
        "create_license must return the parsed JSON body so the operator learns the new key"
    );
}

#[tokio::test]
async fn list_licenses_sends_get_with_bearer_auth() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);

        assert!(
            request.contains("GET /products/test-product/licenses HTTP/1.1"),
            "expected GET to licenses endpoint"
        );
        assert!(
            request.contains("authorization: Bearer sk_test_xxx"),
            "expected Bearer auth"
        );

        let body = r#"{"licenses":[],"total":0}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let client = spur_license_admin::api::AdminClient::new(
        "sk_test_xxx",
        "test-product",
        &format!("http://127.0.0.1:{port}"),
    );

    let result = client.list_licenses().await;

    server.await.unwrap();
    let value = result.expect("list_licenses should succeed");
    assert_eq!(
        value.get("total").and_then(|v| v.as_u64()),
        Some(0),
        "list_licenses must return the parsed JSON body"
    );
}

#[tokio::test]
async fn revoke_license_sends_delete_with_bearer_auth() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);

        assert!(
            request.contains("DELETE /products/test-product/licenses/TEST-KEY-1234 HTTP/1.1"),
            "expected DELETE to license endpoint, got: {request}"
        );
        assert!(
            request.contains("authorization: Bearer sk_test_xxx"),
            "expected Bearer auth"
        );

        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    let client = spur_license_admin::api::AdminClient::new(
        "sk_test_xxx",
        "test-product",
        &format!("http://127.0.0.1:{port}"),
    );

    let result = client.revoke_license("TEST-KEY-1234").await;

    server.await.unwrap();
    let value = result.expect("revoke_license should succeed");
    assert!(
        value.is_null(),
        "revoke_license must return Null on 204 No Content; got {value:?}"
    );
}

#[tokio::test]
async fn list_activations_sends_get_with_license_key() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);

        assert!(
            request.contains("GET /products/test-product/licenses/TEST-KEY/activations HTTP/1.1"),
            "expected GET to activations endpoint, got: {request}"
        );
        assert!(
            request.contains("authorization: Bearer sk_test_xxx"),
            "expected Bearer auth"
        );

        let body = r#"{"activations":[],"total":0}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let client = spur_license_admin::api::AdminClient::new(
        "sk_test_xxx",
        "test-product",
        &format!("http://127.0.0.1:{port}"),
    );

    let result = client.list_activations("TEST-KEY").await;

    server.await.unwrap();
    let value = result.expect("list_activations should succeed");
    assert_eq!(
        value.get("total").and_then(|v| v.as_u64()),
        Some(0),
        "list_activations must return the parsed JSON body"
    );
}
