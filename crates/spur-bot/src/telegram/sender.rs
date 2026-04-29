use std::collections::HashMap;
use tokio::sync::mpsc;

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
        tokio::spawn(async move {
            Self::run_draft_loop(rx, std::time::Duration::from_millis(400), move |update| {
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
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        let (out_tx, out_rx) = mpsc::channel(rate_per_second as usize);
        tokio::spawn(async move {
            Self::run_draft_loop(rx, std::time::Duration::from_millis(400), move |update| {
                let _ = out_tx.try_send(update);
            })
            .await;
        });
        (Self { tx }, out_rx)
    }

    pub async fn queue_draft(&self, update: DraftUpdate) {
        let _ = self.tx.send(update).await;
    }

    pub async fn run_draft_loop(
        mut rx: mpsc::Receiver<DraftUpdate>,
        flush_every: std::time::Duration,
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
