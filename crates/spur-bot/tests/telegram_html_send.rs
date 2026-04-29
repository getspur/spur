use spur_bot::telegram::client::TelegramClient;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

#[tokio::test]
async fn send_html_to_thread_falls_back_to_plain_on_parse_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind telegram test listener");
    let addr = listener.local_addr().expect("listener local addr");
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();
    let _server = tokio::spawn(async move {
        let mut requests = Vec::new();

        let (mut stream, _) = listener.accept().await.expect("accept first request");
        requests.push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
        let body = r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: Can't find end of the entity"}"#;
        write_response(&mut stream, "400 Bad Request", body).await;

        let (mut stream, _) = listener.accept().await.expect("accept second request");
        requests.push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
        let body = r#"{"ok":true,"result":{"message_id":654,"message_thread_id":77,"date":0,"chat":{"id":42,"type":"private"},"text":"plain fallback"}}"#;
        write_response(&mut stream, "200 OK", body).await;

        let _ = request_tx.send(requests);
    });

    let client = TelegramClient::new_with_url(format!("http://{addr}/"), Duration::from_secs(1))
        .expect("client with custom api url should build");

    client
        .send_html_to_thread(42, Some(77), "<b>broken".into(), "plain fallback".into())
        .await
        .expect("parse error should fall back to plain text");

    let requests = request_rx.await.expect("server should capture requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"parse_mode\":\"HTML\""));
    assert!(requests[0].contains("\"text\":\"<b>broken\""));
    assert!(requests[1].contains("\"text\":\"plain fallback\""));
    assert!(
        !requests[1].contains("parse_mode"),
        "fallback request must not set parse_mode: {}",
        requests[1]
    );
}

#[tokio::test]
async fn send_html_to_thread_respects_rate_limit_pause() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind telegram test listener");
    let addr = listener.local_addr().expect("listener local addr");
    let _server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let _request = read_http_request(&mut stream).await;

        let body = r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#;
        write_response(&mut stream, "429 Too Many Requests", body).await;
    });

    let client = TelegramClient::new_with_url(format!("http://{addr}/"), Duration::from_secs(1))
        .expect("client with custom api url should build");

    let result = client
        .send_html_to_thread(42, None, "<b>hello</b>".into(), "hello".into())
        .await;

    assert!(
        result.is_err(),
        "429 response should be returned as an error"
    );
    assert!(
        client.is_paused(Instant::now()),
        "retry_after 429 should activate client pause"
    );
    assert!(
        !client.is_paused(Instant::now() + Duration::from_secs(8)),
        "client pause should clear after retry_after window"
    );
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let n = stream.read(&mut chunk).await.expect("read request");
        if n == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..n]);

        let Some(header_end) = find_header_end(&request) else {
            continue;
        };
        let content_length = content_length(&request[..header_end]).unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            break;
        }
    }
    request
}

async fn write_response(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write telegram response");
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(headers)
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
}
