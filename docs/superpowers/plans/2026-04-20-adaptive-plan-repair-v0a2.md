# Adaptive Plan Repair — v0a.2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:test-driven-development` per task.

**Goal:** Complete the infrastructure layer started in v0a.1. v0a.2 lands (a) audit transport around SPUR-owned `[[spur-audit v1]]` comment breadcrumbs (br audit-record is empirically DOA), (b) bv-primary reconciler using the existing BvAdapter, (c) F1 boundary-safe cursor, (d) correlation backfill + partial-subgraph gate prerequisites.

**Prerequisite context:** Read the "Review addendum II (2026-04-20, post-v0a.1 empirical verification)" section of `docs/superpowers/plans/2026-04-20-adaptive-plan-repair-v0a.md`. It encodes the three-round empirical findings against `br 0.1.14` that drove this design.

**Tech stack:** Rust 2024, `tokio`, `async-trait`, `serde`, `chrono`, `uuid`, `anyhow`. Integration tests shell out to real `br` 0.1.14 and `bv` 0.15.2 (both required on `$PATH`).

---

## File Structure

### Created files

| Path | Purpose |
|---|---|
| `crates/spur-mcp/src/plan/audit_sentinel.rs` | `[[spur-audit v1]]` encoder + parser, `AuditSentinelKind` enum (renamed from `AuditEntryType`) |
| `crates/spur-mcp/tests/audit_sentinel_round_trip.rs` | Live-`br` integration test: emit + read back audit sentinel comments |
| `crates/spur-mcp/tests/reconciler_tick.rs` | Integration test for reconciler bv-primary observation |
| `crates/spur-pm/tests/cursor_boundary_safe.rs` | Regression test for F1 boundary replay |

### Modified files

| Path | Change |
|---|---|
| `crates/spur-pm/src/advanced.rs` | Delete `AuditEntryType`, `AuditRecordInput`, `AuditEntry`, `AuditId`, `audit_record`, `audit_log` from `BeadsAdvanced` trait. Move `AuditSentinelKind` to `spur-mcp/plan/audit_sentinel.rs` (SPUR-internal, not a PM contract) |
| `crates/spur-pm/src/beads.rs` | Delete `audit_record`/`audit_log` impls. F1 boundary-safe cursor refactor. |
| `crates/spur-pm/src/lib.rs` | Drop the deleted re-exports |
| `crates/spur-pm/src/bv.rs` | New `robot_triage()` method wrapping `bv --robot-triage --json` |
| `crates/spur-mcp/src/server.rs` | Correlation backfill; `spur:plan-complete` gate emission; comment-sentinel audit emission on submit path |
| `crates/spur-mcp/src/plan/mod.rs` | Remove `spur:task-text:` label path (rely on `description`); comment-sentinel audit emission on dispatch/completion/approval/rejection; populate `PlanTaskEntry.spec.issue_id` from submit path |
| `crates/spur-mcp/src/plan/reconciler.rs` | bv-primary tick_once with br-ready fallback, filter on `spur:plan-complete` |
| `crates/spur-mcp/src/plan/labels.rs` | Add `PLAN_COMPLETE_MARKER` const + `plan_complete()` constructor |
| `crates/spur-mcp/tests/beads_advanced.rs` | Remove `audit_record_carries_actor_when_set` `#[ignore]`d test (superseded by Task 3's sentinel round-trip) |

### Preflight

- **No in-flight plans.** v0a.2 changes the persistence format (adds `spur:plan-complete` marker). Existing ephemeral plans continue to work; previously-persisted plans without the marker will NOT be observed by the reconciler.
- **bv v0.15.2+ required.** SPUR users must have `bv` installed; the reconciler is bv-primary by design. `br ready` fallback exists for edge cases (bv unresponsive), not for bv-absent.
- **br 0.1.14 required.** Unchanged from v0a.1.

---

## Task 1: Correlation backfill — task_map → PlanTaskEntry.spec.issue_id

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (`handle_submit_plan` persist path)
- Modify: `crates/spur-mcp/src/plan/mod.rs` (expose the right mutation API on `PlanState` if not already public)

**Context:** Today `submit_plan` with `persist_as_epic=true` calls `build_epic_subgraph` which returns `EpicSubgraph { epic_id, task_map: HashMap<String, String> }` where keys are `PlanTask.task_id` and values are the created beads child IDs. But after persist, `PlanState.tasks[i].spec.issue_id` remains `None` — the runtime state never learns the beads IDs. Any audit emission (Tasks 5, 6) needs those IDs to target the right issue.

**Steps:**

- [ ] Write test: after `submit_plan` with `persist_as_epic=true`, verify each `PlanTaskEntry.spec.issue_id` is `Some(<beads-id>)` matching `task_map[task_id]`.
- [ ] In `server.rs` `handle_submit_plan` after `build_epic_subgraph` returns `Ok(EpicSubgraph { epic_id, task_map })`: iterate `plan_state.tasks` and for each entry, if `task_map.contains_key(&entry.spec.task_id)` set `entry.spec.issue_id = Some(task_map[&entry.spec.task_id].clone())`. Also set `plan_state.epic_id = Some(epic_id.clone())` (previously initialized `None` per review addendum).
- [ ] Ensure mutation happens before the `PlanState` is stored/shared — do this inside the same critical section where the state is first constructed.
- [ ] Verify `cargo test -p spur-mcp --test submit_plan_persist` still passes (pure-helper tests unaffected) and the new integration test passes.

**Commit:** `fix(spur-mcp): backfill task_map beads IDs into PlanState after persist`

---

## Task 2: Partial-subgraph gate — `spur:plan-complete` marker

**Files:**
- Modify: `crates/spur-mcp/src/plan/labels.rs` (add constructor)
- Modify: `crates/spur-mcp/src/server.rs` (`build_epic_subgraph`)
- Modify: `crates/spur-mcp/tests/submit_plan_persist.rs` (assertion)

**Context:** `build_epic_subgraph` is not transactional — partial state can land (epic + some children) on mid-creation failure. The reconciler must not treat partially-persisted plans as observable work. Solution: after all children + dependencies are successfully created, add a `spur:plan-complete` label to the epic. Reconciler queries filter on it.

**Steps:**

- [ ] Test: `build_epic_subgraph` success path results in epic carrying `spur:plan-complete` label; failure mid-creation leaves epic WITHOUT the label (test by injecting a failure via a second plan referencing a non-existent dep).
- [ ] Add to `crates/spur-mcp/src/plan/labels.rs`:
  ```rust
  pub const PLAN_COMPLETE: &str = "spur:plan-complete";
  ```
  (Constant, not a constructor — no parameter.) Add to the `is_br_legal` test.
- [ ] In `server.rs` `build_epic_subgraph`: after the `for (task_id, mut child_create) in child_specs` loop succeeds, call `pm.add_label(&epic_id, crate::plan::labels::PLAN_COMPLETE)` (or whatever the existing `IssueTracker::add_label` method is). If add_label doesn't exist on `IssueTracker`, add it (`br label add <issue> -l <label>`).
- [ ] Commit: `feat(spur-mcp): spur:plan-complete marker gates reconciler from partial subgraphs`

**Note:** Reconciler filter on this label lands in Task 9.

---

## Task 3: `[[spur-audit v1]]` sentinel parser + encoder

**Files:**
- Create: `crates/spur-mcp/src/plan/audit_sentinel.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs` (register `pub mod audit_sentinel;`)
- Create: `crates/spur-mcp/tests/audit_sentinel_round_trip.rs`

**Context:** v0a.1 shipped `plan/signals.rs` with `[[spur-signal v1]]\n<json>` sentinel format for worker → brain signals. v0a.2 extends this pattern for brain → audit-log trail. Comment body is the transport (verified: `br comments add/list --json` round-trips verbatim text including newlines).

**Steps:**

- [ ] Create `audit_sentinel.rs` with:
  ```rust
  //! [[spur-audit v1]] sentinel comment encoder/parser — extends the
  //! plan/signals.rs pattern for audit breadcrumbs. `br audit record` is
  //! empirically unsuitable as transport; see plan v0a.md Review addendum II.

  use serde::{Deserialize, Serialize};

  pub const SENTINEL_PREFIX: &str = "[[spur-audit v1]]";

  #[non_exhaustive]
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(tag = "kind", rename_all = "kebab-case")]
  pub enum AuditSentinelKind {
      PlanSubmit { plan_id: String, epic_issue_id: String, task_ids: Vec<String> },
      Dispatch { delegation_id: String, worker: String, attempt: u32 },
      Completion { delegation_id: String, worker_branch: Option<String>, diff_summary: Option<String> },
      Approval { delegation_id: String },
      Rejection { delegation_id: String, feedback: String },
  }

  pub fn encode_comment(kind: &AuditSentinelKind) -> String {
      let json = serde_json::to_string(kind).expect("AuditSentinelKind always serializes");
      format!("{SENTINEL_PREFIX}\n{json}")
  }

  pub fn parse_comment(body: &str) -> Option<Result<AuditSentinelKind, ParseError>> {
      let rest = body.trim_start().strip_prefix(SENTINEL_PREFIX)?;
      Some(serde_json::from_str(rest.trim_start()).map_err(ParseError::Json))
  }

  #[derive(Debug, thiserror::Error)]
  pub enum ParseError {
      #[error("sentinel JSON parse error: {0}")]
      Json(#[from] serde_json::Error),
  }
  ```
- [ ] Unit tests (inline `#[cfg(test)]`): round-trip every variant, non-sentinel rejection, malformed-JSON error path, leading-whitespace tolerance. Mirror `plan/signals.rs::tests`.
- [ ] Create `crates/spur-mcp/tests/audit_sentinel_round_trip.rs`: live-`br` integration test that writes one comment per variant via `br comments add`, reads back via `br comments list --json`, parses each through `parse_comment`, asserts round-trip equality.
- [ ] Register module in `plan/mod.rs`: `pub mod audit_sentinel;` alongside the existing `pub mod labels; pub mod signals;`.
- [ ] Commit: `feat(spur-mcp): [[spur-audit v1]] sentinel parser + encoder`

---

## Task 4: Drop `audit_record`/`audit_log` from BeadsAdvanced

**Files:**
- Modify: `crates/spur-pm/src/advanced.rs` (delete types + trait methods)
- Modify: `crates/spur-pm/src/beads.rs` (delete impls)
- Modify: `crates/spur-pm/src/lib.rs` (drop re-exports)
- Modify: `crates/spur-pm/tests/beads_advanced.rs` (delete `audit_record_carries_actor_when_set` — was `#[ignore]`d, now superseded)

**Context:** `br audit record` is empirically DOA as transport (`data` dropped on persist, no read-back CLI, undocumented in author AGENTS.md). The audit_record/audit_log methods on BeadsAdvanced are stubs that return `anyhow::bail!`. Delete them entirely. `AuditSentinelKind` lives in `spur-mcp/src/plan/audit_sentinel.rs` as SPUR-internal (not a PM adapter contract) — this was decided in Task 3.

**Steps:**

- [ ] From `advanced.rs`:
  - Remove `AuditRecordInput`, `AuditEntryType`, `AuditEntry`, `AuditId` types.
  - Remove `audit_record`, `audit_log` methods from the `BeadsAdvanced` trait.
- [ ] From `beads.rs`: remove the two stub impls. Remove the `use crate::advanced::{AuditEntry, AuditId, AuditRecordInput, ...}` line's deleted items; keep `BeadsAdvanced, Comment, CommentId, DependencyCycle, ReadyFilter`.
- [ ] From `lib.rs`: drop `AuditEntry, AuditEntryType, AuditId, AuditRecordInput` from re-exports.
- [ ] Delete the `#[ignore]`d `audit_record_carries_actor_when_set` test from `crates/spur-pm/tests/beads_advanced.rs`.
- [ ] Remove `use spur_pm::{AuditEntryType, AuditRecordInput}` imports in tests.
- [ ] Run `cargo test -p spur-pm` — expect clean.
- [ ] Run `cargo build -p spur-mcp` — may fail if any spur-mcp code consumed the deleted types. Audit and remove.
- [ ] Commit: `refactor(spur-pm): drop BeadsAdvanced audit_record/audit_log (transport moved to comments)`

---

## Task 5: Submit-path audit emission

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (`handle_submit_plan` — after epic+children+marker success)
- Modify: `crates/spur-mcp/tests/submit_plan_persist.rs` OR new `tests/submit_plan_audit.rs` — live-br assertion

**Context:** After a plan is persisted + `spur:plan-complete` marker added, emit one audit comment on the epic carrying the plan-submit payload. Uses the `BeadsAdvanced::add_comment` method (already shipped in v0a.1).

**Steps:**

- [ ] Test: after `submit_plan { persist_as_epic: true, ... }`, call `br comments list <epic_id> --json` and assert at least one comment matches `parse_comment` → `AuditSentinelKind::PlanSubmit { plan_id, epic_issue_id, task_ids }` with the expected values.
- [ ] In `server.rs` after the marker is set successfully:
  ```rust
  if let Some(adv) = self.pm.advanced() {
      let kind = crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
          plan_id: plan_id.to_string(),
          epic_issue_id: epic_id.clone(),
          task_ids: task_map.values().cloned().collect(),
      };
      let body = crate::plan::audit_sentinel::encode_comment(&kind);
      if let Err(e) = adv.add_comment(&epic_id, &body).await {
          tracing::warn!(%epic_id, "audit PlanSubmit comment emission failed: {e}");
      }
  }
  ```
  (Emission is advisory — failure logs, doesn't abort the plan.)
- [ ] Run integration test against real `br`.
- [ ] Commit: `feat(spur-mcp): emit PlanSubmit audit comment on persisted plans`

---

## Task 6: Lifecycle audit emission — dispatch/completion/approval/rejection

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs` (dispatch path where `PlanTaskStatus::Dispatched` is assigned; completion path where `AwaitingReview` is assigned)
- Modify: `crates/spur-mcp/src/server.rs` (`handle_review_task` approve + request-changes branches)
- New test: `crates/spur-mcp/tests/plan_audit_coverage.rs`

**Context:** Same pattern as Task 5, but at task-lifecycle transition points. Must use `PlanTaskEntry.spec.issue_id` (now populated by Task 1).

**Steps:**

- [ ] Test: drive a plan through submit → dispatch → completion → approve; `br comments list <task_issue_id> --json` returns 3 comments parsing to `Dispatch`, `Completion`, `Approval` in order; `br comments list <epic_id>` returns at least the PlanSubmit one.
- [ ] Locate the dispatch-transition site via `grep -n "PlanTaskStatus::Dispatched" crates/spur-mcp/src/plan/mod.rs`. Right after the state transition, emit `Dispatch` sentinel comment on `entry.spec.issue_id.as_ref()` (skip if `None` — ephemeral plan, not persisted).
- [ ] Locate the completion-transition site (`PlanTaskStatus::AwaitingReview`). Emit `Completion` sentinel. Include `worker_branch` + `diff_summary` from the collector result.
- [ ] In `handle_review_task` approve branch, emit `Approval`. In request-changes branch, emit `Rejection { feedback }`.
- [ ] All emissions advisory — log and swallow errors.
- [ ] Ensure the PM accessor is reachable from each site (may need to thread `pm: Arc<PmService>` or use the existing accessor pattern).
- [ ] Commit: `feat(spur-mcp): emit Dispatch/Completion/Approval/Rejection audit comments`

---

## Task 7: `spur:task-text:` label → `description` field migration

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs` (remove the label-based task_text fallback)
- Modify: `crates/spur-mcp/src/plan/mod.rs` tests that asserted the label path

**Context:** The `spur:task-text:<text>` label key was migrated in v0a.1 but the VALUE may contain `.`, `=`, whitespace — all illegal as label chars. Any real task-text-as-label write would fail. The issue `description` field is the correct home for task text.

**Steps:**

- [ ] In `plan/mod.rs` `derive_epic_plan` (around line 227 per the comment), remove the label-based task_text fallback. Replace with: `task_text = child.description.clone().unwrap_or_default();`. Update the doc-comment.
- [ ] Remove the `spur:task-text:` constructor helper from `plan/labels.rs` if one exists. (There isn't one per the current labels.rs — only a raw prefix string in plan/mod.rs. Confirm and remove any remaining references.)
- [ ] Update tests in plan/mod.rs that use `spur:task-text:` labels in their fixtures — replace with `description` field on the fixture.
- [ ] Commit: `refactor(spur-mcp): drop spur:task-text: label path, rely on issue description`

---

## Task 8: `BvAdapter::robot_triage()` surface

**Files:**
- Modify: `crates/spur-pm/src/bv.rs` (add method)
- Modify: `crates/spur-pm/src/lib.rs` if new types need re-exporting
- New test: add to `crates/spur-pm/tests/` a `bv_triage.rs` live-bv integration test

**Context:** `bv v0.15.2` installed; `bv --robot-triage --json` returns quick_ref, recommendations, quick_wins, blockers_to_clear, project_health, commands. The reconciler (Task 9) needs to read the `recommendations` list (ordered ready-work). Expose a typed method on `BvAdapter`.

**Steps:**

- [ ] Audit existing `crates/spur-pm/src/bv.rs` to understand the current `BvAdapter` shape (how it invokes `bv`, how it handles JSON output, what methods it exposes today).
- [ ] Decide the minimum JSON fields SPUR cares about from `bv --robot-triage`. Candidate:
  ```rust
  pub struct TriageRecommendation {
      pub id: String,             // beads issue ID
      pub score: f64,             // recommendation score
      pub priority: i32,
      pub title: String,
      #[serde(default)]
      pub reasons: Vec<String>,
  }
  pub struct TriageOutput {
      pub recommendations: Vec<TriageRecommendation>,
      // skip other fields for v0a.2
  }
  ```
  Serde `#[serde(rename_all = "snake_case")]` + `#[serde(deny_unknown_fields = false)]` — tolerate bv adding fields.
- [ ] Add method:
  ```rust
  pub async fn robot_triage(&self, plan_id_filter: Option<&str>) -> anyhow::Result<TriageOutput> {
      let mut args = vec!["--robot-triage".into()];
      if let Some(pid) = plan_id_filter {
          args.push("--label".into());
          args.push(format!("spur:plan-id:{pid}"));
      }
      let output = self.run_bv(args).await?;
      serde_json::from_str(&output).map_err(|e| anyhow::anyhow!("parse bv triage: {e}\nraw: {output}"))
  }
  ```
  (Adjust flag name `--label` to whatever bv actually accepts. Confirm via `bv --help | grep label`.)
- [ ] Integration test: create a small beads workspace with 2 open tasks, invoke `BvAdapter::robot_triage(None)`, assert `recommendations.len() >= 1` and each has a valid `id`.
- [ ] Commit: `feat(spur-pm): BvAdapter::robot_triage() wrapping bv --robot-triage`

---

## Task 9: Reconciler bv-primary tick_once + integration test

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs` (replace `tick_once` stub from v0a.1)
- Create: `crates/spur-mcp/tests/reconciler_tick.rs`

**Context:** v0a.1 Task 15 shipped a reconciler skeleton with `tick_once` stubbed to return `Ok(false)`. Now replace it with a real implementation that:
1. Calls `bv --robot-triage` via `BvAdapter::robot_triage()` (Task 8). bv is a hard dependency.
2. Filters returned recommendations to issues under plans labeled `spur:plan-complete` (Task 2).
3. Logs observations (v0a scope is observation only — no dispatch).
4. Falls back to `br ready -l spur:plan-complete -l spur:plan-id:<id>` if bv call fails (observability shouldn't depend on bv health).

**Steps:**

- [ ] Update `Reconciler` constructor to accept `Arc<PmService>` (or `Option<Arc<BvAdapter>>` + `Arc<dyn BeadsAdvanced>`) so tick_once can access both bv and br.
- [ ] Replace `tick_once`:
  ```rust
  async fn tick_once(&self) -> anyhow::Result<bool> {
      let triage = match self.analyzer.as_ref() {
          Some(bv) => bv.robot_triage(self.plan_id.as_deref()).await,
          None => anyhow::bail!("reconciler requires BvAdapter (bv binary) — none configured"),
      };
      let recommendations = match triage {
          Ok(t) => t.recommendations,
          Err(e) => {
              tracing::warn!("bv triage failed, falling back to br ready: {e}");
              return self.tick_fallback_br_ready().await;
          }
      };

      // Filter to plan-complete-gated issues only — requires cross-referencing.
      // For v0a.2, observe every recommendation and log.
      for rec in &recommendations {
          tracing::debug!(issue_id = %rec.id, score = rec.score, "reconciler observed ready task");
      }
      Ok(!recommendations.is_empty())
  }

  async fn tick_fallback_br_ready(&self) -> anyhow::Result<bool> {
      let filter = spur_pm::ReadyFilter {
          labels_all: {
              let mut v = vec![crate::plan::labels::PLAN_COMPLETE.to_string()];
              if let Some(pid) = &self.plan_id {
                  v.push(crate::plan::labels::plan_id(pid));
              }
              v
          },
          limit: Some(50),
          ..Default::default()
      };
      let summaries = self.advanced.list_ready(filter).await?;
      for s in &summaries {
          tracing::debug!(issue_id = %s.id, "reconciler observed ready task (br fallback)");
      }
      Ok(!summaries.is_empty())
  }
  ```
- [ ] Update `ready_for_test()` helper to expose the fallback path for tests. Consider exposing both `bv_ready_for_test()` and `br_ready_for_test()` for explicit test coverage.
- [ ] Integration test at `crates/spur-mcp/tests/reconciler_tick.rs`:
  - Create temp workspace, init br, create an epic + 2 tasks (A blocks B), label with `spur:plan-id:P1`, add `spur:plan-complete` to epic.
  - Exercise `br_ready_for_test()` — assert returns `[A]` only.
  - Exercise `bv_ready_for_test()` — assert returns A (priorities/scores from bv).
- [ ] Verify: `cargo test -p spur-mcp --test reconciler_tick` passes. `cargo test -p spur-mcp` clean.
- [ ] Commit: `feat(spur-mcp): reconciler tick_once with bv-primary + br-ready fallback`

---

## Task 10: F1 boundary-safe cursor

**Files:**
- Modify: `crates/spur-pm/src/beads.rs` (cursor type + poll + save/load helpers)
- Create: `crates/spur-pm/tests/cursor_boundary_safe.rs`

**Context:** v0a.0 plan Task 8's proposed rewrite used `updated_at >= cursor` + advance to `max(updated_at)`, which replays boundary rows forever. Replace with `(ts, ids_at_boundary)` representation:
- Cursor is `Option<(DateTime<Utc>, HashSet<String>)>`.
- Poll filter: `item.updated_at > cursor.ts || (item.updated_at == cursor.ts && !cursor.ids.contains(&item.id))`.
- After poll: new `ts = max(kept.map(updated_at))`, `ids = all kept ids whose updated_at == ts`.

**Steps:**

- [ ] Test: two issues created at the same timestamp (ms-precision collision). First poll returns both, cursor records their IDs at that ts. Second poll with no intervening writes returns empty (no replay). Third write at same ts returns exactly the third issue (ids_at_boundary discriminates).
- [ ] Struct change: `last_poll: Mutex<Option<PollCursor>>` where:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, Default)]
  pub struct PollCursor {
      pub ts: DateTime<Utc>,
      pub ids_at_boundary: HashSet<String>,
  }
  ```
- [ ] Refactor `poll()`:
  ```rust
  // Filter
  let kept: Vec<BrIssueWithCounts> = items.into_iter().filter(|it| match &last_poll {
      Some(c) => it.updated_at > c.ts || (it.updated_at == c.ts && !c.ids_at_boundary.contains(&it.id)),
      None => true,
  }).collect();
  // Advance cursor
  let new_cursor = if let Some(max_ts) = kept.iter().map(|i| i.updated_at).max() {
      let ids_at = kept.iter().filter(|i| i.updated_at == max_ts).map(|i| i.id.clone()).collect();
      Some(PollCursor { ts: max_ts, ids_at_boundary: ids_at })
  } else {
      last_poll.clone()  // empty poll — don't advance
  };
  ```
- [ ] Change disk format to JSON (serde PollCursor). Update `load_cursor` / `save_cursor` accordingly.
- [ ] Update existing `disk_cursor_survives_adapter_restart` test if its assertions depend on the old cursor shape.
- [ ] Run `cargo test -p spur-pm --test poll_cursor --test cursor_boundary_safe` — both pass.
- [ ] Commit: `fix(spur-pm): F1 boundary-safe poll cursor (ts, ids_at_boundary)`

---

## Final verification

- [ ] `cargo clippy -p spur-pm -p spur-mcp --all-targets -- -D warnings` — clean.
- [ ] `cargo test -p spur-pm -p spur-mcp` — all green (no ignored tests in spur-pm).
- [ ] `cargo fmt --all -- --check` — no diff.
- [ ] Manual smoke: submit a persisted plan with 2 tasks, drive through dispatch → completion → approve, inspect `br comments list <epic_id>` and `br comments list <task_id>` for the four expected sentinel comments.

---

## Self-review

- ✅ Task 4 audit transport redesigned around comment breadcrumbs (Tasks 3, 5, 6)
- ✅ Task 8 from v0a.0 — F1 boundary-safe cursor — addressed by Task 10
- ✅ Reconciler bv-primary — Tasks 8 + 9
- ✅ Correlation backfill — Task 1
- ✅ Partial-subgraph gate — Task 2
- ✅ `spur:task-text:` migration — Task 7
- Deferred to v0b: adaptive mutation itself; `superseded-by` label redesign (one label per superseder, not needed in v0a)
