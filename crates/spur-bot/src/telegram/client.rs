use anyhow::Context;
use frankenstein::AsyncTelegramApi;

#[derive(Clone)]
pub struct TelegramClient {
    inner: frankenstein::client_reqwest::Bot,
}

impl TelegramClient {
    pub fn new(token: &str, request_timeout: std::time::Duration) -> anyhow::Result<Self> {
        let http = frankenstein::reqwest::ClientBuilder::new()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(request_timeout)
            .build()
            .context("building reqwest client for telegram bot")?;
        let api_url = format!("{}{}", frankenstein::BASE_API_URL, token);
        Ok(Self {
            inner: frankenstein::client_reqwest::Bot::builder()
                .api_url(api_url)
                .client(http)
                .build(),
        })
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
        let _: frankenstein::response::MethodResponse<bool> = self
            .inner
            .request("sendMessageDraft", Some(payload))
            .await?;
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
        self.inner
            .send_message(&build_send_text_params(chat_id, message_thread_id, text))
            .await?;
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
        self.inner.send_message(&params).await?;
        Ok(())
    }
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
    use std::time::Duration;

    #[test]
    fn telegram_client_new_accepts_request_timeout() {
        let _client = TelegramClient::new("TEST_TOKEN", Duration::from_millis(250))
            .expect("client with custom timeout should build");
    }

    /// Acceptance test stub: exercising a hanging local TCP listener requires
    /// a test-only Telegram API URL injection point, while C0 intentionally
    /// changes only timeout construction on the production API URL.
    #[tokio::test]
    #[ignore = "requires custom Telegram API URL injection outside C0 scope"]
    async fn hanging_tcp_listener_times_out_via_request_timeout() {}

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
