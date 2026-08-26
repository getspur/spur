use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _};
use serde_json::Value;
use tokio::sync::{mpsc, Notify};

use super::overlay_runtime::{RuntimeChangeStream, RuntimeSubscriptionFactory};
use super::{
    exact_overlay_observations, opaque_published_generation_id,
    open_code_search_backend_for_request, overlay_runtime_lifecycle_for, provider_diagnostic,
    reset_exact_overlay_observations, trust_diagnostic, GraphMcpDeps, GraphMcpModule,
    McpOverlayGenerationBuilder, OverlayGenerationBuilder, OverlayRuntimeKey,
    OverlayRuntimeLifecycle, PublishedState, PublishedTrust, RebuildCoordinator,
    SCOPED_CODE_GRAPH_WORKTREE_ROOT,
};
use crate::overlay_watch::{ChangeBatch, ChangeProviderKind, CompositeCursor, TrustLoss};

static EXACT_OBSERVATION_SCOPE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// A one-path provider event accepted by the deterministic runtime support seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayFileChange {
    Add(PathBuf),
    Modify(PathBuf),
    Delete(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
}

/// Provider continuity failures that the deterministic support seam can inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayProviderLoss {
    Disconnected,
    Overflow,
    FreshInstance,
}

/// Public, private-type-free view of one runtime publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRuntimeSnapshot {
    pub provider: String,
    pub trust: String,
    pub trust_reason: Option<String>,
    pub epoch: u64,
    pub generation_id: String,
    pub indexed_graph_content_hash: String,
    pub indexed_head_oid: Option<String>,
    pub current_head_oid: String,
    pub index_identity: String,
    pub arm_count: usize,
}

/// Elapsed time and resulting state for one injected event or recovery release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPublication {
    pub elapsed: Duration,
    pub state: OverlayRuntimeSnapshot,
}

/// Bounded generation-route fields extracted from one real `GraphMcpModule` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRequestDiagnostics {
    pub route: String,
    pub provider: Option<String>,
    pub trust: String,
    pub epoch: Option<u64>,
    pub generation_id: Option<String>,
    pub generation_pins: u64,
    pub query_operations: Option<u64>,
    pub validation_observations: u64,
    pub finalization_stages: u64,
    pub fallback_reason: Option<String>,
}

/// Response, duration, and generation pin/route diagnostics for one code request.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayRequestSample {
    pub elapsed: Duration,
    pub response: Value,
    pub diagnostics: OverlayRequestDiagnostics,
}

/// Exclusive reset/delta observation scope for exact overlay observations.
///
/// Construction resets the feature-gated counter. Drop resets it again and
/// releases the process-local lease so measurements cannot bleed between tests.
#[derive(Debug)]
pub struct ExactObservationScope {
    active: bool,
}

impl ExactObservationScope {
    fn begin() -> anyhow::Result<Self> {
        EXACT_OBSERVATION_SCOPE_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| anyhow!("an exact-overlay observation scope is already active"))?;
        reset_exact_overlay_observations();
        Ok(Self { active: true })
    }

    pub fn delta(&self) -> usize {
        exact_overlay_observations()
    }
}

impl Drop for ExactObservationScope {
    fn drop(&mut self) {
        if self.active {
            reset_exact_overlay_observations();
            EXACT_OBSERVATION_SCOPE_ACTIVE.store(false, Ordering::SeqCst);
            self.active = false;
        }
    }
}

/// Opaque adapter around the real overlay lifecycle actor and MCP code route.
pub struct OverlayRuntimeSupport {
    root: PathBuf,
    module: GraphMcpModule,
    lifecycle: Arc<OverlayRuntimeLifecycle>,
    key: OverlayRuntimeKey,
    subscriptions: Arc<SupportSubscriptionFactory>,
    paused_state: Mutex<Option<OverlayRuntimeSnapshot>>,
    _rebuild_coordinator: Arc<RebuildCoordinator>,
}

impl OverlayRuntimeSupport {
    pub async fn start(root: &Path) -> anyhow::Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
        let rebuild_coordinator = Arc::new(RebuildCoordinator::new());
        let module = GraphMcpModule::new(GraphMcpDeps {
            rebuild_coordinator: Arc::clone(&rebuild_coordinator),
            overlay_fsmonitor_auto: true,
        });
        let lifecycle = overlay_runtime_lifecycle_for(&rebuild_coordinator);
        let subscriptions = Arc::new(SupportSubscriptionFactory::new(root.clone()));
        let key = SCOPED_CODE_GRAPH_WORKTREE_ROOT
            .scope(root.clone(), async {
                let backend =
                    open_code_search_backend_for_request(Some(Arc::clone(&rebuild_coordinator)))
                        .await
                        .context("failed to open support graph backend")?;
                let snapshot_base = backend
                    .snapshot_base()
                    .context("failed to read support snapshot base")?;
                let key = OverlayRuntimeKey::new(
                    root.clone(),
                    snapshot_base.indexed_graph_content_hash.clone(),
                );
                let builder: Arc<dyn OverlayGenerationBuilder> =
                    Arc::new(McpOverlayGenerationBuilder {
                        worktree: root.clone(),
                        snapshot_base,
                        full_base_source: backend.full_base_artifact_source(),
                        #[cfg(test)]
                        use_request_cache: false,
                    });
                if !lifecycle.activate_if_current(&key, || true) {
                    return Err(anyhow!("support runtime base was superseded before start"));
                }
                let runtime_subscriptions: Arc<dyn RuntimeSubscriptionFactory> =
                    subscriptions.clone();
                let handle = lifecycle
                    .registry
                    .get_or_start(key.clone(), Arc::clone(&runtime_subscriptions), builder)
                    .await
                    .context("failed to start support overlay runtime")?;
                lifecycle.install_if_active(&key, handle, runtime_subscriptions);
                Ok::<_, anyhow::Error>(key)
            })
            .await?;

        Ok(Self {
            root,
            module,
            lifecycle,
            key,
            subscriptions,
            paused_state: Mutex::new(None),
            _rebuild_coordinator: rebuild_coordinator,
        })
    }

    pub fn state(&self) -> Option<OverlayRuntimeSnapshot> {
        self.lifecycle
            .acquire(&self.key)
            .map(|acquired| self.snapshot(&acquired.published))
    }

    pub fn observe_exact(&self) -> anyhow::Result<ExactObservationScope> {
        ExactObservationScope::begin()
    }

    pub async fn request(&self, name: &str, args: Value) -> anyhow::Result<OverlayRequestSample> {
        let started = Instant::now();
        let response = SCOPED_CODE_GRAPH_WORKTREE_ROOT
            .scope(self.root.clone(), self.module.dispatch(name, args))
            .await
            .map_err(|error| anyhow!("GraphMcpModule `{name}` request failed: {error:?}"))?;
        let elapsed = started.elapsed();
        let diagnostics = OverlayRequestDiagnostics::from_response(&response)?;
        Ok(OverlayRequestSample {
            elapsed,
            response,
            diagnostics,
        })
    }

    pub async fn publish_file_change(
        &self,
        change: OverlayFileChange,
        timeout: Duration,
    ) -> anyhow::Result<OverlayPublication> {
        let previous = self
            .state()
            .ok_or_else(|| anyhow!("support runtime is not installed"))?;
        let cursor = self.subscriptions.next_cursor();
        let (added, modified, deleted, renamed) = match change {
            OverlayFileChange::Add(path) => (
                BTreeSet::from([path]),
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
            ),
            OverlayFileChange::Modify(path) => (
                BTreeSet::new(),
                BTreeSet::from([path]),
                BTreeSet::new(),
                BTreeSet::new(),
            ),
            OverlayFileChange::Delete(path) => (
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::from([path]),
                BTreeSet::new(),
            ),
            OverlayFileChange::Rename { from, to } => (
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::from([(from, to)]),
            ),
        };
        let started = Instant::now();
        self.subscriptions.send(ChangeBatch::Changes {
            cursor,
            added,
            modified,
            deleted,
            renamed,
            git_metadata: BTreeSet::new(),
        })?;
        let state = self
            .wait_for(timeout, |state| {
                state.epoch > previous.epoch && state.trust == "trusted"
            })
            .await?;
        Ok(OverlayPublication {
            elapsed: started.elapsed(),
            state,
        })
    }

    pub async fn pause_recovery(
        &self,
        loss: OverlayProviderLoss,
        timeout: Duration,
    ) -> anyhow::Result<OverlayPublication> {
        let previous = self
            .state()
            .ok_or_else(|| anyhow!("support runtime is not installed"))?;
        let previous_blocked_arms = self.subscriptions.blocked_arm_count();
        self.subscriptions.pause_recovery()?;
        let reason = match loss {
            OverlayProviderLoss::Disconnected => TrustLoss::Disconnected {
                provider: ChangeProviderKind::Notify,
            },
            OverlayProviderLoss::Overflow => TrustLoss::Overflow {
                provider: ChangeProviderKind::Notify,
            },
            OverlayProviderLoss::FreshInstance => TrustLoss::FreshInstance,
        };
        let started = Instant::now();
        if let Err(error) = self.subscriptions.send(ChangeBatch::TrustLost {
            cursor: self.subscriptions.next_cursor(),
            reason,
        }) {
            self.subscriptions.resume_recovery();
            return Err(error);
        }
        let waited = self
            .wait_for(timeout, |state| {
                state.epoch > previous.epoch
                    && state.trust == "untrusted"
                    && self.subscriptions.blocked_arm_count() > previous_blocked_arms
            })
            .await;
        match waited {
            Ok(state) => {
                *self
                    .paused_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(state.clone());
                Ok(OverlayPublication {
                    elapsed: started.elapsed(),
                    state,
                })
            }
            Err(error) => {
                self.subscriptions.resume_recovery();
                Err(error)
            }
        }
    }

    pub async fn resume_recovery(&self, timeout: Duration) -> anyhow::Result<OverlayPublication> {
        let previous = self
            .paused_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| anyhow!("support runtime recovery is not paused"))?;
        let started = Instant::now();
        self.subscriptions.resume_recovery();
        let state = self
            .wait_for(timeout, |state| {
                state.trust == "trusted"
                    && (state.epoch > previous.epoch
                        || state.generation_id != previous.generation_id
                        || state.arm_count > previous.arm_count)
            })
            .await?;
        self.paused_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        Ok(OverlayPublication {
            elapsed: started.elapsed(),
            state,
        })
    }

    async fn wait_for(
        &self,
        timeout: Duration,
        predicate: impl Fn(&OverlayRuntimeSnapshot) -> bool,
    ) -> anyhow::Result<OverlayRuntimeSnapshot> {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(state) = self.state() {
                    if predicate(&state) {
                        return state;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for overlay runtime publication"))
    }

    fn snapshot(&self, published: &PublishedState) -> OverlayRuntimeSnapshot {
        let identity = published.snapshot_identity();
        let (trust, trust_reason) = match published.trust() {
            PublishedTrust::Untrusted(reason) => (
                trust_diagnostic(published.trust()),
                Some(format!("{reason:?}")),
            ),
            _ => (trust_diagnostic(published.trust()), None),
        };
        OverlayRuntimeSnapshot {
            provider: provider_diagnostic(published.provider()).to_owned(),
            trust: trust.to_owned(),
            trust_reason,
            epoch: published.epoch(),
            generation_id: opaque_published_generation_id(identity),
            indexed_graph_content_hash: identity.indexed_graph_content_hash.clone(),
            indexed_head_oid: identity.indexed_head_oid.clone(),
            current_head_oid: identity.current_head_oid.clone(),
            index_identity: identity.index_identity.clone(),
            arm_count: self.subscriptions.arm_count(),
        }
    }
}

impl OverlayRequestDiagnostics {
    fn from_response(response: &Value) -> anyhow::Result<Self> {
        let diagnostics = response
            .get("overlay_generation")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("response omitted overlay_generation diagnostics"))?;
        let string = |field: &str| {
            diagnostics
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Ok(Self {
            route: string("route").ok_or_else(|| anyhow!("diagnostics omitted route"))?,
            provider: string("provider"),
            trust: string("trust").ok_or_else(|| anyhow!("diagnostics omitted trust"))?,
            epoch: diagnostics.get("epoch").and_then(Value::as_u64),
            generation_id: string("generation_id"),
            generation_pins: diagnostics
                .get("generation_pins")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            query_operations: diagnostics.get("query_operations").and_then(Value::as_u64),
            validation_observations: diagnostics
                .get("validation_observations")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            finalization_stages: diagnostics
                .get("finalization_stages")
                .and_then(|stages| stages.get("total"))
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            fallback_reason: string("fallback_reason"),
        })
    }
}

struct SupportSubscriptionFactory {
    root: PathBuf,
    arms: AtomicUsize,
    blocked_arms: AtomicUsize,
    cursor_sequence: AtomicU64,
    recovery_paused: AtomicBool,
    resume: Notify,
    senders: Mutex<Vec<mpsc::UnboundedSender<ChangeBatch>>>,
}

impl SupportSubscriptionFactory {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            arms: AtomicUsize::new(0),
            blocked_arms: AtomicUsize::new(0),
            cursor_sequence: AtomicU64::new(0),
            recovery_paused: AtomicBool::new(false),
            resume: Notify::new(),
            senders: Mutex::new(Vec::new()),
        }
    }

    fn arm_count(&self) -> usize {
        self.arms.load(Ordering::SeqCst)
    }

    fn blocked_arm_count(&self) -> usize {
        self.blocked_arms.load(Ordering::SeqCst)
    }

    fn next_cursor(&self) -> CompositeCursor {
        let sequence = self.cursor_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        CompositeCursor::from_entries([(self.root.clone(), format!("support-{sequence}"))])
    }

    fn pause_recovery(&self) -> anyhow::Result<()> {
        self.recovery_paused
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| ())
            .map_err(|_| anyhow!("provider recovery is already paused"))
    }

    fn resume_recovery(&self) {
        self.recovery_paused.store(false, Ordering::SeqCst);
        self.resume.notify_waiters();
    }

    fn send(&self, batch: ChangeBatch) -> anyhow::Result<()> {
        let senders = self
            .senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for sender in senders.iter().rev() {
            if sender.send(batch.clone()).is_ok() {
                return Ok(());
            }
        }
        Err(anyhow!("support provider has no live armed stream"))
    }

    async fn wait_until_resumed(&self) {
        if !self.recovery_paused.load(Ordering::SeqCst) {
            return;
        }
        self.blocked_arms.fetch_add(1, Ordering::SeqCst);
        while self.recovery_paused.load(Ordering::SeqCst) {
            let resumed = self.resume.notified();
            if !self.recovery_paused.load(Ordering::SeqCst) {
                break;
            }
            resumed.await;
        }
    }
}

#[async_trait::async_trait]
impl RuntimeSubscriptionFactory for SupportSubscriptionFactory {
    async fn arm(&self, _key: &OverlayRuntimeKey) -> anyhow::Result<Box<dyn RuntimeChangeStream>> {
        self.arms.fetch_add(1, Ordering::SeqCst);
        self.wait_until_resumed().await;
        let (sender, receiver) = mpsc::unbounded_channel();
        self.senders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(sender);
        Ok(Box::new(SupportChangeStream {
            cursor: self.next_cursor(),
            receiver,
        }))
    }
}

struct SupportChangeStream {
    cursor: CompositeCursor,
    receiver: mpsc::UnboundedReceiver<ChangeBatch>,
}

#[async_trait::async_trait]
impl RuntimeChangeStream for SupportChangeStream {
    fn provider(&self) -> ChangeProviderKind {
        ChangeProviderKind::Notify
    }

    fn initial_cursor(&self) -> &CompositeCursor {
        &self.cursor
    }

    async fn next_batch(&mut self) -> Option<ChangeBatch> {
        self.receiver.recv().await
    }
}
