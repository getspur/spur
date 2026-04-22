pub async fn render_batch(
    client: &crate::telegram::client::TelegramClient,
    sender: &crate::telegram::sender::TelegramSender,
    chat_id: i64,
    renders: Vec<crate::runtime::RuntimeRender>,
) -> anyhow::Result<()> {
    render_batch_to_thread(client, sender, chat_id, None, renders).await
}

pub async fn render_batch_to_thread(
    client: &crate::telegram::client::TelegramClient,
    sender: &crate::telegram::sender::TelegramSender,
    chat_id: i64,
    message_thread_id: Option<i32>,
    renders: Vec<crate::runtime::RuntimeRender>,
) -> anyhow::Result<()> {
    for render in renders {
        match render {
            crate::runtime::RuntimeRender::ServiceMessage { text }
            | crate::runtime::RuntimeRender::FinalAnswer { text } => {
                client
                    .send_text_to_thread(chat_id, message_thread_id, text)
                    .await?;
            }
            crate::runtime::RuntimeRender::WorkingStatus { text } => {
                sender
                    .queue_draft(crate::telegram::sender::DraftUpdate {
                        chat_id,
                        message_thread_id,
                        draft_id: format!("working-{chat_id}-{:?}", message_thread_id),
                        text,
                    })
                    .await;
            }
            crate::runtime::RuntimeRender::AnswerCallback { query_id, text } => {
                client.answer_callback(query_id, text).await?;
            }
            crate::runtime::RuntimeRender::ReviewPrompt { text, buttons }
            | crate::runtime::RuntimeRender::PermissionPrompt { text, buttons } => {
                client
                    .send_buttons_to_thread(chat_id, message_thread_id, text, &buttons)
                    .await?;
            }
            crate::runtime::RuntimeRender::FinalizePrompt { .. } => {}
            crate::runtime::RuntimeRender::CreateTopic { .. } => {}
        }
    }
    Ok(())
}
