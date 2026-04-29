//! Test-only `LicenseProvider` for exercising cross-crate licensing paths.
//! Enabled via the `test-support` feature.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::provider::{LicenseProvider, RefreshPolicy};
use crate::{LicenseError, LicenseEvent, LicenseEventKind, LicenseState, Result};

/// Scripted fake. Each `push_*_result` enqueues the outcome of the next
/// matching handler call. Unqueued calls reflect the current snapshot and
/// succeed. Call counters (`validate_call_count`, `heartbeat_call_count`)
/// let runtime-level tests assert cadence without peeking at internals.
pub struct FakeProvider {
    state: Mutex<LicenseState>,
    events_tx: broadcast::Sender<LicenseEvent>,
    script: Mutex<Script>,
    refresh_policy: RefreshPolicy,
    requires_heartbeat: bool,
    validate_calls: AtomicUsize,
    heartbeat_calls: AtomicUsize,
    activate_calls: AtomicUsize,
    deactivate_calls: AtomicUsize,
}

/// Outcome shape for a scripted `heartbeat` call.
///
/// Plain `Ok`/`Err` results match the same semantics as the other
/// mutating methods. `DegradedThenErr` mirrors
/// `LicenseSeatProvider::heartbeat`'s degrade-on-failure path: it
/// commits a degraded state to the provider's internal state and
/// broadcasts before returning the error. Used by bd-22q.1's
/// regression test to drive the heartbeat-Err refresh contract.
enum ScriptedHeartbeat {
    Ok(LicenseState),
    Err(LicenseError),
    DegradedThenErr {
        state: LicenseState,
        err: LicenseError,
    },
}

#[derive(Default)]
struct Script {
    validate: VecDeque<Result<LicenseState>>,
    heartbeat: VecDeque<ScriptedHeartbeat>,
    activate: VecDeque<Result<LicenseState>>,
    deactivate: VecDeque<Result<LicenseState>>,
}

impl FakeProvider {
    pub fn new(initial: LicenseState) -> Self {
        let (events_tx, _) = broadcast::channel(64);
        Self {
            state: Mutex::new(initial),
            events_tx,
            script: Mutex::new(Script::default()),
            refresh_policy: RefreshPolicy::default(),
            requires_heartbeat: false,
            validate_calls: AtomicUsize::new(0),
            heartbeat_calls: AtomicUsize::new(0),
            activate_calls: AtomicUsize::new(0),
            deactivate_calls: AtomicUsize::new(0),
        }
    }

    pub fn with_refresh_policy(mut self, policy: RefreshPolicy) -> Self {
        self.refresh_policy = policy;
        self
    }

    pub fn with_requires_heartbeat(mut self, needs: bool) -> Self {
        self.requires_heartbeat = needs;
        self
    }

    pub fn push_validate_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().validate.push_back(r);
    }

    pub fn push_heartbeat_result(&self, r: Result<LicenseState>) {
        let entry = match r {
            Ok(s) => ScriptedHeartbeat::Ok(s),
            Err(e) => ScriptedHeartbeat::Err(e),
        };
        self.script.lock().unwrap().heartbeat.push_back(entry);
    }

    /// Enqueue a scripted heartbeat outcome that commits `state` to
    /// the provider's internal state (and broadcasts a HeartbeatFailed
    /// event) before returning `Err(err)`. Mirrors
    /// `LicenseSeatProvider::heartbeat` degrade-on-failure (`degrade_current`
    /// + `replace_state` + return Err). Required by bd-22q.1's
    /// regression test for the heartbeat-Err refresh contract.
    pub fn push_heartbeat_degraded_err(&self, state: LicenseState, err: LicenseError) {
        self.script
            .lock()
            .unwrap()
            .heartbeat
            .push_back(ScriptedHeartbeat::DegradedThenErr { state, err });
    }

    pub fn push_activate_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().activate.push_back(r);
    }

    pub fn push_deactivate_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().deactivate.push_back(r);
    }

    /// Inject a raw event into the subscribe channel without mutating state.
    /// Models autonomous SDK subscription updates.
    pub fn inject_event(&self, kind: LicenseEventKind, state: LicenseState) {
        let _ = self.events_tx.send(LicenseEvent {
            kind,
            state,
            message: None,
        });
    }

    pub fn validate_call_count(&self) -> usize {
        self.validate_calls.load(Ordering::Relaxed)
    }

    pub fn heartbeat_call_count(&self) -> usize {
        self.heartbeat_calls.load(Ordering::Relaxed)
    }

    pub fn activate_call_count(&self) -> usize {
        self.activate_calls.load(Ordering::Relaxed)
    }

    pub fn deactivate_call_count(&self) -> usize {
        self.deactivate_calls.load(Ordering::Relaxed)
    }

    fn snapshot(&self) -> LicenseState {
        self.state.lock().unwrap().clone()
    }

    fn commit(&self, next: LicenseState, kind: LicenseEventKind) -> LicenseState {
        *self.state.lock().unwrap() = next.clone();
        let _ = self.events_tx.send(LicenseEvent {
            kind,
            state: next.clone(),
            message: None,
        });
        next
    }
}

#[async_trait]
impl LicenseProvider for FakeProvider {
    fn current_state(&self) -> LicenseState {
        self.snapshot()
    }

    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        self.events_tx.subscribe()
    }

    fn refresh_policy(&self) -> RefreshPolicy {
        self.refresh_policy
    }

    fn has_entitlement(&self, feature: &str) -> bool {
        self.snapshot().features.contains(feature)
    }

    fn requires_heartbeat(&self) -> bool {
        self.requires_heartbeat
    }

    async fn activate(&self, _key: &str) -> Result<LicenseState> {
        self.activate_calls.fetch_add(1, Ordering::Relaxed);
        let scripted = self.script.lock().unwrap().activate.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::Activated)),
            Some(Err(e)) => Err(e),
            None => Err(LicenseError::Provider("no scripted activate".into())),
        }
    }

    async fn validate(&self) -> Result<LicenseState> {
        self.validate_calls.fetch_add(1, Ordering::Relaxed);
        let scripted = self.script.lock().unwrap().validate.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::Validated)),
            Some(Err(e)) => Err(e),
            None => Ok(self.snapshot()),
        }
    }

    async fn heartbeat(&self) -> Result<LicenseState> {
        self.heartbeat_calls.fetch_add(1, Ordering::Relaxed);
        let scripted = self.script.lock().unwrap().heartbeat.pop_front();
        match scripted {
            Some(ScriptedHeartbeat::Ok(next)) => {
                Ok(self.commit(next, LicenseEventKind::HeartbeatOk))
            }
            Some(ScriptedHeartbeat::Err(e)) => Err(e),
            Some(ScriptedHeartbeat::DegradedThenErr { state, err }) => {
                // Commit the degraded state (mutating internal state and
                // broadcasting), then propagate the error. Mirrors
                // LicenseSeatProvider::heartbeat's degrade_current path.
                self.commit(state, LicenseEventKind::HeartbeatFailed);
                Err(err)
            }
            None => Ok(self.snapshot()),
        }
    }

    async fn deactivate(&self) -> Result<LicenseState> {
        self.deactivate_calls.fetch_add(1, Ordering::Relaxed);
        let scripted = self.script.lock().unwrap().deactivate.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::Deactivated)),
            Some(Err(e)) => Err(e),
            None => Ok(self.commit(
                LicenseState::inactive("deactivated"),
                LicenseEventKind::Deactivated,
            )),
        }
    }
}
