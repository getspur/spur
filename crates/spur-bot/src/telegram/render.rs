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
                        .send_rich_html_to_thread(
                            chat_id,
                            message_thread_id,
                            chunk.html,
                            chunk.plain,
                        )
                        .await?;
                }
            }
            crate::runtime::RuntimeRender::FinalAnswer { text } => {
                for chunk in markdown_to_telegram_chunks(&text) {
                    client
                        .send_rich_html_to_thread(
                            chat_id,
                            message_thread_id,
                            chunk.html,
                            chunk.plain,
                        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeRender;
    use crate::telegram::client::TelegramClient;
    use crate::telegram::sender::TelegramSender;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn unreachable_client() -> TelegramClient {
        // Port 1 is a privileged port nothing in this test binds to, so any
        // accidental network call in a branch we don't expect to hit fails
        // fast instead of hanging.
        TelegramClient::new_with_url("http://127.0.0.1:1/".to_owned(), Duration::from_secs(1))
            .expect("client with custom api url should build")
    }

    #[tokio::test]
    async fn finalize_and_create_topic_renders_are_no_ops() {
        let client = unreachable_client();
        let (sender, _rx) = TelegramSender::for_test(20);

        let result = render_batch_to_thread(
            &client,
            &sender,
            42,
            None,
            vec![
                RuntimeRender::FinalizePrompt {
                    token: "tok-1".into(),
                    text: "resolved".into(),
                },
                RuntimeRender::CreateTopic {
                    topic_name: "topic".into(),
                },
            ],
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn working_status_queues_draft_without_hitting_network() {
        let client = unreachable_client();
        let (sender, mut rx) = TelegramSender::for_test(20);

        render_batch_to_thread(
            &client,
            &sender,
            10_001,
            Some(7),
            vec![RuntimeRender::WorkingStatus {
                text: "thinking...".into(),
            }],
        )
        .await
        .unwrap();

        tokio::time::advance(Duration::from_millis(500)).await;

        let queued = rx.recv().await.unwrap();
        assert_eq!(queued.chat_id, 10_001);
        assert_eq!(queued.message_thread_id, Some(7));
        assert_eq!(queued.text, "thinking...");
    }

    #[tokio::test]
    async fn service_message_sends_rich_html_over_the_wire() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind telegram test listener");
        let addr = listener.local_addr().expect("listener local addr");
        let _server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let _request = read_http_request(&mut stream).await;

            let body = r#"{"ok":true,"result":{"message_id":1,"date":0,"chat":{"id":42,"type":"private"},"text":"hi"}}"#;
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
        let (sender, _rx) = TelegramSender::for_test(20);

        let result = render_batch_to_thread(
            &client,
            &sender,
            42,
            None,
            vec![RuntimeRender::ServiceMessage { text: "hi".into() }],
        )
        .await;

        assert!(result.is_ok());
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
}
