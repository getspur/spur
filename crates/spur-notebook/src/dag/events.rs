use std::{
    collections::VecDeque,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::{
    sync::{broadcast, mpsc, Mutex},
    task::JoinHandle,
};

const DEFAULT_DRAFT_CHANNEL_CAPACITY: usize = 128;
const DEFAULT_BROADCAST_CAPACITY: usize = 128;
const DEFAULT_RING_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRef {
    pub port: String,
    pub version: u64,
    pub class: PortClass,
    pub schema_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortClass {
    Dataframe,
    Media,
    /// Reserved for v1.5. The v1 runtime keeps events ref-only and does not
    /// support inline signal payload behavior.
    Signal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInput {
    pub port: String,
    pub r#ref: Option<PortRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Agent { tool: String },
    Widget { model_id: String, cell_id: String },
    Capture { cell_id: String },
    Kernel { cell_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Succeeded,
    Failed,
    UpstreamFailed,
    Stale,
    SkippedFresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CascadeStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortEventKind {
    PortPut {
        r#ref: PortRef,
        origin: Origin,
    },
    CascadeStarted {
        cascade_id: u64,
        trigger: Origin,
    },
    RunStarted {
        cascade_id: u64,
        run_id: u64,
        cell_id: String,
        inputs: Vec<RunInput>,
    },
    RunFinished {
        cascade_id: u64,
        run_id: u64,
        cell_id: String,
        status: RunStatus,
        outputs: Vec<PortRef>,
    },
    CascadeFinished {
        cascade_id: u64,
        status: CascadeStatus,
    },
    CascadeError {
        cascade_id: u64,
        code: String,
        message: String,
        port: Option<String>,
    },
    IntentRejected {
        origin: Origin,
        code: String,
        port: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortEventDraft {
    pub kind: PortEventKind,
}

impl PortEventDraft {
    pub fn new(kind: PortEventKind) -> Self {
        Self { kind }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortEvent {
    seq: u64,
    at_ms: u64,
    kind: PortEventKind,
}

impl PortEvent {
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }

    pub fn kind(&self) -> &PortEventKind {
        &self.kind
    }

    pub fn into_kind(self) -> PortEventKind {
        self.kind
    }
}

#[derive(Debug, Clone)]
pub struct PortEventSequencerConfig {
    pub draft_channel_capacity: usize,
    pub broadcast_capacity: usize,
    pub ring_capacity: usize,
}

impl Default for PortEventSequencerConfig {
    fn default() -> Self {
        Self {
            draft_channel_capacity: DEFAULT_DRAFT_CHANNEL_CAPACITY,
            broadcast_capacity: DEFAULT_BROADCAST_CAPACITY,
            ring_capacity: DEFAULT_RING_CAPACITY,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PortEventClient {
    draft_tx: mpsc::Sender<PortEventDraft>,
    broadcast_tx: broadcast::Sender<PortEvent>,
    ring: Arc<Mutex<VecDeque<PortEvent>>>,
}

impl PortEventClient {
    pub async fn emit(&self, draft: PortEventDraft) -> Result<(), PortEventError> {
        self.draft_tx
            .send(draft)
            .await
            .map_err(|_| PortEventError::SequencerClosed)
    }

    pub fn draft_sender(&self) -> mpsc::Sender<PortEventDraft> {
        self.draft_tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PortEvent> {
        self.broadcast_tx.subscribe()
    }

    pub async fn recent_events(&self) -> Vec<PortEvent> {
        self.ring.lock().await.iter().cloned().collect()
    }
}

pub struct PortEventSequencer {
    client: PortEventClient,
    task: JoinHandle<()>,
}

impl PortEventSequencer {
    pub fn spawn(config: PortEventSequencerConfig) -> Self {
        let draft_capacity = config.draft_channel_capacity.max(1);
        let broadcast_capacity = config.broadcast_capacity.max(1);
        let ring_capacity = config.ring_capacity.max(1);

        let (draft_tx, draft_rx) = mpsc::channel(draft_capacity);
        let (broadcast_tx, _) = broadcast::channel(broadcast_capacity);
        let ring = Arc::new(Mutex::new(VecDeque::with_capacity(ring_capacity)));
        let task = tokio::spawn(run_sequencer(
            draft_rx,
            broadcast_tx.clone(),
            Arc::clone(&ring),
            ring_capacity,
        ));
        let client = PortEventClient {
            draft_tx,
            broadcast_tx,
            ring,
        };
        Self { client, task }
    }

    pub fn client(&self) -> PortEventClient {
        self.client.clone()
    }

    pub async fn shutdown(self) {
        drop(self.client);
        let _ = self.task.await;
    }
}

async fn run_sequencer(
    mut draft_rx: mpsc::Receiver<PortEventDraft>,
    broadcast_tx: broadcast::Sender<PortEvent>,
    ring: Arc<Mutex<VecDeque<PortEvent>>>,
    ring_capacity: usize,
) {
    let mut next_seq = 1u64;
    while let Some(draft) = draft_rx.recv().await {
        let event = PortEvent {
            seq: next_seq,
            at_ms: now_ms(),
            kind: draft.kind,
        };
        next_seq = next_seq.checked_add(1).expect("port event seq exhausted");

        {
            let mut ring = ring.lock().await;
            if ring.len() == ring_capacity {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }

        let _ = broadcast_tx.send(event);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PortEventError {
    #[error("port event sequencer is closed")]
    SequencerClosed,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[tokio::test]
    async fn sequencer_assigns_strictly_monotonic_seq_for_cloned_senders() {
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig {
            draft_channel_capacity: 16,
            broadcast_capacity: 32,
            ring_capacity: 32,
        });
        let client = sequencer.client();
        let mut subscriber = client.subscribe();

        let sender_a = client.draft_sender();
        let sender_b = client.draft_sender();
        let send_a = tokio::spawn(async move {
            for run_id in 0..10 {
                sender_a
                    .send(run_started(1, run_id))
                    .await
                    .expect("sequencer should accept sender A drafts");
            }
        });
        let send_b = tokio::spawn(async move {
            for run_id in 0..10 {
                sender_b
                    .send(run_started(2, run_id))
                    .await
                    .expect("sequencer should accept sender B drafts");
            }
        });

        send_a.await.expect("sender A task should finish");
        send_b.await.expect("sender B task should finish");

        let mut events = Vec::new();
        while events.len() < 20 {
            events.push(
                subscriber
                    .recv()
                    .await
                    .expect("subscriber should receive sequenced event"),
            );
        }

        for pair in events.windows(2) {
            assert!(pair[0].seq() < pair[1].seq());
        }
        assert_eq!(
            events.iter().map(PortEvent::seq).collect::<Vec<_>>(),
            (1..=20).collect::<Vec<_>>()
        );

        drop(client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn sequencer_preserves_per_sender_fifo_order() {
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig {
            draft_channel_capacity: 16,
            broadcast_capacity: 32,
            ring_capacity: 32,
        });
        let client = sequencer.client();
        let mut subscriber = client.subscribe();

        let sender_a = client.draft_sender();
        let sender_b = client.draft_sender();
        let send_a = tokio::spawn(async move {
            for run_id in 0..8 {
                sender_a
                    .send(run_started(11, run_id))
                    .await
                    .expect("sequencer should accept sender A drafts");
            }
        });
        let send_b = tokio::spawn(async move {
            for run_id in 0..8 {
                sender_b
                    .send(run_started(22, run_id))
                    .await
                    .expect("sequencer should accept sender B drafts");
            }
        });

        send_a.await.expect("sender A task should finish");
        send_b.await.expect("sender B task should finish");

        let mut by_cascade = BTreeMap::<u64, Vec<u64>>::new();
        while by_cascade.values().map(Vec::len).sum::<usize>() < 16 {
            let event = subscriber
                .recv()
                .await
                .expect("subscriber should receive sequenced event");
            let PortEventKind::RunStarted {
                cascade_id, run_id, ..
            } = event.kind()
            else {
                panic!("expected run-started event");
            };
            by_cascade.entry(*cascade_id).or_default().push(*run_id);
        }

        assert_eq!(by_cascade.get(&11), Some(&(0..8).collect()));
        assert_eq!(by_cascade.get(&22), Some(&(0..8).collect()));

        drop(client);
        sequencer.shutdown().await;
    }

    #[tokio::test]
    async fn sequencer_keeps_bounded_recent_ring_for_late_readers() {
        let sequencer = PortEventSequencer::spawn(PortEventSequencerConfig {
            draft_channel_capacity: 4,
            broadcast_capacity: 8,
            ring_capacity: 3,
        });
        let client = sequencer.client();
        let mut subscriber = client.subscribe();

        for cascade_id in 1..=5 {
            client
                .emit(PortEventDraft::new(PortEventKind::CascadeStarted {
                    cascade_id,
                    trigger: Origin::Agent {
                        tool: "notebook_push_source".to_owned(),
                    },
                }))
                .await
                .expect("sequencer should accept draft");
        }
        while subscriber
            .recv()
            .await
            .expect("subscriber should receive sequenced event")
            .seq()
            < 5
        {}

        let recent = client.recent_events().await;
        assert_eq!(recent.len(), 3);
        assert_eq!(
            recent.iter().map(PortEvent::seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );

        drop(client);
        sequencer.shutdown().await;
    }

    fn run_started(cascade_id: u64, run_id: u64) -> PortEventDraft {
        PortEventDraft::new(PortEventKind::RunStarted {
            cascade_id,
            run_id,
            cell_id: format!("cell-{cascade_id}-{run_id}"),
            inputs: Vec::new(),
        })
    }
}
