use anyhow::Context;
use frankenstein::AsyncTelegramApi;
use std::{
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

const PAUSE_EXTENSION_LOG_THRESHOLD: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct TelegramClient {
    inner: frankenstein::client_reqwest::Bot,
    draft_pause: DraftPauseState,
}

#[derive(Debug, Clone)]
struct DraftPauseState {
    paused_until: Arc<RwLock<Option<Instant>>>,
}

impl DraftPauseState {
    fn new() -> Self {
        Self {
            paused_until: Arc::new(RwLock::new(None)),
        }
    }

    fn paused_until(&self) -> Option<Instant> {
        *self
            .paused_until
            .read()
            .expect("telegram draft pause lock poisoned")
    }

    fn is_paused(&self, now: Instant) -> bool {
        self.paused_until()
            .is_some_and(|paused_until| now < paused_until)
    }

    fn pause_until_at_least(&self, candidate: Instant) {
        let now = Instant::now();
        let mut paused_until = self
            .paused_until
            .write()
            .expect("telegram draft pause lock poisoned");
        let previous = *paused_until;
        if previous.is_none_or(|current| candidate > current) {
            *paused_until = Some(candidate);
        }
        if previous.is_none_or(|current| candidate >= current + PAUSE_EXTENSION_LOG_THRESHOLD) {
            tracing::info!(
                secs = candidate.saturating_duration_since(now).as_secs(),
                "telegram rate limit pause active"
            );
        }
    }
}

impl TelegramClient {
    pub fn new(token: &str, request_timeout: Duration) -> anyhow::Result<Self> {
        Self::new_with_url(
            format!("{}{}", frankenstein::BASE_API_URL, token),
            request_timeout,
        )
    }

    pub fn new_with_url(url: String, request_timeout: Duration) -> anyhow::Result<Self> {
        let http = frankenstein::reqwest::ClientBuilder::new()
            .connect_timeout(Duration::from_secs(10))
            .timeout(request_timeout)
            .build()
            .context("building reqwest client for telegram bot")?;
        Ok(Self {
            inner: frankenstein::client_reqwest::Bot::builder()
                .api_url(url)
                .client(http)
                .build(),
            draft_pause: DraftPauseState::new(),
        })
    }

    pub fn is_paused(&self, now: Instant) -> bool {
        self.draft_pause.is_paused(now)
    }

    pub fn pause_until_at_least(&self, candidate: Instant) {
        self.draft_pause.pause_until_at_least(candidate);
    }

    pub(crate) async fn wait_if_paused(&self) {
        loop {
            let Some(paused_until) = self.draft_pause.paused_until() else {
                return;
            };
            if Instant::now() >= paused_until {
                return;
            }
            tokio::time::sleep_until(paused_until + retry_after_jitter()).await;
        }
    }

    pub async fn delete_webhook(&self) -> anyhow::Result<()> {
        self.inner
            .delete_webhook(&frankenstein::methods::DeleteWebhookParams::builder().build())
            .await?;
        Ok(())
    }

    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u64,
    ) -> anyhow::Result<Vec<frankenstein::updates::Update>> {
        let params = frankenstein::methods::GetUpdatesParams::builder()
            .offset(offset)
            .timeout(timeout_secs as u32)
            .build();
        let response = self.inner.get_updates(&params).await?;
        Ok(response.result)
    }

    pub async fn get_me(&self) -> anyhow::Result<frankenstein::types::User> {
        Ok(self.inner.get_me().await?.result)
    }

    pub async fn create_forum_topic(
        &self,
        chat_id: i64,
        name: String,
    ) -> anyhow::Result<frankenstein::types::ForumTopic> {
        let params = frankenstein::methods::CreateForumTopicParams::builder()
            .chat_id(chat_id)
            .name(name)
            .build();
        Ok(self.inner.create_forum_topic(&params).await?.result)
    }

    pub async fn send_message_draft(
        &self,
        chat_id: i64,
        draft_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.send_message_draft_to_thread(chat_id, None, draft_id, text)
            .await
    }

    pub async fn send_message_draft_to_thread(
        &self,
        chat_id: i64,
        message_thread_id: Option<i32>,
        draft_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "draft_id": encode_draft_id(draft_id),
            "text": text,
        });
        if let Some(thread_id) = normalize_outbound_thread_id(message_thread_id) {
            payload["message_thread_id"] = serde_json::json!(thread_id);
        }
        self.wait_if_paused().await;
        let result: Result<frankenstein::response::MethodResponse<bool>, frankenstein::Error> =
            self.inner.request("sendMessageDraft", Some(payload)).await;
        if let Err(err) = &result {
            self.pause_after_telegram_retry_after(err);
        }
        let _ = result?;
        Ok(())
    }

    pub async fn send_text(&self, chat_id: i64, text: String) -> anyhow::Result<()> {
        self.send_text_to_thread(chat_id, None, text).await
    }

    pub async fn send_text_to_thread(
        &self,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: String,
    ) -> anyhow::Result<()> {
        let params = build_send_text_params(chat_id, message_thread_id, text);
        self.wait_if_paused().await;
        let result = self.inner.send_message(&params).await;
        if let Err(err) = &result {
            self.pause_after_telegram_retry_after(err);
        }
        let response = result?;
        let message_id = response.result.message_id;
        tracing::info!(
            chat_id,
            message_thread_id = ?normalize_outbound_thread_id(message_thread_id),
            message_id,
            "telegram sendMessage delivered"
        );
        Ok(())
    }

    pub async fn send_html_to_thread(
        &self,
        chat_id: i64,
        message_thread_id: Option<i32>,
        html: String,
        plain_fallback: String,
    ) -> anyhow::Result<()> {
        let params = build_send_html_params(chat_id, message_thread_id, html);
        let parse_mode_was_html = matches!(params.parse_mode, Some(frankenstein::ParseMode::Html));
        self.wait_if_paused().await;
        let result = self.inner.send_message(&params).await;
        if let Err(err) = &result {
            self.pause_after_telegram_retry_after(err);
            if parse_mode_was_html && is_telegram_400_error(err) {
                let fallback_params =
                    build_send_text_params(chat_id, message_thread_id, plain_fallback);
                let fallback_result = self.inner.send_message(&fallback_params).await;
                if let Err(fallback_err) = &fallback_result {
                    self.pause_after_telegram_retry_after(fallback_err);
                }
                let response = fallback_result?;
                let error_description = match err {
                    frankenstein::Error::Api(response) => response.description.as_str(),
                    _ => "",
                };
                tracing::warn!(
                    error_description = %error_description,
                    "telegram HTML send failed; retried with plain text"
                );
                let message_id = response.result.message_id;
                tracing::info!(
                    chat_id,
                    message_thread_id = ?normalize_outbound_thread_id(message_thread_id),
                    message_id,
                    "telegram sendMessage delivered"
                );
                return Ok(());
            }
        }
        let response = result?;
        let message_id = response.result.message_id;
        tracing::info!(
            chat_id,
            message_thread_id = ?normalize_outbound_thread_id(message_thread_id),
            message_id,
            "telegram sendMessage delivered"
        );
        Ok(())
    }

    pub async fn answer_callback(&self, query_id: String, text: String) -> anyhow::Result<()> {
        self.inner
            .answer_callback_query(
                &frankenstein::methods::AnswerCallbackQueryParams::builder()
                    .callback_query_id(query_id)
                    .text(text)
                    .build(),
            )
            .await?;
        Ok(())
    }

    pub async fn send_buttons(
        &self,
        chat_id: i64,
        text: String,
        buttons: &[crate::runtime::PromptButton],
    ) -> anyhow::Result<()> {
        self.send_buttons_to_thread(chat_id, None, text, buttons)
            .await
    }

    pub async fn send_buttons_to_thread(
        &self,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: String,
        buttons: &[crate::runtime::PromptButton],
    ) -> anyhow::Result<()> {
        let row = buttons
            .iter()
            .map(|button| {
                frankenstein::types::InlineKeyboardButton::builder()
                    .text(button.label.clone())
                    .callback_data(button.token.clone())
                    .build()
            })
            .collect::<Vec<_>>();
        let markup = frankenstein::types::InlineKeyboardMarkup::builder()
            .inline_keyboard(vec![row])
            .build();

        let builder = frankenstein::methods::SendMessageParams::builder()
            .chat_id(chat_id)
            .text(text)
            .reply_markup(frankenstein::types::ReplyMarkup::InlineKeyboardMarkup(
                markup,
            ));
        let params = if let Some(thread_id) = normalize_outbound_thread_id(message_thread_id) {
            builder.message_thread_id(thread_id).build()
        } else {
            builder.build()
        };
        self.wait_if_paused().await;
        let result = self.inner.send_message(&params).await;
        if let Err(err) = &result {
            self.pause_after_telegram_retry_after(err);
        }
        let response = result?;
        let message_id = response.result.message_id;
        tracing::info!(
            chat_id,
            message_thread_id = ?normalize_outbound_thread_id(message_thread_id),
            message_id,
            "telegram sendMessage delivered"
        );
        Ok(())
    }

    fn pause_after_telegram_retry_after(&self, err: &frankenstein::Error) {
        if let Some(retry_after_secs) = telegram_retry_after_secs(err) {
            self.pause_until_at_least(
                Instant::now()
                    + Duration::from_secs(u64::from(retry_after_secs))
                    + retry_after_jitter(),
            );
        }
    }
}

fn is_telegram_400_error(err: &frankenstein::Error) -> bool {
    matches!(err, frankenstein::Error::Api(response) if response.error_code == 400)
}

fn telegram_retry_after_secs(err: &frankenstein::Error) -> Option<u16> {
    match err {
        frankenstein::Error::Api(response) if response.error_code == 429 => response
            .parameters
            .and_then(|parameters| parameters.retry_after),
        _ => None,
    }
}

fn retry_after_jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or_default();
    Duration::from_millis(100 + u64::from(nanos % 401))
}

pub fn normalize_outbound_thread_id(message_thread_id: Option<i32>) -> Option<i32> {
    match message_thread_id {
        Some(1) | None => None,
        Some(other) => Some(other),
    }
}

pub fn build_send_text_params(
    chat_id: i64,
    message_thread_id: Option<i32>,
    text: String,
) -> frankenstein::methods::SendMessageParams {
    let builder = frankenstein::methods::SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text);
    if let Some(thread_id) = normalize_outbound_thread_id(message_thread_id) {
        builder.message_thread_id(thread_id).build()
    } else {
        builder.build()
    }
}

pub fn build_send_html_params(
    chat_id: i64,
    message_thread_id: Option<i32>,
    html: String,
) -> frankenstein::methods::SendMessageParams {
    let builder = frankenstein::methods::SendMessageParams::builder()
        .chat_id(chat_id)
        .text(html)
        .parse_mode(frankenstein::ParseMode::Html);
    if let Some(thread_id) = normalize_outbound_thread_id(message_thread_id) {
        builder.message_thread_id(thread_id).build()
    } else {
        builder.build()
    }
}

/// Deterministically map a local string draft id to a positive, non-zero
/// `i32` value suitable for the Telegram `sendMessageDraft` API.
pub fn encode_draft_id(local_id: &str) -> i32 {
    let mut folded: u32 = 5381;
    for byte in local_id.bytes() {
        folded = folded.wrapping_mul(33).wrapping_add(byte as u32);
    }
    let bounded = (folded % i32::MAX as u32) + 1;
    bounded as i32
}

#[cfg(test)]
mod tests {
    use super::{encode_draft_id, TelegramClient};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone, Default)]
    struct RecordingSubscriber {
        events: Arc<Mutex<Vec<RecordedEvent>>>,
    }

    #[derive(Debug, Default)]
    struct RecordedEvent {
        fields: HashMap<String, String>,
    }

    #[derive(Default)]
    struct RecordingVisitor {
        fields: HashMap<String, String>,
    }

    impl RecordingSubscriber {
        fn new() -> (Self, Arc<Mutex<Vec<RecordedEvent>>>) {
            let events = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    events: Arc::clone(&events),
                },
                events,
            )
        }
    }

    impl tracing::Subscriber for RecordingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = RecordingVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("recorded events lock poisoned")
                .push(RecordedEvent {
                    fields: visitor.fields,
                });
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    impl tracing::field::Visit for RecordingVisitor {
        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    #[test]
    fn telegram_client_new_accepts_request_timeout() {
        let _client = TelegramClient::new("TEST_TOKEN", Duration::from_millis(250))
            .expect("client with custom timeout should build");
    }

    #[tokio::test]
    async fn hanging_tcp_listener_times_out_via_request_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hanging telegram test listener");
        let addr = listener.local_addr().expect("listener local addr");
        let _server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept request");
            std::future::pending::<()>().await;
            drop(stream);
        });

        let request_timeout = Duration::from_millis(500);
        let client = TelegramClient::new_with_url(format!("http://{addr}/"), request_timeout)
            .expect("client with custom api url should build");

        let started = tokio::time::Instant::now();
        let result = client.get_me().await;
        let elapsed = started.elapsed();

        assert!(result.is_err(), "hanging request should time out");
        assert!(
            elapsed <= request_timeout + Duration::from_secs(1),
            "request took {elapsed:?}, expected at most request timeout plus slack"
        );
        assert!(
            elapsed >= request_timeout.saturating_sub(Duration::from_millis(100)),
            "request ended in {elapsed:?}, before the configured timeout"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_text_to_thread_logs_successful_delivery_message_id() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind telegram test listener");
        let addr = listener.local_addr().expect("listener local addr");
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let _server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_http_request(&mut stream).await;
            let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());

            let body = r#"{"ok":true,"result":{"message_id":321,"message_thread_id":77,"date":0,"chat":{"id":42,"type":"private"},"text":"hello"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write telegram response");
        });

        let client =
            TelegramClient::new_with_url(format!("http://{addr}/"), Duration::from_secs(1))
                .expect("client with custom api url should build");
        let (subscriber, events) = RecordingSubscriber::new();
        let dispatcher = tracing::Dispatch::new(subscriber);
        let _guard = tracing::dispatcher::set_default(&dispatcher);

        client
            .send_text_to_thread(42, Some(77), "hello".into())
            .await
            .expect("send_text_to_thread should return ok");

        let request = request_rx.await.expect("server should capture request");
        assert!(request.contains("\"chat_id\":42"));
        assert!(request.contains("\"message_thread_id\":77"));

        let events = events.lock().expect("recorded events lock poisoned");
        let event = events
            .iter()
            .find(|event| {
                event
                    .fields
                    .values()
                    .any(|value| value.contains("telegram sendMessage delivered"))
            })
            .expect("expected successful sendMessage delivery log");
        assert_eq!(event.fields.get("chat_id").map(String::as_str), Some("42"));
        assert_eq!(
            event.fields.get("message_thread_id").map(String::as_str),
            Some("Some(77)")
        );
        assert_eq!(
            event.fields.get("message_id").map(String::as_str),
            Some("321")
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

    #[test]
    fn draft_id_encoding_is_deterministic() {
        assert_eq!(
            encode_draft_id("working-10001"),
            encode_draft_id("working-10001")
        );
    }

    #[test]
    fn draft_id_encoding_is_non_zero() {
        assert_ne!(encode_draft_id("working-10001"), 0);
        assert_ne!(encode_draft_id(""), 0);
    }

    #[test]
    fn distinct_draft_ids_do_not_collapse() {
        let a = encode_draft_id("draft-a");
        let b = encode_draft_id("draft-b");
        assert_ne!(
            a, b,
            "distinct local draft ids should not map to the same telegram draft id"
        );
    }

    #[test]
    fn draft_id_encoding_fits_positive_i32() {
        let id = encode_draft_id("working-10001");
        assert!(id > 0, "draft id must be positive");
    }

    #[test]
    fn draft_payload_contains_numeric_draft_id() {
        let payload = serde_json::json!({
            "chat_id": 10_001_i64,
            "draft_id": encode_draft_id("working-10001"),
            "text": "hello",
        });
        let draft_id = payload.get("draft_id").and_then(|v| v.as_i64());
        assert!(
            draft_id.is_some(),
            "payload must contain a numeric draft_id"
        );
        assert_ne!(draft_id.unwrap(), 0, "draft_id must be non-zero");
    }
}
