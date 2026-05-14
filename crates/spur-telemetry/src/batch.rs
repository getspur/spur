use crate::client::PosthogEvent;
use crate::ratelimit::TokenBucket;
use crate::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;

const CHANNEL_CAPACITY: usize = 200;
const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
const FLUSH_SIZE: usize = 50;
const SHUTDOWN_TIMEOUT_DEFAULT: Duration = Duration::from_millis(250);
const DROP_WARN_WINDOW: Duration = Duration::from_secs(10);
const SAMPLE_AFTER_PER_MINUTE: u32 = 100;
const SAMPLE_EVERY_N_OVERFLOW: u64 = 10;

type SendBatchFn = Arc<
    dyn Fn(Vec<PosthogEvent>) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync,
>;

struct SourceSampler {
    bucket: Arc<Mutex<TokenBucket>>,
    overflow: u64,
}

#[derive(Default)]
struct SamplingState {
    sources: HashMap<String, SourceSampler>,
}

impl SamplingState {
    fn should_keep(&mut self, event: &PosthogEvent) -> bool {
        if event.event != "mcp_request_duration" && event.event != "acp_request_duration" {
            return true;
        }

        let source = extract_source(&event.properties);
        let key = format!("{}:{}", event.event, source);
        let sampler = self.sources.entry(key).or_insert_with(|| SourceSampler {
            bucket: Arc::new(Mutex::new(TokenBucket::new(
                SAMPLE_AFTER_PER_MINUTE,
                f64::from(SAMPLE_AFTER_PER_MINUTE) / 60.0,
            ))),
            overflow: 0,
        });

        let mut bucket = sampler
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bucket.try_acquire() {
            return true;
        }

        sampler.overflow = sampler.overflow.wrapping_add(1);
        sampler.overflow % SAMPLE_EVERY_N_OVERFLOW == 0
    }
}

fn extract_source(props: &Value) -> &str {
    props
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

pub(crate) struct BatchSender {
    tx: Mutex<Option<mpsc::Sender<PosthogEvent>>>,
    dropped: Arc<AtomicU64>,
    sampling: Mutex<SamplingState>,
    last_drop_warn_epoch_secs: AtomicU64,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl BatchSender {
    pub(crate) fn new<F, Fut>(send_batch: F) -> Self
    where
        F: Fn(Vec<PosthogEvent>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let send_batch: SendBatchFn = Arc::new(move |events| Box::pin(send_batch(events)));
        Self::with_capacity(CHANNEL_CAPACITY, send_batch)
    }

    fn with_capacity(capacity: usize, send_batch: SendBatchFn) -> Self {
        let (tx, mut rx) = mpsc::channel::<PosthogEvent>(capacity);
        let task = tokio::spawn(async move {
            let mut pending = Vec::with_capacity(FLUSH_SIZE);
            let mut tick = time::interval(FLUSH_INTERVAL);
            tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    maybe_event = rx.recv() => {
                        match maybe_event {
                            Some(event) => {
                                pending.push(event);
                                if pending.len() >= FLUSH_SIZE {
                                    flush_pending(&send_batch, &mut pending).await;
                                }
                            }
                            None => {
                                flush_pending(&send_batch, &mut pending).await;
                                break;
                            }
                        }
                    }
                    _ = tick.tick() => {
                        if !pending.is_empty() {
                            flush_pending(&send_batch, &mut pending).await;
                        }
                    }
                }
            }
        });

        Self {
            tx: Mutex::new(Some(tx)),
            dropped: Arc::new(AtomicU64::new(0)),
            sampling: Mutex::new(SamplingState::default()),
            last_drop_warn_epoch_secs: AtomicU64::new(0),
            task: Mutex::new(Some(task)),
        }
    }

    pub(crate) fn try_send(&self, event: PosthogEvent) {
        let keep = {
            let mut sampling = self
                .sampling
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sampling.should_keep(&event)
        };
        if !keep {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let tx_opt = self
            .tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        if let Some(tx) = tx_opt {
            if tx.try_send(event).is_err() {
                self.record_drop_with_warn();
            }
        } else {
            self.record_drop_with_warn();
        }
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) async fn shutdown(&self, timeout: Option<Duration>) {
        {
            let mut tx = self
                .tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tx.take();
        }

        let maybe_task = {
            self.task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };

        if let Some(mut task) = maybe_task {
            let wait_for = timeout.unwrap_or(SHUTDOWN_TIMEOUT_DEFAULT);
            if time::timeout(wait_for, &mut task).await.is_err() {
                task.abort();
            }
        }
    }

    fn record_drop_with_warn(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);

        let now = epoch_secs();
        let last = self.last_drop_warn_epoch_secs.load(Ordering::Relaxed);
        let window_secs = DROP_WARN_WINDOW.as_secs();
        if now.saturating_sub(last) >= window_secs
            && self
                .last_drop_warn_epoch_secs
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            tracing::warn!("telemetry batch channel full; dropping event");
        }
    }
}

async fn flush_pending(send_batch: &SendBatchFn, pending: &mut Vec<PosthogEvent>) {
    if pending.is_empty() {
        return;
    }

    let to_send = std::mem::take(pending);
    if let Err(err) = (send_batch)(to_send).await {
        tracing::warn!(error = %err, "failed to flush telemetry batch");
    }
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::sleep;

    fn mk_event(name: &str, source: &str) -> PosthogEvent {
        PosthogEvent {
            event: name.to_string(),
            distinct_id: "d1".to_string(),
            properties: json!({ "source": source }),
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn burst_drops_when_channel_saturated() {
        let received = Arc::new(AtomicUsize::new(0));
        let sender = {
            let received = Arc::clone(&received);
            BatchSender::with_capacity(
                50,
                Arc::new(move |events| {
                    let received = Arc::clone(&received);
                    Box::pin(async move {
                        received.fetch_add(events.len(), Ordering::Relaxed);
                        sleep(Duration::from_secs(5)).await;
                        Ok(())
                    })
                }),
            )
        };

        for _ in 0..200 {
            sender.try_send(mk_event("other_event", "cli"));
        }

        sender.shutdown(Some(Duration::from_millis(300))).await;

        let got = received.load(Ordering::Relaxed);
        assert!(
            (45..=55).contains(&got),
            "expected around 50 delivered, got {got}"
        );
        assert!(sender.dropped() > 0, "expected dropped > 0");
    }

    #[tokio::test]
    async fn mcp_sampling_reduces_high_rate_volume() {
        let received = Arc::new(AtomicUsize::new(0));
        let sender = {
            let received = Arc::clone(&received);
            BatchSender::new(move |events| {
                let received = Arc::clone(&received);
                async move {
                    received.fetch_add(events.len(), Ordering::Relaxed);
                    Ok(())
                }
            })
        };

        for _ in 0..200 {
            sender.try_send(mk_event("mcp_request_duration", "mcp"));
        }

        sender.shutdown(Some(Duration::from_secs(1))).await;

        let got = received.load(Ordering::Relaxed);
        assert!(
            (108..=132).contains(&got),
            "expected ~120 (+/-10%), got {got}"
        );
    }
}
