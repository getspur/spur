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
    use crate::telegram::format::{
        markdown_to_telegram_chunks, render_truncated_text, truncate_button_label_bytes,
        TELEGRAM_BUTTON_LABEL_MAX_BYTES,
    };

    for render in renders {
        match render {
            crate::runtime::RuntimeRender::ServiceMessage { text } => {
                for chunk in markdown_to_telegram_chunks(&text) {
                    client
                        .send_html_to_thread(chat_id, message_thread_id, chunk.html, chunk.plain)
                        .await?;
                }
            }
            crate::runtime::RuntimeRender::FinalAnswer { text } => {
                for chunk in markdown_to_telegram_chunks(&text) {
                    client
                        .send_html_to_thread(chat_id, message_thread_id, chunk.html, chunk.plain)
                        .await?;
                }
            }
            crate::runtime::RuntimeRender::WorkingStatus { text } => {
                sender
                    .queue_draft(crate::telegram::sender::DraftUpdate {
                        chat_id,
                        message_thread_id,
                        draft_id: format!("working-{chat_id}-{:?}", message_thread_id),
                        text: render_truncated_text(&text),
                    })
                    .await;
            }
            crate::runtime::RuntimeRender::StreamChunk { draft_id, text } => {
                sender
                    .queue_draft(crate::telegram::sender::DraftUpdate {
                        chat_id,
                        message_thread_id,
                        draft_id,
                        text: render_truncated_text(&text),
                    })
                    .await;
            }
            crate::runtime::RuntimeRender::AnswerCallback { query_id, text } => {
                client.answer_callback(query_id, text).await?;
            }
            crate::runtime::RuntimeRender::ReviewPrompt { text, buttons }
            | crate::runtime::RuntimeRender::PermissionPrompt { text, buttons } => {
                let buttons: Vec<crate::runtime::PromptButton> = buttons
                    .into_iter()
                    .map(|b| crate::runtime::PromptButton {
                        token: b.token,
                        label: truncate_button_label_bytes(
                            &b.label,
                            TELEGRAM_BUTTON_LABEL_MAX_BYTES,
                        ),
                    })
                    .collect();
                let body = render_truncated_text(&text);
                client
                    .send_buttons_to_thread(chat_id, message_thread_id, body, &buttons)
                    .await?;
            }
            crate::runtime::RuntimeRender::FinalizePrompt { .. } => {}
            crate::runtime::RuntimeRender::CreateTopic { .. } => {}
        }
    }
    Ok(())
}
