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
        _draft_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "chat_id": chat_id,
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
