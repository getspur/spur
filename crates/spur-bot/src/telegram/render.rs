pub async fn render_batch(
    client: &crate::telegram::client::TelegramClient,
    sender: &crate::telegram::sender::TelegramSender,
    chat_id: i64,
    renders: Vec<crate::runtime::RuntimeRender>,
) -> anyhow::Result<()> {
    for render in renders {
        match render {
            crate::runtime::RuntimeRender::ServiceMessage { text }
            | crate::runtime::RuntimeRender::FinalAnswer { text } => {
                client.send_text(chat_id, text).await?;
            }
            crate::runtime::RuntimeRender::WorkingStatus { text } => {
                sender
                    .queue_draft(crate::telegram::sender::DraftUpdate {
                        chat_id,
                        draft_id: format!("working-{chat_id}"),
                        text,
                    })
                    .await;
            }
            crate::runtime::RuntimeRender::AnswerCallback { query_id, text } => {
                client.answer_callback(query_id, text).await?;
            }
            crate::runtime::RuntimeRender::ReviewPrompt { text, buttons }
            | crate::runtime::RuntimeRender::PermissionPrompt { text, buttons } => {
                client.send_buttons(chat_id, text, &buttons).await?;
            }
            crate::runtime::RuntimeRender::FinalizePrompt { .. } => {}
        }
    }
    Ok(())
}
