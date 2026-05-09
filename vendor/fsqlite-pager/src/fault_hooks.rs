//! Targeted fault hooks for group-commit publish verification.

use std::mem;
use std::sync::{LazyLock, Mutex};

use fsqlite_error::{FrankenError, Result};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultHookArm {
    pub run_id: String,
    pub scenario_id: String,
    pub invariant_family: String,
}

impl FaultHookArm {
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        scenario_id: impl Into<String>,
        invariant_family: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scenario_id: scenario_id.into(),
            invariant_family: invariant_family.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultInjectionRecord {
    pub trigger_seq: u64,
    pub point: &'static str,
    pub run_id: String,
    pub scenario_id: String,
    pub invariant_family: String,
    pub detail: String,
}

#[derive(Debug, Default)]
struct PagerFaultHookState {
    next_trigger_seq: u64,
    after_flush_before_publish: Option<FaultHookArm>,
    during_phase_c: Option<FaultHookArm>,
    drop_condvar_notify: Option<FaultHookArm>,
    records: Vec<FaultInjectionRecord>,
}

static PAGER_FAULT_HOOK_STATE: LazyLock<Mutex<PagerFaultHookState>> =
    LazyLock::new(|| Mutex::new(PagerFaultHookState::default()));

pub fn clear() {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *state = PagerFaultHookState::default();
}

#[must_use]
pub fn take_records() -> Vec<FaultInjectionRecord> {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mem::take(&mut state.records)
}

pub fn arm_after_flush_before_publish(arm: FaultHookArm) {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.after_flush_before_publish = Some(arm);
}

pub(crate) fn maybe_inject_after_flush_before_publish(
    flush_epoch: u64,
    batch_count: usize,
    frame_count: usize,
) -> Result<()> {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(arm) = state.after_flush_before_publish.take() else {
        return Ok(());
    };

    let detail =
        format!("flush_epoch={flush_epoch} batch_count={batch_count} frame_count={frame_count}");
    record_trigger(&mut state, &arm, "after_flush_before_publish", detail);
    Err(FrankenError::Io(std::io::Error::other(format!(
        "fault_inject:after_flush_before_publish run_id={} scenario_id={} invariant_family={}",
        arm.run_id, arm.scenario_id, arm.invariant_family
    ))))
}

/// Arm the Phase-C publication fault hook (F4 / H4).
///
/// When armed, `maybe_inject_during_phase_c()` fires once inside
/// `SimpleTransaction::commit()` after commit_seq is updated but before
/// snapshot publish completes. Simulates crash during metadata publication.
pub fn arm_during_phase_c(arm: FaultHookArm) {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.during_phase_c = Some(arm);
}

/// Check and fire the Phase-C publication fault hook.
pub(crate) fn maybe_inject_during_phase_c(commit_seq: u64, db_size: u32) -> Result<()> {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(arm) = state.during_phase_c.take() else {
        return Ok(());
    };

    let detail = format!("commit_seq={commit_seq} db_size={db_size}");
    record_trigger(&mut state, &arm, "during_phase_c", detail);
    Err(FrankenError::Io(std::io::Error::other(format!(
        "fault_inject:during_phase_c run_id={} scenario_id={} invariant_family={}",
        arm.run_id, arm.scenario_id, arm.invariant_family
    ))))
}

/// Arm the dropped-condvar-notify fault hook (F11 / H11).
///
/// When armed, `maybe_inject_drop_condvar_notify()` returns true,
/// signaling the caller to suppress the `Condvar::notify_all()` that normally
/// follows a successful `publish_completed_epoch()` store.  The completed epoch
/// is still published; only the wakeup is dropped so waiters must recover via
/// timeout-based rechecks.
pub fn arm_drop_condvar_notify(arm: FaultHookArm) {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.drop_condvar_notify = Some(arm);
}

/// Check and fire the dropped-condvar-notify hook.
///
/// Returns `true` if the hook fires (caller should suppress the notify).
pub(crate) fn maybe_inject_drop_condvar_notify(completed_epoch: u64) -> bool {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(arm) = state.drop_condvar_notify.take() else {
        return false;
    };

    let detail = format!("completed_epoch={completed_epoch}");
    record_trigger(&mut state, &arm, "drop_condvar_notify", detail);
    true
}

fn record_trigger(
    state: &mut PagerFaultHookState,
    arm: &FaultHookArm,
    point: &'static str,
    detail: String,
) {
    state.next_trigger_seq = state.next_trigger_seq.saturating_add(1);
    let record = FaultInjectionRecord {
        trigger_seq: state.next_trigger_seq,
        point,
        run_id: arm.run_id.clone(),
        scenario_id: arm.scenario_id.clone(),
        invariant_family: arm.invariant_family.clone(),
        detail,
    };
    warn!(
        target: "fsqlite_pager::fault_injection",
        trigger_seq = record.trigger_seq,
        point = record.point,
        run_id = %record.run_id,
        scenario_id = %record.scenario_id,
        invariant_family = %record.invariant_family,
        detail = %record.detail,
        "fault hook fired"
    );
    state.records.push(record);
}
