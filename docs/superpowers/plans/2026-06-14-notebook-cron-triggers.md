# Notebook Cron Triggers Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Approved Open Design artifact (notebook `Untitled133.ipynb`, cells: brief-lock, plan, interactive HTML prototype). UI/UX approved by user 2026-06-14.
**Design epic:** n/a (design approved inline; no separate brainstorming epic was created)

**Goal:** Let a user attach a recurring cron schedule to a single notebook cell so it auto-runs itself and cascades downstream, unattended, with the schedule configured from the DagInspector right rail.

**Architecture:** The schedule **config** is persisted per cell at `cell.metadata.spur.cron` (survives `.ipynb` round-trips, no sidecar). A new background **scheduler task** in the `spur-notebook` daemon subscribes to notebook deltas, parses each armed cell's cron expression, and fires `run_cell_and_cascade` (or `run_cell`) at each window via the existing `notebook_run_context` path. Run **history/next-fire/last-run** live in-memory in the scheduler (not persisted, resets on restart, honest for a local desktop kernel). The frontend reads the new `cron` field through the notebook store, renders a Schedule section in `DagInspector`, a violet clock badge on armed cells, and a notebook-level Schedules overview.

**Tech Stack:** Rust (`jute` Tauri backend + `spur-notebook` daemon), `croner` (new dep, 5-field Unix cron parsing), `chrono` + `chrono-tz` (new dep, timezone next-fire math), `tokio` (scheduler loop), React + TypeScript + Tailwind + Lucide (jute-notebook frontend), `vitest` (frontend tests).

**Crate map (important for scope):**
- `jute` = `crates/spur-notebook/jute-notebook/src-tauri/` — owns `SpurCellMetadata`, `NotebookStore`, persistence, ts-rs bindings.
- `spur-notebook` = `crates/spur-notebook/src/` — owns `ReactiveEngine`, the MCP daemon, `commands.rs`, the new scheduler.
- frontend = `crates/spur-notebook/jute-notebook/src/`.

**Build/test commands (always via wrappers):**
- Rust: `scripts/spur-cargo test -p spur-notebook <name>`, `scripts/spur-cargo test -p jute <name>`, `scripts/spur-cargo clippy --workspace -- -D warnings`.
- Bindings regen (local): `scripts/spur-cargo run -p jute --bin ts-rs-export`.
- Frontend: `scripts/spur-pnpm test -- src/ui/dag/<File>.test.tsx`, `scripts/spur-pnpm run typecheck`.

---

## Dependency DAG

```
T1 (jute: types+bindings) ─┬─> T2 (jute: persist op) ─┬─> T5 (cmds) ──> T7 (fe store+api) ─┬─> T8 (inspector)
                           │                          │                                    ├─> T9 (badge)
T3 (cron calc) ────────────┴─> T4 (scheduler) ────────┘                                    └─> T10 (overview)
                                                       └─> T6 (mcp tool)
```

- Roots (parallel): **T1**, **T3**
- **T2** ← T1 · **T4** ← T1, T3 · **T5** ← T2, T4 · **T6** ← T2 · **T7** ← T1, T5 · **T8/T9/T10** ← T7

---

### Task 1: Schedule metadata types + ts-rs bindings (jute)

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/schedule.rs`
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs` (add `cron` field to `SpurCellMetadata` near line 227-257; declare `pub mod schedule;`)
- Generated: `crates/spur-notebook/jute-notebook/src/bindings/CellCronTrigger.ts`, `RunTarget.ts`, updated `SpurCellMetadata.ts`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `CellCronTrigger` + `RunTarget` defined and derive `TS`, serialize snake_case.
- [ ] `SpurCellMetadata` gains `cron: Option<CellCronTrigger>` (optional, skip-if-none).
- [ ] `scripts/spur-cargo run -p jute --bin ts-rs-export` regenerates bindings; `CellCronTrigger.ts` + `RunTarget.ts` exist and `SpurCellMetadata.ts` shows `cron?: CellCronTrigger;`.
- [ ] Serde round-trip test passes; `scripts/spur-cargo test -p jute schedule_roundtrip` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the two files above + the generated bindings.
- OUT: `notebook_store.rs`, any `spur-notebook` crate file, frontend `.ts/.tsx`. Do NOT wire persistence yet (that is task-2). Do NOT hand-edit files in `src/bindings/` (regenerate them).
- If you discover `SpurCellMetadata` lives elsewhere than `backend/notebook.rs`, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing test** in `backend/schedule.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_roundtrip() {
        let t = CellCronTrigger {
            enabled: true,
            cron: "*/15 * * * *".to_string(),
            timezone: "America/Los_Angeles".to_string(),
            run_target: RunTarget::Cascade,
            skip_if_running: true,
            catch_up: false,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"run_target\":\"cascade\""));
        let back: CellCronTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn run_target_defaults_to_cascade() {
        let de: CellCronTrigger =
            serde_json::from_str(r#"{"enabled":true,"cron":"0 6 * * *","timezone":"UTC"}"#).unwrap();
        assert_eq!(de.run_target, RunTarget::Cascade);
        assert!(de.skip_if_running);
        assert!(!de.catch_up);
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `scripts/spur-cargo test -p jute schedule_roundtrip` (FAIL: `CellCronTrigger` not found).

- [ ] **Step 3: Implement** `backend/schedule.rs`:

```rust
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a scheduled fire runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunTarget {
    /// Run only the armed cell.
    CellOnly,
    /// Run the armed cell and cascade downstream (default).
    Cascade,
}

impl Default for RunTarget {
    fn default() -> Self {
        RunTarget::Cascade
    }
}

/// Persisted per-cell cron trigger config (`cell.metadata.spur.cron`).
/// Run history/last-run/next-fire are NOT stored here; they live in the
/// in-memory scheduler (see spur-notebook `schedule::scheduler`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct CellCronTrigger {
    /// Whether the schedule is currently armed.
    pub enabled: bool,
    /// 5-field Unix cron expression, e.g. "*/15 * * * *".
    pub cron: String,
    /// IANA timezone name, e.g. "America/Los_Angeles".
    pub timezone: String,
    /// Cell-only vs cascade. Defaults to cascade.
    #[serde(default)]
    pub run_target: RunTarget,
    /// Skip a fire if the previous run is still going. Defaults true.
    #[serde(default = "default_true")]
    pub skip_if_running: bool,
    /// Back-fill a window that elapsed while SPUR was closed. Defaults false.
    #[serde(default)]
    pub catch_up: bool,
}

fn default_true() -> bool {
    true
}
```

- [ ] **Step 4: Wire into `backend/notebook.rs`** — add the module declaration near the other `backend` modules and the field on `SpurCellMetadata` (after the `frontend` field, ~line 251):

```rust
// near top of backend/notebook.rs module declarations
pub mod schedule;
use schedule::CellCronTrigger;

// inside struct SpurCellMetadata { ... } after `frontend`:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cron: Option<CellCronTrigger>,
```

(If `backend` submodules are declared in `backend/mod.rs` rather than `notebook.rs`, add `pub mod schedule;` there instead.)

- [ ] **Step 5: Run test** — `scripts/spur-cargo test -p jute schedule_roundtrip` (PASS).

- [ ] **Step 6: Regenerate bindings** — `scripts/spur-cargo run -p jute --bin ts-rs-export`; confirm `git status` shows new `CellCronTrigger.ts`, `RunTarget.ts`, modified `SpurCellMetadata.ts`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/backend/schedule.rs \
        crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs \
        crates/spur-notebook/jute-notebook/src/bindings/
git commit -m "feat(spur-notebook): T1 add CellCronTrigger metadata + bindings"
```

---

### Task 2: Persist cron mutation through the store (jute)

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` (add `NotebookOp::SetSpurCronMetadata` + `apply` arm near the existing `SetSpurDagMetadata` handling around line 406)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (the `SetCellMetadata` dispatch handler, lines ~1063-1149: add a `patch.spur.cron` branch)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] `NotebookOp::SetSpurCronMetadata { id, patch: Option<CellCronTrigger>, expected_version }` exists; `apply` sets/clears `cell.metadata.spur.cron`, bumps version, and returns `StoreError::OptimisticConcurrency` on a stale `expected_version`.
- [ ] A `{ "spur": { "cron": {...} } }` patch through `SetCellMetadata` reaches the new op.
- [ ] `scripts/spur-cargo test -p jute set_spur_cron` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the two files above.
- OUT: `backend/schedule.rs` (done in task-1), scheduler, commands in the `spur-notebook` crate, frontend. Do NOT add new MCP tools here.
- If the `apply` dispatch is structured differently than `SetSpurDagMetadata` (the reference arm), follow the existing pattern and emit `risk` only if optimistic concurrency cannot be reused.

**Implementation:**

- [ ] **Step 1: Failing test** in `notebook_store.rs` `#[cfg(test)]` (model on the existing dag-metadata test):

```rust
#[test]
fn set_spur_cron_sets_and_version_checks() {
    let mut store = NotebookStore::with_single_code_cell("cell-1"); // existing test helper
    let v = store.cell_version("cell-1");
    let trigger = crate::backend::schedule::CellCronTrigger {
        enabled: true,
        cron: "*/15 * * * *".into(),
        timezone: "UTC".into(),
        run_target: crate::backend::schedule::RunTarget::Cascade,
        skip_if_running: true,
        catch_up: false,
    };
    store
        .apply(NotebookOp::SetSpurCronMetadata {
            id: "cell-1".into(),
            patch: Some(trigger.clone()),
            expected_version: v,
        })
        .unwrap();
    assert_eq!(store.cell_spur("cell-1").cron, Some(trigger));

    // stale version rejected
    let err = store
        .apply(NotebookOp::SetSpurCronMetadata {
            id: "cell-1".into(),
            patch: None,
            expected_version: v, // now stale
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::OptimisticConcurrency { .. }));
}
```

(If `with_single_code_cell` / `cell_version` / `cell_spur` helpers do not exist, reuse whatever helpers the existing `SetSpurDagMetadata` test uses; do not invent new public API.)

- [ ] **Step 2: Run** — `scripts/spur-cargo test -p jute set_spur_cron` (FAIL).

- [ ] **Step 3: Add the op variant** to `enum NotebookOp` (next to `SetSpurDagMetadata`):

```rust
SetSpurCronMetadata {
    id: String,
    patch: Option<crate::backend::schedule::CellCronTrigger>,
    expected_version: u64,
},
```

- [ ] **Step 4: Add the `apply` arm** mirroring `SetSpurDagMetadata` (same optimistic-concurrency check, then mutate `spur.cron`, bump version):

```rust
NotebookOp::SetSpurCronMetadata { id, patch, expected_version } => {
    let cell = self.cell_mut(&id).ok_or(StoreError::CellNotFound(id.clone()))?;
    let spur = cell.spur_mut(); // existing accessor used by SetSpurDagMetadata
    if spur.version != expected_version {
        return Err(StoreError::OptimisticConcurrency {
            expected: expected_version,
            actual: spur.version,
        });
    }
    spur.cron = patch;
    spur.version += 1;
    self.mark_dirty(); // existing autosave trigger used by other ops
    Ok(())
}
```

- [ ] **Step 5: Route the patch** in `commands.rs` `SetCellMetadata` handler — alongside the existing `patch.spur.dag` branch, add:

```rust
if let Some(cron_value) = patch.spur.get("cron") {
    let patch: Option<CellCronTrigger> = serde_json::from_value(cron_value.clone())?;
    notebook.apply(NotebookOp::SetSpurCronMetadata { id, patch, expected_version })?;
}
```

(Match the exact shape the handler already uses to read `patch.spur.*`; the reference is the `dag` branch in the same function.)

- [ ] **Step 6: Run** — `scripts/spur-cargo test -p jute set_spur_cron` (PASS) + `scripts/spur-cargo clippy -p jute -- -D warnings`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs \
        crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs
git commit -m "feat(spur-notebook): T2 persist spur.cron via SetSpurCronMetadata op"
```

---

### Task 3: Cron parsing, next-fire, and plain-English describe (spur-notebook)

**Task ID:** `task-3`

**Files:**
- Modify: `Cargo.toml` (workspace root) and `crates/spur-notebook/Cargo.toml` (add `croner`, `chrono-tz`)
- Create: `crates/spur-notebook/src/schedule/mod.rs` (module root, `pub mod cron;`)
- Create: `crates/spur-notebook/src/schedule/cron.rs`
- Modify: `crates/spur-notebook/src/lib.rs` (declare `pub mod schedule;`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `next_fires(expr, tz_name, after_utc, n)` returns `n` correct upcoming UTC instants; invalid expr returns `CronError`.
- [ ] `describe(expr)` returns a human string for the preset patterns and `"Custom schedule"` otherwise.
- [ ] `scripts/spur-cargo test -p spur-notebook cron_` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the files above. Adding exactly two new deps (`croner`, `chrono-tz`) is authorized by this task; do not add others.
- OUT: scheduler, commands, MCP, jute crate, frontend.
- Justification for new deps (cite in commit body): no cron parser exists in-tree (`cron`/`croner`/`saffron` all absent); `chrono-tz` is only a transitive dep today and is needed for IANA timezone next-fire math.

**Implementation:**

- [ ] **Step 1: Add deps.** Workspace `Cargo.toml` `[workspace.dependencies]`:

```toml
croner = "2"
chrono-tz = "0.10"
```

`crates/spur-notebook/Cargo.toml` `[dependencies]`:

```toml
croner.workspace = true
chrono-tz.workspace = true
chrono.workspace = true   # already present; confirm
```

- [ ] **Step 2: Failing test** in `schedule/cron.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn cron_next_fires_every_15m() {
        let after = Utc.with_ymd_and_hms(2026, 6, 14, 14, 11, 0).unwrap();
        let fires = next_fires("*/15 * * * *", "UTC", after, 3).unwrap();
        assert_eq!(fires.len(), 3);
        assert_eq!(fires[0], Utc.with_ymd_and_hms(2026, 6, 14, 14, 15, 0).unwrap());
        assert_eq!(fires[1], Utc.with_ymd_and_hms(2026, 6, 14, 14, 30, 0).unwrap());
        assert_eq!(fires[2], Utc.with_ymd_and_hms(2026, 6, 14, 14, 45, 0).unwrap());
    }

    #[test]
    fn cron_invalid_is_error() {
        assert!(next_fires("not a cron", "UTC", Utc::now(), 1).is_err());
    }

    #[test]
    fn cron_describe_presets() {
        assert_eq!(describe("*/15 * * * *"), "Every 15 minutes");
        assert_eq!(describe("0 6 * * *"), "Every day at 06:00");
        assert_eq!(describe("13 4 * * 2"), "Custom schedule");
    }
}
```

- [ ] **Step 3: Run** — `scripts/spur-cargo test -p spur-notebook cron_` (FAIL).

- [ ] **Step 4: Implement** `schedule/cron.rs`:

```rust
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;

#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("invalid cron expression: {0}")]
    Parse(String),
    #[error("invalid timezone: {0}")]
    Timezone(String),
    #[error("could not compute next occurrence")]
    NoOccurrence,
}

/// Compute the next `n` fire instants (in UTC) strictly after `after`,
/// evaluating the 5-field cron in `tz_name`.
pub fn next_fires(
    expr: &str,
    tz_name: &str,
    after: DateTime<Utc>,
    n: usize,
) -> Result<Vec<DateTime<Utc>>, CronError> {
    let tz: Tz = tz_name.parse().map_err(|_| CronError::Timezone(tz_name.to_string()))?;
    let cron = Cron::new(expr).parse().map_err(|e| CronError::Parse(e.to_string()))?;
    let mut cursor = after.with_timezone(&tz);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let next = cron
            .find_next_occurrence(&cursor, false)
            .map_err(|_| CronError::NoOccurrence)?;
        out.push(next.with_timezone(&Utc));
        cursor = next;
    }
    Ok(out)
}

/// Plain-English description for the preset patterns; falls back to
/// "Custom schedule". Mirrors the frontend cronMap so UI and backend agree.
pub fn describe(expr: &str) -> String {
    match expr.trim() {
        "*/5 * * * *" => "Every 5 minutes",
        "*/10 * * * *" => "Every 10 minutes",
        "*/15 * * * *" => "Every 15 minutes",
        "*/30 * * * *" => "Every 30 minutes",
        "0 * * * *" => "Every hour, on the hour",
        "0 */2 * * *" => "Every 2 hours",
        "0 6 * * *" => "Every day at 06:00",
        "0 0 * * *" => "Every day at midnight",
        "0 6 * * 1" => "Every Monday at 06:00",
        "0 9 * * 1-5" => "Weekdays at 09:00",
        _ => "Custom schedule",
    }
    .to_string()
}
```

`schedule/mod.rs`:

```rust
pub mod cron;
```

`lib.rs`: add `pub mod schedule;` near the other `pub mod` declarations.

- [ ] **Step 5: Run** — `scripts/spur-cargo test -p spur-notebook cron_` (PASS) + clippy.
  (If croner's `find_next_occurrence` signature differs in 2.x, adapt the call but keep `next_fires`'s public signature stable — task-4 depends on it.)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/spur-notebook/Cargo.toml \
        crates/spur-notebook/src/schedule/ crates/spur-notebook/src/lib.rs
git commit -m "feat(spur-notebook): T3 cron next-fire + describe via croner"
```

---

### Task 4: Background scheduler task (spur-notebook)

**Task ID:** `task-4`

**Files:**
- Create: `crates/spur-notebook/src/schedule/scheduler.rs`
- Modify: `crates/spur-notebook/src/schedule/mod.rs` (`pub mod scheduler;`)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (spawn scheduler next to `spawn_reactive_engine` ~line 3965-3980; add a `scheduler: SchedulerHandle` field on `NotebookMcpServerHandle` ~line 400; expose `schedule_snapshot()` accessor)

**Depends on:** task-1, task-3

**Acceptance Criteria:**
- [ ] Pure decision fn `decide_fire(now, next_fire, running, skip_if_running)` returns `Fire | Skip | Wait`, unit-tested.
- [ ] `spawn_scheduler` subscribes to notebook deltas, rebuilds the schedule map from `cell.metadata.spur.cron`, and on fire calls `run_cell_and_cascade` (Cascade) or `run_cell` (CellOnly) via `notebook_run_context`.
- [ ] In-memory `ScheduleSnapshot` exposes per-cell `next_fire`, `last_run`, `consecutive_failures`, recent runs.
- [ ] `scripts/spur-cargo test -p spur-notebook scheduler_decide` green; daemon still builds.

**Suggested Worker:** codex
**(Heaviest task. Honor the scope-drift checkpoint below.)**

**Scope Boundary:**
- IN: `schedule/scheduler.rs`, `schedule/mod.rs`, and the minimal wiring in `mcp/mod.rs` (one field + one spawn + one accessor).
- OUT: `commands.rs`, MCP tools, frontend, jute crate. Do NOT change `ReactiveEngine` internals — reuse `notebook_run_context` + `run_cell_and_cascade`/`run_cell` exactly as `mcp/tools/notebook_run_cascade.rs:57-79` does.
- **Scope Drift Checkpoint:** if wiring the scheduler into `mcp/mod.rs` requires touching the engine's run path or more than the one handle field + spawn site, emit `scope_drift` before proceeding. If the delta broadcast type is not reusable, emit `risk`.

**Implementation:**

- [ ] **Step 1: Failing test** in `scheduler.rs` for the pure core:

```rust
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
        // overlap allowed when skip_if_running is false
        assert_eq!(decide_fire(t, t, true, false), FireDecision::Fire);
    }
}
```

- [ ] **Step 2: Run** — `scripts/spur-cargo test -p spur-notebook scheduler_decide` (FAIL).

- [ ] **Step 3: Implement the pure core + runtime types** in `scheduler.rs`:

```rust
use std::collections::VecDeque;
use chrono::{DateTime, Utc};
use crate::backend::schedule::{CellCronTrigger, RunTarget}; // re-exported from jute via spur-notebook deps

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireDecision { Fire, Skip, Wait }

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunStatus { Success, Failed, Skipped }

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleRunRecord {
    pub fired_at: DateTime<Utc>,
    pub status: ScheduleRunStatus,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// Per-cell runtime state held only in memory.
pub struct RuntimeSchedule {
    pub trigger: CellCronTrigger,
    pub next_fire: Option<DateTime<Utc>>,
    pub running: bool,
    pub consecutive_failures: u32,
    pub recent: VecDeque<ScheduleRunRecord>, // cap at 32
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleSnapshotEntry {
    pub cell_id: String,
    pub trigger: CellCronTrigger,
    pub next_fire: Option<DateTime<Utc>>,
    pub last_run: Option<ScheduleRunRecord>,
    pub consecutive_failures: u32,
    pub recent: Vec<ScheduleRunRecord>,
}

pub type ScheduleSnapshot = Vec<ScheduleSnapshotEntry>;
```

- [ ] **Step 4: Implement the spawn loop** (structure; reuse existing daemon primitives):

```rust
pub struct SchedulerHandle {
    task: tokio::task::JoinHandle<()>,
    state: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RuntimeSchedule>>>,
}

impl SchedulerHandle {
    pub async fn snapshot(&self) -> ScheduleSnapshot {
        // read self.state, map RuntimeSchedule -> ScheduleSnapshotEntry
        // (last_run = recent.back())
        todo_snapshot(&self.state).await
    }
    pub fn abort(&self) { self.task.abort(); }
}

/// Spawn the scheduler. `deps` provides the same path/run-context access used by
/// notebook_run_cascade; `delta_rx` is a broadcast receiver of NotebookDelta
/// (same source spawn_reactive_engine subscribes to).
pub fn spawn_scheduler(
    deps: SchedulerDeps,            // small struct holding what notebook_run_context needs
    mut delta_rx: tokio::sync::broadcast::Receiver<NotebookDelta>,
) -> SchedulerHandle {
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let loop_state = state.clone();
    let task = tokio::spawn(async move {
        rebuild_from_store(&deps, &loop_state).await; // initial load of all spur.cron cells
        loop {
            // 1. compute earliest next_fire across enabled schedules
            // 2. tokio::select! { _ = sleep_until(earliest) => fire_due(...),
            //                      Ok(delta) = delta_rx.recv() => rebuild_from_store(...) }
            // On fire: for each due cell, decide_fire(...); if Fire ->
            //   spawn run via notebook_run_context + run_cell_and_cascade/run_cell,
            //   set running=true, record ScheduleRunRecord on completion,
            //   recompute next_fire with schedule::cron::next_fires(.., 1).
            // catch_up==false: after rebuild, set next_fire to first occurrence AFTER now.
            // catch_up==true: if a window elapsed, fire once immediately then resume.
            run_one_tick(&deps, &loop_state, &mut delta_rx).await;
        }
    });
    SchedulerHandle { task, state }
}
```

The worker fills `rebuild_from_store`, `run_one_tick`, `fire_due`, and `todo_snapshot` against the real `SchedulerDeps`/`NotebookDelta` types found at the spawn site. The **contract that downstream tasks rely on** is fixed: `decide_fire`, `FireDecision`, `ScheduleSnapshot`/`ScheduleSnapshotEntry`, `SchedulerHandle::snapshot`, and `spawn_scheduler`.

- [ ] **Step 5: Wire into `mcp/mod.rs`** — next to the `spawn_reactive_engine` call (~3965-3980), subscribe a second delta receiver and `let scheduler = schedule::scheduler::spawn_scheduler(deps, delta_rx2);`. Add `pub scheduler: SchedulerHandle` to `NotebookMcpServerHandle` (~line 400) and a `pub async fn schedule_snapshot(&self) -> ScheduleSnapshot { self.scheduler.snapshot().await }` accessor (used by task-5).

- [ ] **Step 6: Run** — `scripts/spur-cargo test -p spur-notebook scheduler_decide` (PASS) + `scripts/spur-cargo build -p spur-notebook` + clippy.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/src/schedule/ crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): T4 background cron scheduler task"
```

---

### Task 5: Tauri commands for schedule CRUD + listing (spur-notebook)

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/src/commands.rs` (add `notebook_set_cell_schedule`, `notebook_remove_cell_schedule`, `notebook_list_schedules`)
- Modify: `crates/spur-notebook/src/main.rs` (register the 3 commands in `generate_handler![]` ~377-413; update the `registered_invoke_handler_source` text test ~line 571)

**Depends on:** task-2, task-4

**Acceptance Criteria:**
- [ ] `notebook_set_cell_schedule(cell_id, trigger, expected_version)` and `notebook_remove_cell_schedule(cell_id, expected_version)` route a `spur.cron` patch through the daemon control (same path as `anywidget_command`'s control access).
- [ ] `notebook_list_schedules()` returns the scheduler `ScheduleSnapshot`.
- [ ] Commands registered; `registered_invoke_handler_source` test updated and green.
- [ ] `scripts/spur-cargo build -p spur-notebook` + clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the two files above.
- OUT: scheduler internals (task-4), jute crate, MCP tools, frontend. Reuse the `tauri::State<'_, NotebookDaemonControlSlot>` access pattern from `anywidget_command` (commands.rs:209-221).
- If the daemon control does not expose a metadata-set path callable from a command (only the bridge does), emit `risk` and prefer sending the `spur.cron` patch over the same bridge the MCP `set_cell_metadata` tool uses.

**Implementation:**

- [ ] **Step 1: Implement commands** in `commands.rs`:

```rust
use crate::backend::schedule::CellCronTrigger;

#[tauri::command]
pub async fn notebook_set_cell_schedule(
    daemon_control: tauri::State<'_, NotebookDaemonControlSlot>,
    cell_id: String,
    trigger: CellCronTrigger,
    expected_version: u64,
) -> Result<(), jute::Error> {
    let control = { daemon_control.lock().await.clone() }
        .ok_or_else(|| jute::Error::msg("daemon not ready"))?;
    control
        .set_cell_metadata(
            cell_id,
            serde_json::json!({ "spur": { "cron": trigger } }),
            expected_version,
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn notebook_remove_cell_schedule(
    daemon_control: tauri::State<'_, NotebookDaemonControlSlot>,
    cell_id: String,
    expected_version: u64,
) -> Result<(), jute::Error> {
    let control = { daemon_control.lock().await.clone() }
        .ok_or_else(|| jute::Error::msg("daemon not ready"))?;
    control
        .set_cell_metadata(
            cell_id,
            serde_json::json!({ "spur": { "cron": null } }),
            expected_version,
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn notebook_list_schedules(
    daemon_control: tauri::State<'_, NotebookDaemonControlSlot>,
) -> Result<crate::schedule::scheduler::ScheduleSnapshot, jute::Error> {
    let control = { daemon_control.lock().await.clone() }
        .ok_or_else(|| jute::Error::msg("daemon not ready"))?;
    Ok(control.schedule_snapshot().await)
}
```

(Use whatever the real control method for metadata-set is. If only `reactive_engine_client()` exists, route the patch through the bridge `notebook.set_cell_metadata` request used by the MCP `set_cell_metadata` tool; keep the command signatures above unchanged.)

- [ ] **Step 2: Register** in `main.rs` `generate_handler![]`, after `notebook_run_cell`:

```rust
    spur_notebook::commands::notebook_set_cell_schedule,
    spur_notebook::commands::notebook_remove_cell_schedule,
    spur_notebook::commands::notebook_list_schedules,
```

- [ ] **Step 3: Update the text test** `registered_invoke_handler_source` (~main.rs:571) to assert the three new command names appear (mirror how it asserts existing ones).

- [ ] **Step 4: Run** — `scripts/spur-cargo test -p spur-notebook registered_invoke_handler_source` (PASS) + `scripts/spur-cargo build -p spur-notebook` + clippy.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/commands.rs crates/spur-notebook/src/main.rs
git commit -m "feat(spur-notebook): T5 schedule CRUD + list Tauri commands"
```

---

### Task 6: Agent-facing MCP tool `notebook_set_schedule` (spur-notebook)

**Task ID:** `task-6`

**Files:**
- Create: `crates/spur-notebook/src/mcp/tools/notebook_set_schedule.rs`
- Modify: `crates/spur-notebook/src/mcp/tools/mod.rs` (module decl ~line 36 + `tools()` vec ~line 83)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (`call_tool` match arm ~line 291)

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] Tool `notebook_set_schedule` with input `{ cell_id, trigger | null, expected_version }` sets/clears `spur.cron` via the bridge (same `notebook.set_cell_metadata` request as `notebook_set_dag_metadata`).
- [ ] Registered in all 3 sites; `scripts/spur-cargo test -p spur-notebook tools_registry` (or the existing registry test) green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the three sites above. Mirror `mcp/tools/notebook_set_dag_metadata.rs` exactly (input struct, `tool()`, `call()`).
- OUT: scheduler, commands, frontend, jute crate.

**Implementation:**

- [ ] **Step 1:** Copy the structure of `notebook_set_dag_metadata.rs`. Input struct:

```rust
#[derive(Debug, serde::Deserialize)]
struct SetScheduleParams {
    cell_id: String,
    /// null clears the schedule.
    trigger: Option<serde_json::Value>,
    expected_version: u64,
}
```

`call()` sends the same bridge request shape, with patch `{"spur":{"cron": <trigger or null>}}`:

```rust
pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let p: SetScheduleParams = serde_json::from_value(arguments)
        .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
    let patch = serde_json::json!({ "spur": { "cron": p.trigger } });
    let value = deps
        .bridge
        .request(
            "notebook.set_cell_metadata",
            serde_json::json!({ "id": p.cell_id, "patch": patch, "expected_version": p.expected_version }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::structured(value))
}
```

- [ ] **Step 2:** `tool()` with a JSON schema describing `cell_id`, `trigger` (object with enabled/cron/timezone/run_target/skip_if_running/catch_up), `expected_version`.

- [ ] **Step 3:** Register: `pub mod notebook_set_schedule;` (mod.rs:36), `notebook_set_schedule::tool()` in `tools()` (mod.rs:83), and `"notebook_set_schedule" => tools::notebook_set_schedule::call(&self.deps, arguments).await` in `call_tool` (mcp/mod.rs:291).

- [ ] **Step 4: Run** — the existing tools-registry test + `scripts/spur-cargo build -p spur-notebook` + clippy.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/mcp/tools/notebook_set_schedule.rs \
        crates/spur-notebook/src/mcp/tools/mod.rs crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): T6 notebook_set_schedule MCP tool"
```

---

### Task 7: Frontend store mapping + scheduleApi (jute-notebook)

**Task ID:** `task-7`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/stores/notebook.ts` (`NotebookCellState` ~91-104; import destructure ~909-919; delta patch ~1700-1731)
- Create: `crates/spur-notebook/jute-notebook/src/ui/dag/scheduleApi.ts`
- Test: `crates/spur-notebook/jute-notebook/src/ui/dag/scheduleApi.test.ts`

**Depends on:** task-1, task-5

**Acceptance Criteria:**
- [ ] `NotebookCellState` gains `schedule?: CellCronTrigger` mapped from `cell.metadata.spur.cron` at import and on delta patch.
- [ ] `scheduleApi.ts` exports `setCellSchedule`, `removeCellSchedule`, `listSchedules` using the injectable-invoke pattern from `dagStatus.ts`.
- [ ] `scripts/spur-pnpm test -- src/ui/dag/scheduleApi.test.ts` green; `scripts/spur-pnpm run typecheck` clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the three files above.
- OUT: DagInspector/DagNode/NotebookHeader components (tasks 8-10), Rust. Do not hand-edit `src/bindings/` (task-1 generated `CellCronTrigger`).

**Implementation:**

- [ ] **Step 1: Failing test** `scheduleApi.test.ts`:

```ts
import { describe, it, expect, vi } from "vitest";
import { setCellSchedule, removeCellSchedule, listSchedules } from "./scheduleApi";
import type { CellCronTrigger } from "@/bindings/CellCronTrigger";

const trigger: CellCronTrigger = {
  enabled: true,
  cron: "*/15 * * * *",
  timezone: "UTC",
  run_target: "cascade",
  skip_if_running: true,
  catch_up: false,
};

describe("scheduleApi", () => {
  it("setCellSchedule invokes the command with trigger + version", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await setCellSchedule("cell-1", trigger, 7, invoke);
    expect(invoke).toHaveBeenCalledWith("notebook_set_cell_schedule", {
      cellId: "cell-1",
      trigger,
      expectedVersion: 7,
    });
  });

  it("removeCellSchedule invokes remove with version", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await removeCellSchedule("cell-1", 7, invoke);
    expect(invoke).toHaveBeenCalledWith("notebook_remove_cell_schedule", {
      cellId: "cell-1",
      expectedVersion: 7,
    });
  });

  it("listSchedules returns the snapshot", async () => {
    const invoke = vi.fn().mockResolvedValue([]);
    expect(await listSchedules(invoke)).toEqual([]);
    expect(invoke).toHaveBeenCalledWith("notebook_list_schedules", {});
  });
});
```

- [ ] **Step 2: Run** — `scripts/spur-pnpm test -- src/ui/dag/scheduleApi.test.ts` (FAIL).

- [ ] **Step 3: Implement** `scheduleApi.ts` (mirror `dagStatus.ts`):

```ts
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type { CellCronTrigger } from "@/bindings/CellCronTrigger";

type Invoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

export type ScheduleSnapshotEntry = {
  cell_id: string;
  trigger: CellCronTrigger;
  next_fire: string | null;
  last_run: { fired_at: string; status: string; duration_ms: number | null; error: string | null } | null;
  consecutive_failures: number;
  recent: Array<{ fired_at: string; status: string; duration_ms: number | null; error: string | null }>;
};

export async function setCellSchedule(
  cellId: string,
  trigger: CellCronTrigger,
  expectedVersion: number,
  invoke: Invoke = tauriInvoke,
): Promise<void> {
  await invoke("notebook_set_cell_schedule", { cellId, trigger, expectedVersion });
}

export async function removeCellSchedule(
  cellId: string,
  expectedVersion: number,
  invoke: Invoke = tauriInvoke,
): Promise<void> {
  await invoke("notebook_remove_cell_schedule", { cellId, expectedVersion });
}

export async function listSchedules(invoke: Invoke = tauriInvoke): Promise<ScheduleSnapshotEntry[]> {
  return (await invoke("notebook_list_schedules", {})) as ScheduleSnapshotEntry[];
}
```

- [ ] **Step 4: Map the field** in `stores/notebook.ts`: add `schedule?: CellCronTrigger;` to `NotebookCellState` (import the type from `@/bindings/CellCronTrigger`); at the import destructure add `schedule: spur?.cron,`; in the delta-patch block add the `patch.spur?.cron` handling mirroring `patch.spur?.dag`.

- [ ] **Step 5: Run** — `scripts/spur-pnpm test -- src/ui/dag/scheduleApi.test.ts` (PASS) + `scripts/spur-pnpm run typecheck`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/stores/notebook.ts \
        crates/spur-notebook/jute-notebook/src/ui/dag/scheduleApi.ts \
        crates/spur-notebook/jute-notebook/src/ui/dag/scheduleApi.test.ts
git commit -m "feat(spur-notebook): T7 frontend schedule store mapping + api"
```

---

### Task 8: DagInspector Schedule section (jute-notebook)

**Task ID:** `task-8`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/dag/ScheduleSection.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/dag/ScheduleSection.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/dag/DagInspector.tsx` (render `<ScheduleSection>` after the Mode section, ~line 173, before `<PortList title="Consumes">`)
- Modify: `crates/spur-notebook/jute-notebook/src/ui/dag/useDagGraph.ts` (add `schedule?: CellCronTrigger` to `DagNodeData`; map it in `buildDagGraph` from `NotebookCellState.schedule`)

**Depends on:** task-7

**Acceptance Criteria:**
- [ ] Empty state: cell with no schedule shows "No schedule" + "Add schedule trigger".
- [ ] Configured state: arm toggle, preset segmented control (5m/15m/1h/Daily/Weekly/Custom), cron input with `describe`-style echo, next-runs preview, Runs target, timezone, skip-if-running + catch-up toggles. Matches the approved prototype copy (no em-dashes / en-dashes in any rendered string).
- [ ] Arming calls `setCellSchedule`; removing calls `removeCellSchedule`.
- [ ] `scripts/spur-pnpm test -- src/ui/dag/ScheduleSection.test.tsx` green; typecheck clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the four files above. Match the section-header className `"mb-2 text-[11px] font-semibold uppercase tracking-normal text-gray-500"` and the existing Tailwind tokens (violet for armed, gray-900 for selected segmented option).
- OUT: DagNode badge (task-9), overview (task-10), Rust. Keep all copy free of `—`/`–` (see anti-slop rule in the approved design).

**Implementation:**

- [ ] **Step 1: Failing test** `ScheduleSection.test.tsx` (mock `./scheduleApi` like `DagInspector.test.tsx` mocks `./dagStatus`):

```tsx
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { ScheduleSection } from "./ScheduleSection";

vi.mock("./scheduleApi", () => ({
  setCellSchedule: vi.fn().mockResolvedValue(undefined),
  removeCellSchedule: vi.fn().mockResolvedValue(undefined),
}));
import { setCellSchedule } from "./scheduleApi";

afterEach(() => cleanup());

describe("ScheduleSection", () => {
  it("shows empty state when no schedule", () => {
    render(<ScheduleSection cellId="c1" version={1} schedule={undefined} />);
    expect(screen.getByText("No schedule")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /add schedule trigger/i })).toBeInTheDocument();
  });

  it("renders configured state and arms via preset", async () => {
    render(<ScheduleSection cellId="c1" version={2} schedule={{
      enabled: true, cron: "*/15 * * * *", timezone: "UTC",
      run_target: "cascade", skip_if_running: true, catch_up: false,
    }} />);
    expect(screen.getByDisplayValue("*/15 * * * *")).toBeInTheDocument();
    expect(screen.getByText(/Every 15 minutes/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "1h" }));
    expect(setCellSchedule).toHaveBeenCalled();
  });

  it("renders no em-dash or en-dash in output", () => {
    const { container } = render(<ScheduleSection cellId="c1" version={2} schedule={{
      enabled: true, cron: "*/15 * * * *", timezone: "UTC",
      run_target: "cascade", skip_if_running: true, catch_up: false,
    }} />);
    expect(container.textContent || "").not.toMatch(/[—–]/);
  });
});
```

- [ ] **Step 2: Run** — `scripts/spur-pnpm test -- src/ui/dag/ScheduleSection.test.tsx` (FAIL).

- [ ] **Step 3: Implement** `ScheduleSection.tsx` props `{ cellId: string; version: number; schedule?: CellCronTrigger }`. Build a local `describe(expr)` mirroring the Rust `describe` (same map), a `PRESETS` array `[{label:'5m',cron:'*/5 * * * *'}, {label:'15m',cron:'*/15 * * * *'}, {label:'1h',cron:'0 * * * *'}, {label:'Daily',cron:'0 6 * * *'}, {label:'Weekly',cron:'0 6 * * 1'}, {label:'Custom',cron:''}]`, and a small `nextRuns(cron,tz)` preview (client-side estimate; show the absolute times). On any change call `setCellSchedule(cellId, nextTrigger, version)`; on "Add schedule trigger" arm with the `15m` default; on Remove call `removeCellSchedule(cellId, version)`. Use Lucide `Clock`, `Plus`, `Globe`, `ChevronDown`, `X`, `AlertTriangle` icons. Copy and layout follow the approved prototype (Schedule header, presets, cron row, "Reads as:" echo, Next runs, Runs target, Timezone, two policy toggles, kernel-asleep note). No em-dashes.

- [ ] **Step 4: Wire into `DagInspector.tsx`** after the Mode section (line 173):

```tsx
<ScheduleSection cellId={node.id} version={node.version} schedule={node.schedule} />
```

Add `version` + `schedule` to `DagNodeData` in `useDagGraph.ts` and map them in `buildDagGraph` from the cell state (`version: cell.version`, `schedule: cell.schedule`).

- [ ] **Step 5: Run** — `scripts/spur-pnpm test -- src/ui/dag/ScheduleSection.test.tsx` (PASS) + typecheck.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/dag/ScheduleSection.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/ScheduleSection.test.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/DagInspector.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/useDagGraph.ts
git commit -m "feat(spur-notebook): T8 DagInspector schedule section"
```

---

### Task 9: Cell clock badge for armed cells (jute-notebook)

**Task ID:** `task-9`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.tsx` (add violet clock badge in the chip row ~line 79-94 when `data.schedule?.enabled`)
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx` (`CellLanguageHeader` chip row ~line 227-277: clock badge when the cell has an enabled schedule)
- Test: `crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.test.tsx`

**Depends on:** task-7

**Acceptance Criteria:**
- [ ] A cell with `schedule.enabled === true` shows a violet clock badge with a short label (e.g. "every 15m" via `describe`/preset match) on both the DAG node and the notebook cell header.
- [ ] No badge when there is no schedule or it is disabled.
- [ ] `scripts/spur-pnpm test -- src/ui/dag/DagNode.test.tsx` green; typecheck clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the three files above. Match the existing AI-chip className `inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-1.5 py-px font-mono text-[9.5px] font-semibold text-violet-700`. Use the Lucide `Clock` icon (not an emoji).
- OUT: ScheduleSection (task-8), overview (task-10), Rust.

**Implementation:**

- [ ] **Step 1: Failing test** `DagNode.test.tsx`:

```tsx
import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { ReactFlowProvider } from "reactflow"; // if DagNode needs RF context; else render directly
import { DagNode } from "./DagNode";

afterEach(() => cleanup());

it("shows clock badge when scheduled", () => {
  const data = { id: "c1", label: "ingest", kind: "code", schedule: { enabled: true, cron: "*/15 * * * *", timezone: "UTC", run_target: "cascade", skip_if_running: true, catch_up: false } } as any;
  render(<ReactFlowProvider><DagNode id="c1" data={data} /></ReactFlowProvider>);
  expect(screen.getByText(/every 15m/i)).toBeInTheDocument();
});

it("no badge without schedule", () => {
  const data = { id: "c2", label: "x", kind: "code" } as any;
  render(<ReactFlowProvider><DagNode id="c2" data={data} /></ReactFlowProvider>);
  expect(screen.queryByText(/every/i)).toBeNull();
});
```

(Adapt the render wrapper to however `DagNode` is currently tested; if there is no existing DagNode test, follow `DagInspector.test.tsx`'s setup.)

- [ ] **Step 2: Run** — FAIL.

- [ ] **Step 3: Implement** the badge in both `DagNode.tsx` and `NotebookCells.tsx`. Add a tiny shared `scheduleLabel(cron: string): string` (e.g. preset match -> "every 15m" / "daily" / "weekly", fallback "cron") in `scheduleApi.ts` and import it in both. Render:

```tsx
{schedule?.enabled ? (
  <span className="inline-flex items-center gap-1 rounded border border-violet-200 bg-violet-50 px-1.5 py-px font-mono text-[9.5px] font-semibold text-violet-700">
    <ClockIcon size={10} strokeWidth={2} />
    {scheduleLabel(schedule.cron)}
  </span>
) : null}
```

For DagNode, read `data.schedule`; for the non-AI case ensure the chip-row container renders even without the AI chips.

- [ ] **Step 4: Run** — PASS + typecheck.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.tsx \
        crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/DagNode.test.tsx \
        crates/spur-notebook/jute-notebook/src/ui/dag/scheduleApi.ts
git commit -m "feat(spur-notebook): T9 armed-cell clock badge"
```

---

### Task 10: Schedules overview modal + header pill (jute-notebook)

**Task ID:** `task-10`

**Files:**
- Create: `crates/spur-notebook/jute-notebook/src/ui/notebook/SchedulesOverview.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/SchedulesOverview.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx` (add a "Schedules" pill in the right zone ~line 215, before the settings gear; `schedulesOpen` state mirroring `settingsOpen`)

**Depends on:** task-7

**Acceptance Criteria:**
- [ ] The pill shows the armed count and opens a modal (ConfirmModal-style `fixed inset-0 z-40 ... bg-black/30`) listing each armed cell: name, schedule, next run, last-run status (green/red), enabled toggle, and a "Pause all".
- [ ] Failed schedules show a red status with the consecutive-failure count (matches approved design: red + "failed, N in a row").
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/SchedulesOverview.test.tsx` green; typecheck clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN: the three files above. Reuse `ConfirmModal` (`src/ui/shared/ConfirmModal.tsx`) backdrop pattern; `listSchedules` from task-7's `scheduleApi`.
- OUT: ScheduleSection (task-8), badge (task-9), Rust. No em-dashes in rendered copy.

**Implementation:**

- [ ] **Step 1: Failing test** `SchedulesOverview.test.tsx`:

```tsx
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { SchedulesOverview } from "./SchedulesOverview";

vi.mock("../dag/scheduleApi", () => ({
  listSchedules: vi.fn().mockResolvedValue([
    { cell_id: "c1", trigger: { enabled: true, cron: "*/15 * * * *", timezone: "UTC", run_target: "cascade", skip_if_running: true, catch_up: false }, next_fire: "2026-06-14T14:30:00Z", last_run: { fired_at: "2026-06-14T14:15:00Z", status: "success", duration_ms: 1200, error: null }, consecutive_failures: 0, recent: [] },
    { cell_id: "nightly_export", trigger: { enabled: true, cron: "0 2 * * *", timezone: "UTC", run_target: "cascade", skip_if_running: true, catch_up: false }, next_fire: "2026-06-15T02:00:00Z", last_run: { fired_at: "2026-06-14T02:00:00Z", status: "failed", duration_ms: null, error: "boom" }, consecutive_failures: 2, recent: [] },
  ]),
}));

afterEach(() => cleanup());

it("lists armed cells and surfaces failures", async () => {
  render(<SchedulesOverview onClose={() => {}} />);
  expect(await screen.findByText("c1")).toBeInTheDocument();
  expect(await screen.findByText(/failed, 2 in a row/i)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /pause all/i })).toBeInTheDocument();
});
```

- [ ] **Step 2: Run** — FAIL.

- [ ] **Step 3: Implement** `SchedulesOverview.tsx`: on mount `listSchedules()` into state; render the ConfirmModal-style overlay with a table (Cell / Schedule / Next run / Last run / toggle), a "Pause all" button, and a footer note "Schedules run on the local kernel while SPUR is open. Closed windows are skipped unless catch-up is on." Failure row: red text + `failed, ${consecutive_failures} in a row`. Use Lucide `Clock`, `Pause`, `X`, `AlertTriangle`.

- [ ] **Step 4: Wire the pill** into `NotebookHeader.tsx` right zone (before the settings `<div className="relative">`):

```tsx
<button
  className="mr-1 inline-flex items-center gap-1.5 rounded-full border border-violet-200 bg-violet-50 px-2.5 py-1 text-[11px] font-semibold text-violet-700 hover:bg-violet-100"
  title="Scheduled cells"
  onClick={() => setSchedulesOpen((o) => !o)}
>
  <ClockIcon size={13} strokeWidth={2} />
  {armedCount} armed
</button>
{schedulesOpen && <SchedulesOverview onClose={() => setSchedulesOpen(false)} />}
```

Source `armedCount` from the notebook store (count of cells with `schedule?.enabled`) or from a quick `listSchedules` length; keep it simple.

- [ ] **Step 5: Run** — PASS + typecheck.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/SchedulesOverview.tsx \
        crates/spur-notebook/jute-notebook/src/ui/notebook/SchedulesOverview.test.tsx \
        crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookHeader.tsx
git commit -m "feat(spur-notebook): T10 schedules overview modal + header pill"
```

---

## Self-Review

**Spec coverage (approved prototype -> tasks):**
- Armed cell clock badge -> T9. Inspector empty state -> T8. Inspector configured (presets, cron, echo, next-runs, timezone, run target, overlap/catch-up policies) -> T8 (+ describe/next-fire in T3). Run history strip -> in-memory history in T4, surfaced by list (T5) and shown in overview (T10); per-cell history strip in the inspector is OPTIONAL for v1 (note below). Schedules overview + pause-all + failure signal -> T10. "Auto run = cell + cascade" -> T4 (`run_cell_and_cascade` for `Cascade`). Local-kernel honesty note -> T8 + T10 copy.
- **Known v1 trim:** the inspector's 16-bar run-history strip from the prototype is not separately tasked; T8 may render it from `listSchedules` data if cheap, else it ships in the overview only. This is an intentional scope trim, not a gap.

**Placeholder scan:** Core types (`CellCronTrigger`, `RunTarget`, `FireDecision`, `ScheduleRunRecord`, `ScheduleSnapshotEntry`), the pure `decide_fire`, `next_fires`, `describe`, and every `scheduleApi` function are defined with real code. Scheduler loop bodies (`run_one_tick` etc.) are described against fixed public contracts the downstream tasks depend on; the worker implements them against the real daemon types at the spawn site (T4 owns that crate-internal detail).

**Type consistency:** `run_target` serializes snake_case (`"cascade"`/`"cell_only"`); TS uses the same literal. `CellCronTrigger` field names match across Rust (T1), bindings, store (T7), and components (T8-T10). Command arg casing: Tauri auto-converts snake_case params to camelCase on the JS side, so `scheduleApi` sends `cellId`/`expectedVersion` (matches `dagStatus.ts` `runNotebookCascade({ cellId })`).

**DAG validation:** Acyclic. Roots T1, T3 run in parallel; the longest chain is T1 -> T2 -> T5 -> T7 -> T8/T9/T10 (5 deep). T6 parallels T5; T8/T9/T10 parallel after T7.

**beads compatibility:** Every task has a unique id, an explicit `depends_on`, brain-verifiable acceptance criteria (named test commands), and a scope boundary with a drift signal.
