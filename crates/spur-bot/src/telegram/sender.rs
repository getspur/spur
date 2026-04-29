use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, time::Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftUpdate {
    pub chat_id: i64,
    pub message_thread_id: Option<i32>,
    pub draft_id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct DraftPauseState {
    paused_until: Arc<RwLock<Option<Instant>>>,
}

impl DraftPauseState {
    pub fn new() -> Self {
        Self {
            paused_until: Arc::new(RwLock::new(None)),
        }
    }

    pub fn paused_until(&self) -> Option<Instant> {
        *self
            .paused_until
            .read()
            .expect("telegram draft pause lock poisoned")
    }

    pub fn pause_until_at_least(&self, candidate: Instant) {
        let mut paused_until = self
            .paused_until
            .write()
            .expect("telegram draft pause lock poisoned");
        if paused_until.is_none_or(|current| candidate > current) {
            *paused_until = Some(candidate);
        }
    }

    pub fn pause_for_retry_after(&self, now: Instant, retry_after_secs: u16, jitter: Duration) {
        self.pause_until_at_least(now + Duration::from_secs(u64::from(retry_after_secs)) + jitter);
    }

    fn is_paused(&self, now: Instant) -> bool {
        self.paused_until()
            .is_some_and(|paused_until| now < paused_until)
    }
}

impl Default for DraftPauseState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TelegramSender {
    tx: mpsc::Sender<DraftUpdate>,
    _draft_pause: DraftPauseState,
}

impl TelegramSender {
    pub fn new(client: crate::telegram::client::TelegramClient, rate_per_second: u32) -> Self {
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        let draft_pause = DraftPauseState::new();
        let loop_pause = draft_pause.clone();
        let send_pause = draft_pause.clone();
        tokio::spawn(async move {
            Self::run_draft_loop(rx, Duration::from_millis(400), loop_pause, move |update| {
                let client = client.clone();
                let send_pause = send_pause.clone();
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
                        pause_after_telegram_retry_after(&send_pause, &err);
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
        Self {
            tx,
            _draft_pause: draft_pause,
        }
    }

    pub fn for_test(rate_per_second: u32) -> (Self, mpsc::Receiver<DraftUpdate>) {
        Self::for_test_with_pause_state(rate_per_second, DraftPauseState::new())
    }

    pub fn for_test_with_pause_state(
        rate_per_second: u32,
        draft_pause: DraftPauseState,
    ) -> (Self, mpsc::Receiver<DraftUpdate>) {
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        let (out_tx, out_rx) = mpsc::channel(rate_per_second as usize);
        let loop_pause = draft_pause.clone();
        tokio::spawn(async move {
            Self::run_draft_loop(rx, Duration::from_millis(400), loop_pause, move |update| {
                let _ = out_tx.try_send(update);
            })
            .await;
        });
        (
            Self {
                tx,
                _draft_pause: draft_pause,
            },
            out_rx,
        )
    }

    pub async fn queue_draft(&self, update: DraftUpdate) {
        let _ = self.tx.send(update).await;
    }

    pub async fn run_draft_loop(
        mut rx: mpsc::Receiver<DraftUpdate>,
        flush_every: Duration,
        draft_pause: DraftPauseState,
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
                    if draft_pause.is_paused(Instant::now()) {
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

fn pause_after_telegram_retry_after(pause: &DraftPauseState, err: &anyhow::Error) {
    if let Some(retry_after_secs) = telegram_retry_after_secs(err) {
        pause.pause_for_retry_after(Instant::now(), retry_after_secs, retry_after_jitter());
    }
}

fn telegram_retry_after_secs(err: &anyhow::Error) -> Option<u16> {
    err.chain().find_map(|cause| {
        let telegram_error = cause.downcast_ref::<frankenstein::Error>()?;
        match telegram_error {
            frankenstein::Error::Api(response) if response.error_code == 429 => response
                .parameters
                .and_then(|parameters| parameters.retry_after),
            _ => None,
        }
    })
}

fn retry_after_jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or_default();
    Duration::from_millis(100 + u64::from(nanos % 401))
}
