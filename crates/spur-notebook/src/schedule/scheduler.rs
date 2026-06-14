use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use chrono::{DateTime, Utc};
use jute::{
    backend::{
        notebook::{Cell, NotebookRoot},
        schedule::{CellCronTrigger, RunTarget},
    },
    notebook_store::NotebookDelta,
};
use serde::Serialize;
use tokio::{
    sync::{broadcast, Mutex},
    task::JoinHandle,
    time::{sleep, Duration},
};
use tracing::{debug, warn};

use crate::{
    dag::{engine::EngineError, run_context::notebook_run_context},
    mcp::ServerDeps,
    schedule::cron::next_fires,
};

const RECENT_RUN_LIMIT: usize = 32;
const IDLE_SLEEP: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireDecision {
    Fire,
    Skip,
    Wait,
}

/// Pure, testable fire decision for one cell at `now`.
pub fn decide_fire(
    now: DateTime<Utc>,
    next_fire: DateTime<Utc>,
    running: bool,
    skip_if_running: bool,
) -> FireDecision {
    if now < next_fire {
        return FireDecision::Wait;
    }
    if running && skip_if_running {
        FireDecision::Skip
    } else {
        FireDecision::Fire
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunStatus {
    Success,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleRunRecord {
    pub fired_at: DateTime<Utc>,
    pub status: ScheduleRunStatus,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// Per-cell runtime state held only in memory.
#[derive(Debug)]
pub struct RuntimeSchedule {
    pub trigger: CellCronTrigger,
    pub next_fire: Option<DateTime<Utc>>,
    pub running: bool,
    pub consecutive_failures: u32,
    pub recent: VecDeque<ScheduleRunRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleSnapshotEntry {
    pub cell_id: String,
    pub trigger: CellCronTrigger,
    pub next_fire: Option<DateTime<Utc>>,
    pub last_run: Option<ScheduleRunRecord>,
    pub consecutive_failures: u32,
    pub recent: Vec<ScheduleRunRecord>,
}

pub type ScheduleSnapshot = Vec<ScheduleSnapshotEntry>;

pub struct SchedulerHandle {
    task: JoinHandle<()>,
    state: Arc<Mutex<BTreeMap<String, RuntimeSchedule>>>,
}

impl SchedulerHandle {
    pub async fn snapshot(&self) -> ScheduleSnapshot {
        snapshot_from_state(&self.state).await
    }

    pub fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn spawn_scheduler(
    deps: Arc<ServerDeps>,
    mut delta_rx: broadcast::Receiver<NotebookDelta>,
) -> SchedulerHandle {
    let state = Arc::new(Mutex::new(BTreeMap::new()));
    let loop_state = Arc::clone(&state);
    let task = tokio::spawn(async move {
        rebuild_from_current_notebook(&deps, &loop_state).await;
        loop {
            let sleep_duration = sleep_duration_until_next_fire(&loop_state).await;
            tokio::select! {
                () = sleep(sleep_duration) => {
                    fire_due(&deps, &loop_state).await;
                }
                delta = delta_rx.recv() => {
                    match delta {
                        Ok(delta) => {
                            debug!(version = delta.version, "scheduler observed notebook delta");
                            rebuild_from_current_notebook(&deps, &loop_state).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            debug!(skipped, "scheduler delta subscriber lagged");
                            rebuild_from_current_notebook(&deps, &loop_state).await;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    SchedulerHandle { task, state }
}

async fn snapshot_from_state(
    state: &Arc<Mutex<BTreeMap<String, RuntimeSchedule>>>,
) -> ScheduleSnapshot {
    let schedules = state.lock().await;
    let mut entries = schedules
        .iter()
        .map(|(cell_id, schedule)| ScheduleSnapshotEntry {
            cell_id: cell_id.clone(),
            trigger: schedule.trigger.clone(),
            next_fire: schedule.next_fire,
            last_run: schedule.recent.back().cloned(),
            consecutive_failures: schedule.consecutive_failures,
            recent: schedule.recent.iter().cloned().collect(),
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.cell_id.cmp(&right.cell_id));
    entries
}

async fn sleep_duration_until_next_fire(
    state: &Arc<Mutex<BTreeMap<String, RuntimeSchedule>>>,
) -> Duration {
    let now = Utc::now();
    let next = {
        let schedules = state.lock().await;
        schedules
            .values()
            .filter_map(|schedule| schedule.next_fire)
            .min()
    };

    match next {
        Some(next_fire) if next_fire <= now => Duration::ZERO,
        Some(next_fire) => (next_fire - now).to_std().unwrap_or(Duration::ZERO),
        None => IDLE_SLEEP,
    }
}

async fn rebuild_from_current_notebook(
    deps: &Arc<ServerDeps>,
    state: &Arc<Mutex<BTreeMap<String, RuntimeSchedule>>>,
) {
    let Some(daemon) = deps.daemon.as_ref() else {
        state.lock().await.clear();
        return;
    };
    let Some(jute_state) = deps.state.as_ref() else {
        state.lock().await.clear();
        return;
    };
    let Some(path) = daemon.current_path().await else {
        state.lock().await.clear();
        return;
    };

    let store = jute_state.notebook_for_path(&path);
    let (root, _) = store.snapshot();
    let triggers = collect_triggers(&root);
    let now = Utc::now();
    let mut schedules = state.lock().await;

    schedules.retain(|cell_id, _| triggers.contains_key(cell_id));
    for (cell_id, trigger) in triggers {
        let next_fire = match next_fire_after(&trigger, now) {
            Some(next_fire) => Some(next_fire),
            None => {
                warn!(
                    cell_id,
                    cron = %trigger.cron,
                    timezone = %trigger.timezone,
                    "scheduler ignored invalid cron trigger"
                );
                schedules.remove(&cell_id);
                continue;
            }
        };
        if let Some(schedule) = schedules.get_mut(&cell_id) {
            if schedule.trigger != trigger {
                schedule.trigger = trigger;
                schedule.next_fire = next_fire;
            }
            continue;
        }
        schedules.insert(
            cell_id,
            RuntimeSchedule {
                trigger,
                next_fire,
                running: false,
                consecutive_failures: 0,
                recent: VecDeque::new(),
            },
        );
    }
}

fn collect_triggers(root: &NotebookRoot) -> BTreeMap<String, CellCronTrigger> {
    root.cells
        .iter()
        .filter_map(|cell| {
            let (cell_id, metadata) = match cell {
                Cell::Raw(cell) => (cell.id.as_ref()?, &cell.metadata),
                Cell::Markdown(cell) => (cell.id.as_ref()?, &cell.metadata),
                Cell::Code(cell) => (cell.id.as_ref()?, &cell.metadata),
            };
            let trigger = metadata.spur.as_ref()?.cron.as_ref()?;
            trigger.enabled.then(|| (cell_id.clone(), trigger.clone()))
        })
        .collect()
}

async fn fire_due(deps: &Arc<ServerDeps>, state: &Arc<Mutex<BTreeMap<String, RuntimeSchedule>>>) {
    let now = Utc::now();
    let mut to_run = Vec::new();
    {
        let mut schedules = state.lock().await;
        for (cell_id, schedule) in schedules.iter_mut() {
            let Some(next_fire) = schedule.next_fire else {
                continue;
            };
            match decide_fire(
                now,
                next_fire,
                schedule.running,
                schedule.trigger.skip_if_running,
            ) {
                FireDecision::Wait => {}
                FireDecision::Skip => {
                    push_record(
                        schedule,
                        ScheduleRunRecord {
                            fired_at: now,
                            status: ScheduleRunStatus::Skipped,
                            duration_ms: None,
                            error: Some("previous run still in progress".to_string()),
                        },
                    );
                    schedule.next_fire = next_fire_after(&schedule.trigger, now);
                }
                FireDecision::Fire => {
                    schedule.running = true;
                    schedule.next_fire = next_fire_after(&schedule.trigger, now);
                    to_run.push((cell_id.clone(), schedule.trigger.run_target));
                }
            }
        }
    }

    for (cell_id, run_target) in to_run {
        spawn_fire(
            Arc::clone(deps),
            Arc::clone(state),
            cell_id,
            run_target,
            now,
        );
    }
}

fn spawn_fire(
    deps: Arc<ServerDeps>,
    state: Arc<Mutex<BTreeMap<String, RuntimeSchedule>>>,
    cell_id: String,
    run_target: RunTarget,
    fired_at: DateTime<Utc>,
) {
    tokio::spawn(async move {
        let started = Instant::now();
        let result = run_scheduled_cell(&deps, &cell_id, run_target).await;
        let duration_ms = started.elapsed().as_millis().try_into().ok();
        let mut schedules = state.lock().await;
        let Some(schedule) = schedules.get_mut(&cell_id) else {
            return;
        };
        schedule.running = false;
        let status = match result {
            Ok(()) => {
                schedule.consecutive_failures = 0;
                ScheduleRunStatus::Success
            }
            Err(error) => {
                schedule.consecutive_failures = schedule.consecutive_failures.saturating_add(1);
                push_record(
                    schedule,
                    ScheduleRunRecord {
                        fired_at,
                        status: ScheduleRunStatus::Failed,
                        duration_ms,
                        error: Some(error),
                    },
                );
                return;
            }
        };
        push_record(
            schedule,
            ScheduleRunRecord {
                fired_at,
                status,
                duration_ms,
                error: None,
            },
        );
    });
}

async fn run_scheduled_cell(
    deps: &Arc<ServerDeps>,
    cell_id: &str,
    run_target: RunTarget,
) -> Result<(), String> {
    let state = deps
        .state
        .as_ref()
        .ok_or_else(|| "notebook daemon state unavailable".to_string())?;
    let daemon = deps
        .daemon
        .as_ref()
        .ok_or_else(|| "daemon control unavailable".to_string())?;
    let path = daemon
        .current_path()
        .await
        .ok_or_else(|| "no notebook is open".to_string())?;

    let mut context = notebook_run_context(
        &path,
        Arc::clone(state),
        Arc::clone(&deps.bridge),
        deps.app.clone(),
        deps.daemon.clone(),
    );

    match run_target {
        RunTarget::Cascade => context
            .engine
            .run_cell_and_cascade(cell_id)
            .await
            .map(|_| ())
            .map_err(format_engine_error),
        RunTarget::CellOnly => context
            .engine
            .run_cell(cell_id)
            .await
            .map(|_| ())
            .map_err(format_engine_error),
    }
}

fn format_engine_error(error: EngineError) -> String {
    error.to_string()
}

fn next_fire_after(trigger: &CellCronTrigger, after_utc: DateTime<Utc>) -> Option<DateTime<Utc>> {
    next_fires(&trigger.cron, &trigger.timezone, after_utc, 1)
        .ok()
        .and_then(|mut fires| fires.pop())
}

fn push_record(schedule: &mut RuntimeSchedule, record: ScheduleRunRecord) {
    schedule.recent.push_back(record);
    while schedule.recent.len() > RECENT_RUN_LIMIT {
        schedule.recent.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn scheduler_decide_waits_before_window() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 14, 10, 0).unwrap();
        let next = Utc.with_ymd_and_hms(2026, 6, 14, 14, 15, 0).unwrap();
        assert_eq!(decide_fire(now, next, false, true), FireDecision::Wait);
    }

    #[test]
    fn scheduler_decide_fires_at_window() {
        let t = Utc.with_ymd_and_hms(2026, 6, 14, 14, 15, 0).unwrap();
        assert_eq!(decide_fire(t, t, false, true), FireDecision::Fire);
    }

    #[test]
    fn scheduler_decide_skips_when_running_and_skip_set() {
        let t = Utc.with_ymd_and_hms(2026, 6, 14, 14, 15, 0).unwrap();
        assert_eq!(decide_fire(t, t, true, true), FireDecision::Skip);
        assert_eq!(decide_fire(t, t, true, false), FireDecision::Fire);
    }
}
