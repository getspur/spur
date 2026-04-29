use spur_bot::telegram::client::TelegramClient;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Instant};

#[tokio::test]
async fn send_html_to_thread_fallback_on_parse_error() {
    assert_html_error_falls_back(
        "400 Bad Request",
        telegram_error_body(
            400,
            "Bad Request: can't parse entities: Can't find end of the entity",
        ),
    )
    .await;
}

#[tokio::test]
async fn send_html_to_thread_fallback_on_message_too_long_400() {
    assert_html_error_falls_back(
        "400 Bad Request",
        telegram_error_body(400, "Bad Request: message is too long"),
    )
    .await;
}

#[tokio::test]
async fn send_html_to_thread_fallback_on_arbitrary_400_with_html_parse_mode() {
    assert_html_error_falls_back(
        "400 Bad Request",
        telegram_error_body(400, "Bad Request: unrelated message"),
    )
    .await;
}

#[tokio::test]
async fn send_html_to_thread_does_not_fallback_on_429() {
    let (result, requests, paused_now, paused_later) = send_html_error_and_capture_retry(
        "429 Too Many Requests",
        r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#.into(),
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "429 response should be returned as an error"
    );
    assert_eq!(requests.len(), 1, "429 must not retry as plain text");
    assert!(paused_now, "retry_after 429 should activate client pause");
    assert!(
        !paused_later,
        "client pause should clear after retry_after window"
    );
}

#[tokio::test]
async fn send_html_to_thread_does_not_fallback_on_403() {
    let (result, requests, paused_now, _) = send_html_error_and_capture_retry(
        "403 Forbidden",
        telegram_error_body(403, "Forbidden: bot was blocked by the user"),
        None,
    )
    .await;

    assert!(
        result.is_err(),
        "403 response should be returned as an error"
    );
    assert_eq!(requests.len(), 1, "403 must not retry as plain text");
    assert!(!paused_now, "403 should not activate client pause");
}

#[tokio::test]
async fn send_html_to_thread_does_not_fallback_on_500_server_error() {
    assert_api_error_does_not_fallback(
        "500 Internal Server Error",
        telegram_error_body(500, "Internal Server Error"),
        500,
        "Internal Server Error",
    )
    .await;
}

#[tokio::test]
async fn send_html_to_thread_does_not_fallback_on_503_service_unavailable() {
    assert_api_error_does_not_fallback(
        "503 Service Unavailable",
        telegram_error_body(503, "Service Unavailable"),
        503,
        "Service Unavailable",
    )
    .await;
}

#[tokio::test]
async fn send_html_to_thread_does_not_fallback_on_json_decode_error() {
    let (result, requests, paused_now, _) = send_html_error_and_capture_retry(
        "200 OK",
        r#"{"ok":true,"result":NOT_VALID_JSON"#.into(),
        None,
    )
    .await;

    assert_json_decode_error(result);
    assert_no_retry(requests, paused_now, "JSON decode errors");
}

#[tokio::test]
async fn send_html_to_thread_propagates_fallback_400_after_html_400() {
    let (result, requests, _, _) = send_html_error_and_capture_retry(
        "400 Bad Request",
        telegram_error_body(400, "Bad Request: can't parse entities"),
        Some((
            "400 Bad Request",
            telegram_error_body(400, "Bad Request: chat not found"),
        )),
    )
    .await;

    assert_api_error(result, 400, "Bad Request: chat not found");
    assert_eq!(requests.len(), 2, "fallback error must not loop");
    assert!(requests[0].contains("\"parse_mode\":\"HTML\""));
    assert!(
        !requests[1].contains("parse_mode"),
        "fallback request must not set parse_mode: {}",
        requests[1]
    );
}

#[tokio::test]
async fn send_html_to_thread_falls_back_on_400_with_empty_description() {
    assert_html_error_falls_back("400 Bad Request", telegram_error_body(400, "")).await;
}

#[tokio::test(flavor = "current_thread")]
async fn send_html_to_thread_fallback_respects_pause_set_after_first_send() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind telegram test listener");
    let addr = listener.local_addr().expect("listener local addr");
    let client = TelegramClient::new_with_url(format!("http://{addr}/"), Duration::from_secs(2))
        .expect("client with custom api url should build");
    let pauser = client.clone();
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();
    let _server = tokio::spawn(async move {
        let mut requests = Vec::new();

        let (mut stream, _) = listener.accept().await.expect("accept first request");
        requests.push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
        let body = telegram_error_body(400, "Bad Request: can't parse entities");
        write_response(&mut stream, "400 Bad Request", &body).await;
        let pause_until = Instant::now() + Duration::from_millis(250);
        pauser.pause_until_at_least(pause_until);
        drop(stream);

        let (mut stream, _) = listener.accept().await.expect("accept fallback request");
        let fallback_at = Instant::now();
        requests.push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
        let body = telegram_message_body(655, "plain fallback");
        write_response(&mut stream, "200 OK", &body).await;

        let _ = request_tx.send((requests, pause_until, fallback_at));
    });

    client
        .send_html_to_thread(42, None, "<b>hello</b>".into(), "plain fallback".into())
        .await
        .expect("HTML 400 should fall back to plain text after pause");

    let (requests, pause_until, fallback_at) =
        request_rx.await.expect("server should capture requests");
    assert_eq!(requests.len(), 2);
    assert!(
        fallback_at >= pause_until,
        "fallback fired before pause elapsed: {fallback_at:?} < {pause_until:?}"
    );
}

async fn assert_html_error_falls_back(status: &'static str, body: String) {
    let fallback_body = telegram_message_body(654, "plain fallback");
    let (result, requests, _, _) =
        send_html_error_and_capture_retry(status, body, Some(("200 OK", fallback_body))).await;

    result.expect("HTML 400 should fall back to plain text");

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

async fn send_html_error_and_capture_retry(
    status: &'static str,
    body: String,
    fallback_response: Option<(&'static str, String)>,
) -> (anyhow::Result<()>, Vec<String>, bool, bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind telegram test listener");
    let addr = listener.local_addr().expect("listener local addr");
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();
    let _server = tokio::spawn(async move {
        let mut requests = Vec::new();

        let (mut stream, _) = listener.accept().await.expect("accept request");
        requests.push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
        write_response(&mut stream, status, &body).await;

        if let Some((fallback_status, fallback_body)) = fallback_response {
            let (mut stream, _) = listener.accept().await.expect("accept second request");
            requests
                .push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
            write_response(&mut stream, fallback_status, &fallback_body).await;
        } else if let Ok(Ok((mut stream, _))) =
            timeout(Duration::from_millis(200), listener.accept()).await
        {
            requests
                .push(String::from_utf8_lossy(&read_http_request(&mut stream).await).into_owned());
            let body = telegram_message_body(655, "plain fallback");
            write_response(&mut stream, "200 OK", &body).await;
        }

        let _ = request_tx.send(requests);
    });

    let client = TelegramClient::new_with_url(format!("http://{addr}/"), Duration::from_secs(1))
        .expect("client with custom api url should build");

    let result = client
        .send_html_to_thread(42, None, "<b>broken".into(), "plain fallback".into())
        .await;

    let paused_now = client.is_paused(Instant::now());
    let paused_later = client.is_paused(Instant::now() + Duration::from_secs(8));
    let requests = request_rx.await.expect("server should capture requests");
    (result, requests, paused_now, paused_later)
}

async fn assert_api_error_does_not_fallback(
    status: &'static str,
    body: String,
    error_code: u64,
    description: &str,
) {
    let (result, requests, paused_now, _) =
        send_html_error_and_capture_retry(status, body, None).await;

    assert_api_error(result, error_code, description);
    assert_no_retry(requests, paused_now, description);
}

fn assert_no_retry(requests: Vec<String>, paused_now: bool, reason: &str) {
    assert_eq!(requests.len(), 1, "{reason} must not retry as plain text");
    assert!(!paused_now, "{reason} should not activate client pause");
}

fn assert_api_error(result: anyhow::Result<()>, error_code: u64, description: &str) {
    let err = result.expect_err("telegram API error should propagate");
    let telegram_err = err.downcast_ref::<frankenstein::Error>();
    assert!(
        matches!(
            telegram_err,
            Some(frankenstein::Error::Api(response))
                if response.error_code == error_code && response.description == description
        ),
        "frankenstein API error expected, got {telegram_err:?}"
    );
}

fn assert_json_decode_error(result: anyhow::Result<()>) {
    let err = result.expect_err("telegram JSON decode error should propagate");
    let telegram_err = err.downcast_ref::<frankenstein::Error>();
    assert!(
        matches!(telegram_err, Some(frankenstein::Error::JsonDecode { .. })),
        "frankenstein JSON decode error expected, got {telegram_err:?}"
    );
}

fn telegram_error_body(error_code: u64, description: &str) -> String {
    format!(r#"{{"ok":false,"error_code":{error_code},"description":"{description}"}}"#)
}

fn telegram_message_body(message_id: i32, text: &str) -> String {
    format!(
        r#"{{"ok":true,"result":{{"message_id":{message_id},"date":0,"chat":{{"id":42,"type":"private"}},"text":"{text}"}}}}"#
    )
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
