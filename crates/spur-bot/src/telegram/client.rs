use frankenstein::AsyncTelegramApi;

#[derive(Clone)]
pub struct TelegramClient {
    inner: frankenstein::client_reqwest::Bot,
}

impl TelegramClient {
    pub fn new(token: &str) -> Self {
        Self {
            inner: frankenstein::client_reqwest::Bot::new(token),
        }
    }

    pub async fn delete_webhook(&self) -> anyhow::Result<()> {
        self.inner
            .delete_webhook(
                &frankenstein::methods::DeleteWebhookParams::builder().build(),
            )
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

    pub async fn send_message_draft(
        &self,
        chat_id: i64,
        draft_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "draft_id": encode_draft_id(draft_id),
            "text": text,
        });
        let _: frankenstein::response::MethodResponse<bool> = self
            .inner
            .request("sendMessageDraft", Some(payload))
            .await?;
        Ok(())
    }

    pub async fn send_text(&self, chat_id: i64, text: String) -> anyhow::Result<()> {
        self.inner
            .send_message(
                &frankenstein::methods::SendMessageParams::builder()
                    .chat_id(chat_id)
                    .text(text)
                    .build(),
            )
            .await?;
        Ok(())
    }

    pub async fn answer_callback(
        &self,
        query_id: String,
        text: String,
    ) -> anyhow::Result<()> {
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
        self.inner
            .send_message(
                &frankenstein::methods::SendMessageParams::builder()
                    .chat_id(chat_id)
                    .text(text)
                    .reply_markup(frankenstein::types::ReplyMarkup::InlineKeyboardMarkup(
                        markup,
                    ))
                    .build(),
            )
            .await?;
        Ok(())
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
    use super::encode_draft_id;

    #[test]
    fn draft_id_encoding_is_deterministic() {
        assert_eq!(encode_draft_id("working-10001"), encode_draft_id("working-10001"));
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
        assert_ne!(a, b, "distinct local draft ids should not map to the same telegram draft id");
    }

    #[test]
    fn draft_id_encoding_fits_positive_i32() {
        let id = encode_draft_id("working-10001");
        assert!(id > 0, "draft id must be positive");
        assert!(id <= i32::MAX, "draft id must fit in i32");
    }

    #[test]
    fn draft_payload_contains_numeric_draft_id() {
        let payload = serde_json::json!({
            "chat_id": 10_001_i64,
            "draft_id": encode_draft_id("working-10001"),
            "text": "hello",
        });
        let draft_id = payload.get("draft_id").and_then(|v| v.as_i64());
        assert!(draft_id.is_some(), "payload must contain a numeric draft_id");
        assert_ne!(draft_id.unwrap(), 0, "draft_id must be non-zero");
    }
}
