use crate::telegram::client::TelegramClient;
use std::{collections::HashMap, time::Duration};
use tokio::{sync::mpsc, time::Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftUpdate {
    pub chat_id: i64,
    pub message_thread_id: Option<i32>,
    pub draft_id: String,
    pub text: String,
}

pub struct TelegramSender {
    tx: mpsc::Sender<DraftUpdate>,
}

impl TelegramSender {
    pub fn new(client: crate::telegram::client::TelegramClient, rate_per_second: u32) -> Self {
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        let loop_client = client.clone();
        tokio::spawn(async move {
            Self::run_draft_loop(rx, Duration::from_millis(400), loop_client, move |update| {
                let client = client.clone();
                tokio::spawn(async move {
                    let draft_id = update.draft_id.clone();
                    if let Err(err) = client
                        .send_message_draft_to_thread(
                            update.chat_id,
                            update.message_thread_id,
                            &update.draft_id,
                            &update.text,
                        )
                        .await
                    {
                        tracing::warn!(
                            error = ?err,
                            draft_id = %draft_id,
                            "telegram draft send failed"
                        );
                    }
                });
            })
            .await;
        });
        Self { tx }
    }

    pub fn for_test(rate_per_second: u32) -> (Self, mpsc::Receiver<DraftUpdate>) {
        Self::for_test_with_client(rate_per_second, test_client())
    }

    fn for_test_with_client(
        rate_per_second: u32,
        client: TelegramClient,
    ) -> (Self, mpsc::Receiver<DraftUpdate>) {
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        let (out_tx, out_rx) = mpsc::channel(rate_per_second as usize);
        tokio::spawn(async move {
            Self::run_draft_loop(rx, Duration::from_millis(400), client, move |update| {
                let _ = out_tx.try_send(update);
            })
            .await;
        });
        (Self { tx }, out_rx)
    }

    pub async fn queue_draft(&self, update: DraftUpdate) {
        let _ = self.tx.send(update).await;
    }

    async fn run_draft_loop(
        mut rx: mpsc::Receiver<DraftUpdate>,
        flush_every: Duration,
        client: TelegramClient,
        mut flush: impl FnMut(DraftUpdate) + Send + 'static,
    ) {
        let mut pending: HashMap<String, DraftUpdate> = HashMap::new();
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + flush_every, flush_every);

        loop {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(update) => {
                        let key = format!("{}:{}:{:?}", update.chat_id, update.draft_id, update.message_thread_id);
                        pending.insert(key, update);
                    }
                    None => break,
                },
                _ = ticker.tick() => {
                    if client.is_paused(Instant::now()) {
                        continue;
                    }
                    for (_id, update) in pending.drain() {
                        flush(update);
                    }
                }
            }
        }

        for (_id, update) in pending.drain() {
            flush(update);
        }
    }
}

fn test_client() -> TelegramClient {
    TelegramClient::new_with_url("http://127.0.0.1:1/".to_owned(), Duration::from_secs(1))
        .expect("test telegram client should build")
}

#[cfg(test)]
mod tests {
    use super::{DraftUpdate, TelegramSender};
    use std::time::Duration;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn sender_holds_pending_draft_while_client_pause_active() {
        let client = super::test_client();
        client.pause_until_at_least(Instant::now() + Duration::from_secs(13));
        let (sender, mut rx) = TelegramSender::for_test_with_client(20, client);

        sender
            .queue_draft(DraftUpdate {
                chat_id: 10_001,
                message_thread_id: None,
                draft_id: "draft-429".into(),
                text: "held during pause".into(),
            })
            .await;

        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;

        assert!(
            rx.try_recv().is_err(),
            "draft must not flush while retry_after pause is active"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sender_resumes_after_pause_window_expires() {
        let client = super::test_client();
        client.pause_until_at_least(Instant::now() - Duration::from_millis(1));
        let (sender, mut rx) = TelegramSender::for_test_with_client(20, client);

        sender
            .queue_draft(DraftUpdate {
                chat_id: 10_001,
                message_thread_id: None,
                draft_id: "draft-expired".into(),
                text: "sent after pause".into(),
            })
            .await;

        tokio::time::advance(Duration::from_millis(500)).await;

        let sent = rx.recv().await.unwrap();
        assert_eq!(sent.text, "sent after pause");
    }

    #[tokio::test(start_paused = true)]
    async fn client_pause_extends_monotonically_under_repeated_429s() {
        let client = super::test_client();
        let now = Instant::now();
        client.pause_until_at_least(now + Duration::from_secs(30));
        client.pause_until_at_least(now + Duration::from_secs(13));

        assert!(client.is_paused(now + Duration::from_secs(29)));
        assert!(!client.is_paused(now + Duration::from_secs(31)));
    }

    #[tokio::test(start_paused = true)]
    async fn sender_coalesces_drafts_during_pause_then_sends_latest() {
        let client = super::test_client();
        client.pause_until_at_least(Instant::now() + Duration::from_secs(2));
        let (sender, mut rx) = TelegramSender::for_test_with_client(20, client);

        for text in ["first", "second", "latest"] {
            sender
                .queue_draft(DraftUpdate {
                    chat_id: 10_001,
                    message_thread_id: None,
                    draft_id: "draft-paused".into(),
                    text: text.into(),
                })
                .await;
        }

        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        assert!(
            rx.try_recv().is_err(),
            "draft must remain pending while pause is active"
        );

        tokio::time::advance(Duration::from_secs(2)).await;

        let sent = rx.recv().await.unwrap();
        assert_eq!(sent.text, "latest");
        assert!(
            rx.try_recv().is_err(),
            "only the latest coalesced draft should be flushed"
        );
    }
}
