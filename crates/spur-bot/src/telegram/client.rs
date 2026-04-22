#[derive(Debug, Clone)]
pub struct TelegramClient;

impl TelegramClient {
    pub async fn send_message_draft(
        &self,
        _chat_id: i64,
        _draft_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
