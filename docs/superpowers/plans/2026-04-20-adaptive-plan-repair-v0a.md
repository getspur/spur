# Adaptive Plan Repair — v0a (Beads-Native Plan Execution) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `spur-pm::BeadsAdapter` with the missing `br` CLI primitives (`ready`, `comments`, `audit`, `dep cycles`, `dep remove`); thread `--actor` attribution through every beads call; fix two `poll()` bugs; wire `br audit` breadcrumbs into the existing plan executor; add a reconciler that uses `br ready` for beads-backed observation/parity checks. **v0a does not transfer dispatch authority from the current in-memory executor to the reconciler yet.**

**Architecture:** v0a is the infrastructure layer for the adaptive-plan-repair design (see `docs/superpowers/specs/2026-04-20-adaptive-plan-repair-design.md`). It ships as three layers:
- **Layer α** — `BeadsAdvanced` trait + impl on `BeadsAdapter` (adapter extensions, actor, cursor fixes)
- **Layer β** — conventions (label vocabulary module, comment sentinel parser)
- **Layer γ** — reconciler tick (observation/parity only in v0a) + audit-record emission across existing plan-executor paths

No new user-visible features in v0a (adaptive mutation itself is v0b). The standalone value is observability (`br audit` trails across plan executions), actor attribution, and correctness (cursor bugs fixed).

**Tech Stack:** Rust 2024 edition, `tokio`, `async-trait`, `serde`, `serde_json`, `chrono`, `uuid`, `anyhow`, `thiserror`, `tracing`. Beads CLI (`br`) v1.x with SQLite WAL mode. Tests use `tokio::test` and shell out to real `br` in a temp `.beads/` directory.

---

## File Structure

### Created files

| Path | Purpose |
|---|---|
| `crates/spur-pm/src/advanced.rs` | `BeadsAdvanced` trait + shared types (`ReadyFilter`, `Comment`, `AuditEntry`, `AuditEntryType`, `DependencyCycle`, newtypes) |
| `crates/spur-pm/tests/beads_advanced.rs` | Integration tests for all `BeadsAdvanced` methods — shells out to real `br` in a temp workspace |
| `crates/spur-pm/tests/poll_cursor.rs` | Regression tests for cursor race (F1) and disk-backed cursor (F2) |
| `crates/spur-mcp/src/plan/labels.rs` | Label vocabulary — constants + typed helpers |
| `crates/spur-mcp/src/plan/signals.rs` | Sentinel comment parser (`[[spur-signal v1]]`); types for v0b consumption |
| `crates/spur-mcp/src/plan/audit.rs` | Helpers that translate plan-executor events into `BeadsAdvanced::audit_record` calls |
| `crates/spur-mcp/src/plan/reconciler.rs` | `tokio::spawn`'d reconciler task with adaptive pull cadence |
| `crates/spur-mcp/tests/reconciler_tick.rs` | Integration test for reconciler `br ready` observation/parity filtering |

### Modified files

| Path | Change |
|---|---|
| `crates/spur-pm/Cargo.toml` | Add `uuid` workspace dep |
| `crates/spur-pm/src/lib.rs` | `pub mod advanced;` and re-export `BeadsAdvanced` |
| `crates/spur-pm/src/adapter.rs` | (no change — `BeadsAdvanced` lives in its own module to keep the cross-backend `IssueTracker` surface clean) |
| `crates/spur-pm/src/beads.rs` | Impl `BeadsAdvanced`; add `default_actor` + `cursor_path` fields; `connect_with_actor` ctor; thread `--actor` in `run_br_as`; fix `poll()` cursor (F1 `max(updated_at)`, F2 optional disk backing) |
| `crates/spur-pm/src/service.rs` | Expose `PmService::advanced() -> Option<&dyn BeadsAdvanced>` (pattern from `analyzer()`) |
| `crates/spur-mcp/src/plan.rs` (or a new `plan/mod.rs` if the file is split) | Register the three new submodules; spawn reconciler on server startup |
| `crates/spur-mcp/src/server.rs:1734` `handle_submit_plan` + completion handlers | Call `BeadsAdvanced::audit_record` on plan-submit, dispatch, completion, approval, rejection |

### Test strategy

- **Unit tests** (inline `#[cfg(test)] mod tests`): pure Rust logic — parsing, cursor math, label helpers, sentinel parser.
- **Integration tests** (`crates/spur-pm/tests/` and `crates/spur-mcp/tests/`): shell out to real `br` binary. Each test uses `tempfile::TempDir`, runs `br init`, exercises the path, asserts via another `br` call or direct DB inspection. The `br` binary is required on `$PATH`; tests are gated with a helper `require_br_binary()` that calls `which br` and skips if absent.

### Preflight constraints

- **Dispatch authority stays in `plan.rs` for v0a.** `run_plan()` / `dispatch_newly_ready()` remain the only dispatchers in this phase. The reconciler may observe, log, and parity-check ready tasks, but it MUST NOT enqueue ACP work independently in v0a.
- **Beads-backed scope only.** Any `br ready`-based logic in v0a applies only to plans whose tasks already exist as beads issues (`persist_as_epic=true`, `execute_epic`, or equivalent persisted subgraph). Ephemeral `submit_plan` calls with `issue_id: None` remain outside beads authority in v0a.
- **Pinned CLI surface.** The locally verified `br` version for this plan is `0.1.14` (`br version --json` on 2026-04-20). If CLI flags or JSON output differ from that version family, stop and update the wrappers/tests before implementation.
- **Single-owner cursor files only.** The disk-backed cursor from Task 9 is for restart recovery of one SPUR process. Do not share one cursor file across multiple concurrent SPUR instances in v0a; cross-process locking/claiming is out of scope here.

### Review addendum (2026-04-20)

- **Audit transport is blocked as written.** On `br 0.1.14`, `br audit record --help` exposes `--kind` and `--issue-id`, not positional issue ID / `--type` / `--data`. `br audit log <ID> --json` also does not match the custom `AuditEntry` shape assumed in Tasks 4 and 12–14. Do not execute those tasks as written. First choose and verify one transport:
  1. SPUR-owned breadcrumb comments / labels, or
  2. a proven `br audit record --stdin` schema with round-trip tests in a temp beads workspace, or
  3. an upstream `br` CLI change that adds the required typed record contract.
- **F1 cursor fix is blocked as written.** The Task 8 rewrite still filters on an inclusive boundary (`updated_at >= cursor`) while advancing the cursor to `max(updated_at)`. That can replay boundary rows forever. Before implementing F1, upgrade the cursor design to a boundary-safe representation such as `(updated_at, issue_ids_at_boundary)` and use that same representation for the disk-backed cursor in Task 9.
- **Label namespace must stay grounded in current code.** Persisted plans today use `spur.plan_id=<id>` / `spur.plan_task_id=<id>` / `spur.agent=<name>`. Do not introduce `plan-epic:` / `plan-task:` as the authoritative runtime namespace in v0a unless the plan also includes a concrete migration or dual-read/dual-write strategy across `build_epic_subgraph`, `derive_epic_plan`, `execute_epic`, and any reconciler filters.
- **Correlation is missing for persisted plans.** Current `submit_plan` persists a `task_map` but does not backfill those beads child IDs into `PlanState.tasks[*].spec.issue_id`, and current `execute_epic` still initializes `PlanState.epic_id` as `None`. Before any audit or reconciler work, add a correlation step so the runtime state carries the actual beads IDs it intends to reference.
- **Do not let the reconciler see partial subgraphs by default.** `build_epic_subgraph` explicitly allows partial beads state on failure. Until v0a defines a completion marker or registry gate for fully persisted plans, the reconciler must not scan all candidate labels and treat every matching task as dispatchable/observable work.

### Review addendum II (2026-04-20, post-v0a.1 empirical verification)

Three rounds of empirical probing against real `br 0.1.14` (help output + temp-workspace round-trips + `br schema all --format json` + the author's AGENTS.md in `Dicklesworthstone/beads_rust`) changed several assumptions. v0a.1 ships with these assumption corrections already merged on `main` (see commits `841945a`, `d97ecf1`, `1f6fbbc`). The remaining items below are v0a.2 inputs.

- **B5 — label vocabulary was incompatible with `br` grammar** (resolved in `841945a`). `br 0.1.14` enforces labels `[A-Za-z0-9_:-]+`; the v0a.0 namespace `spur.plan_id=<id>` / `spur.plan_task_id=<id>` / `spur.agent=<name>` / `spur.source_issue=<id>` contained illegal `.` and `=`. Migrated to `spur:plan-id:<id>`, `spur:plan-task-id:<id>`, `spur:agent:<name>`, `spur:source-issue:<id>`. New integration test `crates/spur-mcp/tests/labels_br_round_trip.rs` round-trips every constructor through real `br label add`. This supersedes the "Label namespace must stay grounded in current code" item above — the pre-existing namespace was never functional against `br 0.1.14`.

- **B1 + B2 — `ReadyFilter` priority semantics** (resolved in `d97ecf1`). `br ready -p <n>` is empirically a repeatable set-membership filter (`-p 0 -p 2` returns P0 ∪ P2), not a range and not exact-match-single. Prior `priority_min`/`priority_max` misrepresented br's model and silently dropped `priority_max`. Replaced with `priorities: Vec<i32>` that bijectively maps to `br`'s flag surface.

- **D1 — `poll()` mutex across `fs::write`** (resolved in `1f6fbbc`). `std::sync::Mutex` was held across synchronous `save_cursor` fs::write in an async context. Released before I/O.

- **Task 4 audit transport — DOA, not gated.** The blocker above offered three transport options; empirical testing on `br 0.1.14`:
  - Option 2 (`br audit record --stdin`) is **structurally broken**: the `data` field in the stdin JSON is silently dropped on persist (`.beads/interactions.jsonl` stores only `{id, kind, created_at, actor, issue_id}`), AND there is no CLI to query interactions (`br audit log` returns issue events, not interactions). The author's AGENTS.md does not document `br audit record` as an agent contract. No `AuditEntry` schema in `br schema all` (9 public schemas only: `BlockedIssue, ErrorEnvelope, Issue, IssueDetails, IssueWithCounts, ReadyIssue, StaleIssue, Statistics, TreeNode`).
  - Option 3 (upstream br change) is out of scope.
  - **Option 1 (SPUR-owned comment breadcrumbs) is the only viable path.** Verified end-to-end: `br comments add <issue> <body> --actor <actor> --json` preserves the full body text verbatim including embedded newlines and JSON; `br comments list <issue> --json` round-trips cleanly. Extends Task 11's `[[spur-signal v1]]` pattern to `[[spur-audit v1]]`.
  - **Redraft Tasks 4, 12, 13, 14** around comment-sentinel transport. `BeadsAdvanced::audit_record` / `audit_log` should be **deleted from the trait** (or repurposed as fire-and-forget stubs for the unindexed `interactions.jsonl` side channel). The primary breadcrumb API becomes `add_comment` + a new `plan/audit_sentinel.rs` parser that reuses the sentinel parser pattern from `plan/signals.rs`.

- **B4 — Task 8 proposed status filter is invalid.** Plan Task 8's rewrite at line ~1270 uses `-s "open,in_progress,blocked"`. Empirically: `br list -s` is "can be repeated" (set membership, same pattern as `-p`), and comma-separated single arg is rejected with `INVALID_STATUS` (exit 4). Correct form for the Task 8 rewrite: `"-s", "open", "-s", "in_progress", "-s", "blocked"` (three argv pairs).

- **B3 — `br ready` JSON omits `labels`.** Per `br schema ready-issue --json`, the `ReadyIssue` type has no `labels` field. Our `BrReadyItem` has `#[serde(default)] labels: Vec<String>`, so `IssueSummary.labels` from `list_ready` is always empty. Server-side `-l <label>` filter works; reconciler **must not inspect returned labels** on `list_ready` results. Either tolerate server-side-only filtering, enrich each ID via a separate `br show <id>` call, or use `br list --json -l <label>` (which DOES return labels per the `IssueWithCounts` schema) and filter to "ready" in Rust.

- **Reconciler engine choice — use `bv` as primary, not raw `br ready`.** The author's AGENTS.md explicitly designates `bv --robot-triage` as the "single entry point" for agent pick-next-work workflows. `bv v0.15.2` outputs include `recommendations`, `quick_wins`, `blockers_to_clear`, `project_health`, `--robot-plan` (parallel execution tracks), `--robot-priority` (priority misalignment detection) — all directly modeling the observation/parity work Task 15-16 needs. SPUR already has a `BvAdapter` wired via `PmService::analyzer()`. Redesign the reconciler around `bv` primary with `br ready` fallback.

- **`br sync --flush-only` in agent workflow.** AGENTS.md prescribes `br sync --flush-only && git add .beads/` before session end. Default behavior auto-flushes (the `--no-auto-flush` flag is opt-out). The wrapper does not currently invoke explicit sync. For v0a.2 plan persistence across SPUR sessions, confirm reliance on auto-flush is intentional or add an explicit sync hook at persist boundaries.

- **`labels::superseded_by` was illegal (removed in `841945a`).** Comma-separated IDs violate br's label grammar. Unused at time of removal. v0b mutation work will need one label per superseder (e.g. iterate `for id in child_ids { br label add <parent> -l (format!("superseded-by:{id}")) }`) rather than a single multi-ID label.

- **`spur:task-text:<text>` label key is structurally misaligned.** Task text can contain `.`, `=`, whitespace — all illegal as label chars. The key migration landed in `841945a` but values remain problematic. Follow-up: migrate task text onto the issue `description` field (or a sentinel-framed comment), remove the `spur:task-text:` label path entirely.

- **Grammar documentation.** `br`'s label grammar `[A-Za-z0-9_:-]+` is enforced at runtime via `VALIDATION_FAILED` but undocumented in AGENTS.md. `plan/labels.rs` now encodes it in the `is_br_legal` test helper and module docstring. When adding label constructors, extend the constructor-emits-legal-labels test.

---

## Task 1: Scaffolding — create `advanced.rs` with types + trait skeleton

**Files:**
- Create: `crates/spur-pm/src/advanced.rs`
- Modify: `crates/spur-pm/src/lib.rs`
- Modify: `crates/spur-pm/Cargo.toml`

- [ ] **Step 1: Add `uuid` to `spur-pm/Cargo.toml` dependencies**

Insert in alphabetical order under `[dependencies]`:

```toml
uuid = { workspace = true, features = ["v4", "serde"] }
```

Verify `uuid` exists in workspace root `Cargo.toml`. If absent, add it to the workspace `[workspace.dependencies]` block first:

```toml
uuid = { version = "1.10", features = ["v4", "serde"] }
```

- [ ] **Step 2: Create `crates/spur-pm/src/advanced.rs` with full content**

```rust
//! Beads-only extension surface.
//!
//! These methods expose `br` CLI primitives that have no GitHub-backend
//! analog (ready, audit, comment CRUD, dep cycles). Only `BeadsAdapter`
//! implements this trait. Callers obtain a `&dyn BeadsAdvanced` from
//! `PmService::advanced()`, which returns `None` for non-beads backends.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::IssueSummary;

// ─── Filter & input types ─────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadyFilter {
    pub assignee: Option<String>,
    pub labels_all: Vec<String>,
    pub labels_any: Vec<String>,
    pub issue_type: Option<String>,
    pub priority_min: Option<i32>,
    pub priority_max: Option<i32>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecordInput {
    pub entry_type: AuditEntryType,
    pub data: serde_json::Value,
}

// ─── Closed vocabulary for audit entry types ──────────────────────────

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditEntryType {
    PlanSubmit,
    Dispatch,
    Completion,
    Approval,
    Rejection,
    Signal,
    MutationPlan,
    MutationCommit,
    MutationInvariantViolation,
    MutationCancelled,
    LateSignal,
    OrphanDepDetected,
}

// ─── Output types ─────────────────────────────────────────────────────

pub type AuditId = String;
pub type CommentId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: AuditId,
    pub issue_id: String,
    pub entry_type: AuditEntryType,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub body: String,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyCycle {
    /// Issue IDs forming the cycle, in dependency order.
    pub issues: Vec<String>,
}

// ─── Trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait BeadsAdvanced: Send + Sync {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>>;

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<Comment>>;

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId>;

    async fn audit_record(
        &self,
        issue_id: &str,
        entry: AuditRecordInput,
    ) -> anyhow::Result<AuditId>;

    async fn audit_log(&self, issue_id: &str) -> anyhow::Result<Vec<AuditEntry>>;

    async fn remove_dependency(
        &self,
        issue_id: &str,
        depends_on_id: &str,
    ) -> anyhow::Result<()>;

    async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>>;
}

// ─── Unit tests for type serialization ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_entry_type_serializes_kebab_case() {
        let t = AuditEntryType::MutationPlan;
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "\"mutation-plan\"");
    }

    #[test]
    fn audit_entry_type_round_trips() {
        for t in [
            AuditEntryType::PlanSubmit,
            AuditEntryType::Dispatch,
            AuditEntryType::Completion,
            AuditEntryType::Approval,
            AuditEntryType::Rejection,
            AuditEntryType::Signal,
            AuditEntryType::MutationPlan,
            AuditEntryType::MutationCommit,
            AuditEntryType::MutationInvariantViolation,
            AuditEntryType::MutationCancelled,
            AuditEntryType::LateSignal,
            AuditEntryType::OrphanDepDetected,
        ] {
            let s = serde_json::to_string(&t).unwrap();
            let back: AuditEntryType = serde_json::from_str(&s).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn ready_filter_default_is_empty() {
        let f = ReadyFilter::default();
        assert!(f.assignee.is_none());
        assert!(f.labels_all.is_empty());
        assert!(f.labels_any.is_empty());
        assert!(f.limit.is_none());
    }
}
```

- [ ] **Step 3: Modify `crates/spur-pm/src/lib.rs` to export the module**

Replace the existing content with:

```rust
pub mod adapter;
pub mod advanced;
pub mod beads;
pub mod bv;
pub mod github;
pub mod graph;
pub mod service;
pub mod types;

pub use adapter::{IssueTracker, PrService};
pub use advanced::{
    AuditEntry, AuditEntryType, AuditId, AuditRecordInput, BeadsAdvanced, Comment, CommentId,
    DependencyCycle, ReadyFilter,
};
pub use beads::BeadsAdapter;
pub use bv::BvAdapter;
pub use github::GitHubAdapter;
pub use service::PmService;
pub use types::*;
```

- [ ] **Step 4: Verify compilation + unit tests pass**

Run:

```bash
cargo test -p spur-pm --lib advanced::tests --no-fail-fast
```

Expected: 3 passed, 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/Cargo.toml crates/spur-pm/src/lib.rs crates/spur-pm/src/advanced.rs
git commit -m "feat(spur-pm): BeadsAdvanced trait skeleton + shared types"
```

---

## Task 2: Test harness for integration tests + `list_ready` method

**Files:**
- Create: `crates/spur-pm/tests/beads_advanced.rs`
- Modify: `crates/spur-pm/src/beads.rs`

- [ ] **Step 1: Write the integration test harness + failing test for `list_ready`**

Create `crates/spur-pm/tests/beads_advanced.rs`:

```rust
//! Integration tests for `BeadsAdvanced`. Each test spins up a temp `.beads/`
//! workspace and shells out to the real `br` binary. Tests auto-skip if `br`
//! is not installed.

use std::path::Path;
use std::process::Command;

use spur_pm::{BeadsAdapter, BeadsAdvanced, ReadyFilter};
use tempfile::TempDir;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .output()
        .expect("br invocation failed");
    assert!(out.status.success(), "br {:?} failed: {:?}", args, out);
    String::from_utf8(out.stdout).unwrap()
}

async fn setup_workspace() -> (TempDir, BeadsAdapter) {
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let adapter = BeadsAdapter::connect(dir.path())
        .await
        .expect("connect failed");
    (dir, adapter)
}

#[tokio::test]
async fn list_ready_returns_unblocked_issues() {
    if !br_available() {
        eprintln!("skipping: `br` binary not on PATH");
        return;
    }
    let (dir, adapter) = setup_workspace().await;

    // Create two tasks with a dependency: A blocks B. Only A is ready.
    let a = run_br(dir.path(), &["create", "Task A", "--silent", "-t", "task"])
        .trim()
        .to_string();
    let b = run_br(dir.path(), &["create", "Task B", "--silent", "-t", "task"])
        .trim()
        .to_string();
    // Wait a sec — `br create` returns just the ID with --silent.
    let a_id = a.trim_matches('"').to_string();
    let b_id = b.trim_matches('"').to_string();
    run_br(dir.path(), &["dep", "add", &b_id, &a_id]);

    let ready = adapter
        .list_ready(ReadyFilter {
            limit: Some(50),
            ..Default::default()
        })
        .await
        .unwrap();

    let ids: Vec<String> = ready.into_iter().map(|i| i.id).collect();
    assert!(ids.contains(&a_id), "expected A ({a_id}) in ready, got {ids:?}");
    assert!(!ids.contains(&b_id), "B ({b_id}) should be blocked");
}
```

Also ensure `tempfile` is a dev-dependency. Add to `crates/spur-pm/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Run test — it should fail with "no `list_ready` method"**

```bash
cargo test -p spur-pm --test beads_advanced list_ready_returns_unblocked_issues
```

Expected: compile error — `list_ready` not implemented on `BeadsAdapter`.

- [ ] **Step 3: Implement `BeadsAdvanced` on `BeadsAdapter` — `list_ready` first**

Append to `crates/spur-pm/src/beads.rs`, after the existing `impl IssueTracker for BeadsAdapter { ... }` block:

```rust
use crate::advanced::{
    AuditEntry, AuditEntryType, AuditId, AuditRecordInput, BeadsAdvanced, Comment, CommentId,
    DependencyCycle, ReadyFilter,
};

// ─── BeadsAdvanced impl ───────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct BrReadyItem {
    id: String,
    title: String,
    status: String,
    priority: i32,
    issue_type: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignee: Option<String>,
}

impl From<BrReadyItem> for IssueSummary {
    fn from(r: BrReadyItem) -> Self {
        Self {
            id: r.id.clone(),
            source: PmSource::Beads,
            title: r.title,
            status: r.status,
            labels: r.labels,
            url: format!("beads://{}", r.id),
            priority: Some(r.priority),
            issue_type: Some(r.issue_type),
            assignee: r.assignee,
        }
    }
}

#[async_trait]
impl BeadsAdvanced for BeadsAdapter {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
        let mut args: Vec<String> = vec!["ready".into()];

        if let Some(ref a) = filter.assignee {
            args.push("--assignee".into());
            args.push(a.clone());
        }
        for l in &filter.labels_all {
            args.push("-l".into());
            args.push(l.clone());
        }
        for l in &filter.labels_any {
            args.push("--label-any".into());
            args.push(l.clone());
        }
        if let Some(ref t) = filter.issue_type {
            args.push("-t".into());
            args.push(t.clone());
        }
        if let Some(p) = filter.priority_min {
            args.push("-p".into());
            args.push(p.to_string());
        }
        args.push("--limit".into());
        args.push(filter.limit.unwrap_or(20).to_string());

        let output = self.run_br(args).await?;
        let items: Vec<BrReadyItem> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("parse `br ready`: {e}\nraw: {output}"))?;
        Ok(items.into_iter().map(IssueSummary::from).collect())
    }

    async fn list_comments(&self, _issue_id: &str) -> anyhow::Result<Vec<Comment>> {
        anyhow::bail!("list_comments: not yet implemented")
    }

    async fn add_comment(&self, _issue_id: &str, _body: &str) -> anyhow::Result<CommentId> {
        anyhow::bail!("add_comment: not yet implemented")
    }

    async fn audit_record(
        &self,
        _issue_id: &str,
        _entry: AuditRecordInput,
    ) -> anyhow::Result<AuditId> {
        anyhow::bail!("audit_record: not yet implemented")
    }

    async fn audit_log(&self, _issue_id: &str) -> anyhow::Result<Vec<AuditEntry>> {
        anyhow::bail!("audit_log: not yet implemented")
    }

    async fn remove_dependency(
        &self,
        _issue_id: &str,
        _depends_on_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("remove_dependency: not yet implemented")
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
        anyhow::bail!("dep_cycles: not yet implemented")
    }
}
```

- [ ] **Step 4: Run the test again — it should pass**

```bash
cargo test -p spur-pm --test beads_advanced list_ready_returns_unblocked_issues
```

Expected: 1 passed (or skipped with "br not on PATH" message).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/Cargo.toml crates/spur-pm/tests/beads_advanced.rs crates/spur-pm/src/beads.rs
git commit -m "feat(spur-pm): BeadsAdvanced::list_ready wrapping \`br ready\`"
```

---

## Task 3: `list_comments` + `add_comment`

**Files:**
- Modify: `crates/spur-pm/src/beads.rs`
- Modify: `crates/spur-pm/tests/beads_advanced.rs`

- [ ] **Step 1: Append failing tests for comment CRUD**

Append to `crates/spur-pm/tests/beads_advanced.rs`:

```rust
#[tokio::test]
async fn add_comment_then_list_returns_it() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let (dir, adapter) = setup_workspace().await;
    let id_raw = run_br(dir.path(), &["create", "T", "--silent", "-t", "task"]);
    let id = id_raw.trim().trim_matches('"').to_string();

    let cid = adapter.add_comment(&id, "hello world").await.unwrap();
    assert!(!cid.is_empty(), "expected non-empty comment id");

    let comments = adapter.list_comments(&id).await.unwrap();
    assert!(
        comments.iter().any(|c| c.body.contains("hello world")),
        "expected comment with body 'hello world', got {comments:?}"
    );
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p spur-pm --test beads_advanced add_comment_then_list_returns_it
```

Expected: FAIL — `list_comments: not yet implemented`.

- [ ] **Step 3: Implement `list_comments` + `add_comment`**

In `crates/spur-pm/src/beads.rs`, replace the two `bail!` stubs with:

```rust
// Inside impl BeadsAdvanced for BeadsAdapter — replace the two stubs:

async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<Comment>> {
    #[derive(serde::Deserialize)]
    struct BrComment {
        id: String,
        body: String,
        #[serde(default)]
        actor: Option<String>,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let output = self
        .run_br(vec!["comments".into(), "list".into(), issue_id.into()])
        .await?;
    let items: Vec<BrComment> = serde_json::from_str(&output)
        .map_err(|e| anyhow::anyhow!("parse `br comments list`: {e}\nraw: {output}"))?;
    Ok(items
        .into_iter()
        .map(|c| Comment {
            id: c.id,
            body: c.body,
            actor: c.actor.unwrap_or_default(),
            created_at: c.created_at,
        })
        .collect())
}

async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId> {
    #[derive(serde::Deserialize)]
    struct BrCommentAdd {
        id: String,
    }
    let output = self
        .run_br(vec![
            "comments".into(),
            "add".into(),
            issue_id.into(),
            body.into(),
        ])
        .await?;
    // `br comments add --json` returns `{"id": "..."}` on success.
    let added: BrCommentAdd = serde_json::from_str(&output)
        .map_err(|e| anyhow::anyhow!("parse `br comments add`: {e}\nraw: {output}"))?;
    Ok(added.id)
}
```

- [ ] **Step 4: Run tests to confirm pass**

```bash
cargo test -p spur-pm --test beads_advanced
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/beads.rs crates/spur-pm/tests/beads_advanced.rs
git commit -m "feat(spur-pm): BeadsAdvanced comment CRUD wrappers"
```

---

## Task 4: `audit_record` + `audit_log` [BLOCKED AS WRITTEN]

**Files:**
- Modify: `crates/spur-pm/src/beads.rs`
- Modify: `crates/spur-pm/tests/beads_advanced.rs`

- [ ] **Step 1: Append failing tests**

Append to `crates/spur-pm/tests/beads_advanced.rs`:

```rust
use spur_pm::{AuditEntryType, AuditRecordInput};

#[tokio::test]
async fn audit_record_then_log_round_trips() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let (dir, adapter) = setup_workspace().await;
    let id = run_br(dir.path(), &["create", "T", "--silent", "-t", "task"])
        .trim()
        .trim_matches('"')
        .to_string();

    let input = AuditRecordInput {
        entry_type: AuditEntryType::PlanSubmit,
        data: serde_json::json!({"plan_id": "P1"}),
    };
    let audit_id = adapter.audit_record(&id, input).await.unwrap();
    assert!(!audit_id.is_empty());

    let log = adapter.audit_log(&id).await.unwrap();
    assert!(
        log.iter()
            .any(|e| e.entry_type == AuditEntryType::PlanSubmit),
        "expected PlanSubmit in log, got {log:?}"
    );
}
```

- [ ] **Step 2: Run tests to confirm failure**

```bash
cargo test -p spur-pm --test beads_advanced audit_record_then_log_round_trips
```

Expected: FAIL with "audit_record: not yet implemented".

- [ ] **Step 3: Implement `audit_record` + `audit_log`**

Before writing any code for this step, stop and resolve the audit transport gate in the review addendum above. The snippet below is preserved as historical draft context only; it uses unsupported CLI flags on `br 0.1.14` and must not be implemented unchanged.

Replace the two stubs in `impl BeadsAdvanced for BeadsAdapter`:

```rust
async fn audit_record(
    &self,
    issue_id: &str,
    entry: AuditRecordInput,
) -> anyhow::Result<AuditId> {
    #[derive(serde::Deserialize)]
    struct BrAuditOut {
        id: String,
    }
    let type_str = serde_json::to_string(&entry.entry_type)
        .map_err(|e| anyhow::anyhow!("serialize audit type: {e}"))?;
    // `serde_json::to_string` gives us "mutation-plan" (with quotes); strip them.
    let type_str = type_str.trim_matches('"').to_string();
    let data_str = serde_json::to_string(&entry.data)
        .map_err(|e| anyhow::anyhow!("serialize audit data: {e}"))?;

    let output = self
        .run_br(vec![
            "audit".into(),
            "record".into(),
            issue_id.into(),
            "--type".into(),
            type_str,
            "--data".into(),
            data_str,
        ])
        .await?;
    let out: BrAuditOut = serde_json::from_str(&output)
        .map_err(|e| anyhow::anyhow!("parse `br audit record`: {e}\nraw: {output}"))?;
    Ok(out.id)
}

async fn audit_log(&self, issue_id: &str) -> anyhow::Result<Vec<AuditEntry>> {
    #[derive(serde::Deserialize)]
    struct BrAuditEntry {
        id: String,
        issue_id: String,
        #[serde(rename = "type")]
        entry_type: AuditEntryType,
        #[serde(default)]
        actor: Option<String>,
        timestamp: chrono::DateTime<chrono::Utc>,
        #[serde(default)]
        data: serde_json::Value,
    }
    let output = self
        .run_br(vec!["audit".into(), "log".into(), issue_id.into()])
        .await?;
    let items: Vec<BrAuditEntry> = serde_json::from_str(&output)
        .map_err(|e| anyhow::anyhow!("parse `br audit log`: {e}\nraw: {output}"))?;
    Ok(items
        .into_iter()
        .map(|e| AuditEntry {
            id: e.id,
            issue_id: e.issue_id,
            entry_type: e.entry_type,
            actor: e.actor.unwrap_or_default(),
            timestamp: e.timestamp,
            data: e.data,
        })
        .collect())
}
```

- [ ] **Step 4: Run tests to confirm pass**

```bash
cargo test -p spur-pm --test beads_advanced
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/beads.rs crates/spur-pm/tests/beads_advanced.rs
git commit -m "feat(spur-pm): BeadsAdvanced audit record + log wrappers"
```

---

## Task 5: `remove_dependency` + `dep_cycles`

**Files:**
- Modify: `crates/spur-pm/src/beads.rs`
- Modify: `crates/spur-pm/tests/beads_advanced.rs`

- [ ] **Step 1: Append failing tests**

Append to `crates/spur-pm/tests/beads_advanced.rs`:

```rust
#[tokio::test]
async fn remove_dependency_unblocks_task() {
    if !br_available() { return; }
    let (dir, adapter) = setup_workspace().await;
    let a = run_br(dir.path(), &["create", "A", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();
    let b = run_br(dir.path(), &["create", "B", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();
    run_br(dir.path(), &["dep", "add", &b, &a]);

    adapter.remove_dependency(&b, &a).await.unwrap();

    let ready = adapter
        .list_ready(ReadyFilter { limit: Some(50), ..Default::default() })
        .await
        .unwrap();
    let ids: Vec<String> = ready.into_iter().map(|i| i.id).collect();
    assert!(ids.contains(&b), "B should be ready after dep removed, got {ids:?}");
}

#[tokio::test]
async fn dep_cycles_detects_cycle() {
    if !br_available() { return; }
    let (dir, adapter) = setup_workspace().await;
    let a = run_br(dir.path(), &["create", "A", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();
    let b = run_br(dir.path(), &["create", "B", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();
    run_br(dir.path(), &["dep", "add", &a, &b]); // A blocks on B
    // Try to create a cycle: B blocks on A. `br` may reject at add time;
    // if so, the cycle never exists and dep_cycles should return empty.
    let maybe_cycle = Command::new("br")
        .args(["dep", "add", &b, &a, "--json"])
        .current_dir(dir.path())
        .output()
        .expect("br invocation");

    let cycles = adapter.dep_cycles().await.unwrap();
    if maybe_cycle.status.success() {
        // br allowed the cycle; detector must find it.
        assert!(!cycles.is_empty(), "expected cycle, got {cycles:?}");
    } else {
        // br rejected the cycle; detector should find none.
        assert!(cycles.is_empty(), "no cycle but detector returned {cycles:?}");
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

```bash
cargo test -p spur-pm --test beads_advanced remove_dependency_unblocks_task
```

Expected: FAIL — `remove_dependency: not yet implemented`.

- [ ] **Step 3: Implement both methods**

Replace the two remaining stubs:

```rust
async fn remove_dependency(
    &self,
    issue_id: &str,
    depends_on_id: &str,
) -> anyhow::Result<()> {
    self.run_br(vec![
        "dep".into(),
        "remove".into(),
        issue_id.into(),
        depends_on_id.into(),
    ])
    .await?;
    Ok(())
}

async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
    #[derive(serde::Deserialize)]
    struct BrCycle {
        #[serde(default)]
        issues: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct BrCyclesOutput {
        #[serde(default)]
        cycles: Vec<BrCycle>,
    }
    let output = self.run_br(vec!["dep".into(), "cycles".into()]).await?;
    // `br dep cycles --json` returns either an array or {"cycles": [...]};
    // try the wrapped form first, then fall back to a bare array.
    if let Ok(wrapped) = serde_json::from_str::<BrCyclesOutput>(&output) {
        return Ok(wrapped
            .cycles
            .into_iter()
            .map(|c| DependencyCycle { issues: c.issues })
            .collect());
    }
    let bare: Vec<BrCycle> = serde_json::from_str(&output)
        .map_err(|e| anyhow::anyhow!("parse `br dep cycles`: {e}\nraw: {output}"))?;
    Ok(bare
        .into_iter()
        .map(|c| DependencyCycle { issues: c.issues })
        .collect())
}
```

- [ ] **Step 4: Run tests to confirm pass**

```bash
cargo test -p spur-pm --test beads_advanced
```

Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/beads.rs crates/spur-pm/tests/beads_advanced.rs
git commit -m "feat(spur-pm): BeadsAdvanced dep_cycles + remove_dependency"
```

---

## Task 6: `PmService::advanced()` accessor

**Files:**
- Modify: `crates/spur-pm/src/service.rs`

- [ ] **Step 1: Write a unit test at the bottom of `service.rs`**

Append inside the existing `#[cfg(test)] mod tests` block near line 169 in `crates/spur-pm/src/service.rs`:

```rust
#[test]
fn advanced_returns_none_without_backend() {
    // Synthesize a PmService with no backend — just to prove the accessor compiles.
    // Actual integration is exercised in the integration tests.
    // This test intentionally does not construct PmService directly because its
    // fields are private; it is a smoke test that the method exists and returns
    // Option<&dyn BeadsAdvanced>.
    fn assert_accessor(svc: &super::PmService) -> Option<&dyn crate::BeadsAdvanced> {
        svc.advanced()
    }
    let _ = assert_accessor; // suppress unused warning
}
```

- [ ] **Step 2: Run — should fail: method does not exist**

```bash
cargo test -p spur-pm --lib service::tests::advanced_returns_none_without_backend
```

Expected: compile error — `no method named advanced`.

- [ ] **Step 3: Add the `advanced()` method**

In `crates/spur-pm/src/service.rs`, add this inside `impl PmService` (near the existing `pub fn analyzer(&self) -> Option<&BvAdapter>` method, around line 163):

```rust
    /// Returns the beads-advanced extension surface if the backend is beads.
    /// Returns `None` for non-beads backends (GitHub). Callers use this to
    /// gate adaptive-plan-repair features on beads availability.
    pub fn advanced(&self) -> Option<&dyn crate::advanced::BeadsAdvanced> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => Some(beads as &dyn crate::advanced::BeadsAdvanced),
            PmBackendInner::GitHub { .. } => None,
        }
    }
```

- [ ] **Step 4: Run test — should pass**

```bash
cargo test -p spur-pm --lib service::tests
```

Expected: all `service::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/service.rs
git commit -m "feat(spur-pm): PmService::advanced() exposes BeadsAdvanced"
```

---

## Task 7: Actor threading — `connect_with_actor` + `--actor` in `run_br`

**Files:**
- Modify: `crates/spur-pm/src/beads.rs`
- Modify: `crates/spur-pm/tests/beads_advanced.rs`

- [ ] **Step 1: Write failing integration test asserting actor attribution**

Append to `crates/spur-pm/tests/beads_advanced.rs`:

```rust
#[tokio::test]
async fn audit_record_carries_actor_when_set() {
    if !br_available() { return; }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);

    let adapter = BeadsAdapter::connect_with_actor(
        dir.path(),
        Some("brain:test-session".to_string()),
        None,
    )
    .await
    .unwrap();

    let id = run_br(dir.path(), &["create", "T", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();

    adapter
        .audit_record(
            &id,
            AuditRecordInput {
                entry_type: AuditEntryType::PlanSubmit,
                data: serde_json::json!({}),
            },
        )
        .await
        .unwrap();

    let log = adapter.audit_log(&id).await.unwrap();
    let entry = log
        .iter()
        .find(|e| e.entry_type == AuditEntryType::PlanSubmit)
        .expect("expected PlanSubmit entry");
    assert_eq!(entry.actor, "brain:test-session");
}
```

- [ ] **Step 2: Run — should fail: no `connect_with_actor`**

```bash
cargo test -p spur-pm --test beads_advanced audit_record_carries_actor_when_set
```

Expected: compile error — method not found.

- [ ] **Step 3: Add `default_actor` field + `connect_with_actor` ctor + `run_br_as`**

In `crates/spur-pm/src/beads.rs`:

Change the `BeadsAdapter` struct (around line 147) to:

```rust
pub struct BeadsAdapter {
    cwd: PathBuf,
    last_poll: Mutex<Option<DateTime<Utc>>>,
    default_actor: Option<String>,
    cursor_path: Option<PathBuf>, // used by Task 10; present now for forward compat
}
```

Replace the existing `connect` method and add `connect_with_actor`:

```rust
impl BeadsAdapter {
    pub async fn connect(repo_root: &Path) -> anyhow::Result<Self> {
        Self::connect_with_actor(repo_root, None, None).await
    }

    pub async fn connect_with_actor(
        repo_root: &Path,
        default_actor: Option<String>,
        cursor_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let adapter = Self {
            cwd: repo_root.to_path_buf(),
            last_poll: Mutex::new(None),
            default_actor,
            cursor_path,
        };

        // Verify br binary
        let version_output = adapter
            .run_br(vec!["version".into()])
            .await
            .map_err(|e| {
                if e.to_string().contains("br binary not found") {
                    e
                } else {
                    anyhow::anyhow!(
                        "Failed to run `br version`: {e}\n\
                         Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
                    )
                }
            })?;
        let version: BrVersion = serde_json::from_str(&version_output).map_err(|e| {
            anyhow::anyhow!("Failed to parse `br version` output: {e}\nRaw: {version_output}")
        })?;
        tracing::info!(version = %version.version, "connected to beads_rust (br)");

        adapter
            .run_br(vec!["stats".into()])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read .beads/ database (`br stats`): {e}"))?;

        Ok(adapter)
    }

    async fn run_br_as(
        &self,
        args: Vec<String>,
        actor_override: Option<&str>,
    ) -> anyhow::Result<String> {
        let actor = actor_override.or(self.default_actor.as_deref());
        let mut full = args;
        if let Some(a) = actor {
            full.insert(0, "--actor".into());
            full.insert(1, a.to_string());
        }
        self.run_br_inner(full).await
    }
}
```

Rename the existing private `run_br` implementation to `run_br_inner` (keep its body identical), and change the existing public `run_br` to delegate:

```rust
// Keep the existing retry-wrapping body under a new name:
async fn run_br_inner(&self, args: Vec<String>) -> anyhow::Result<String> {
    // (existing body of run_br, unchanged)
    tracing::debug!(?args, "running br CLI");
    match self.run_br_once(&args).await {
        Ok(out) => Ok(out),
        Err(BrCallError::Fatal(e)) => Err(e),
        Err(BrCallError::Retryable(msg)) => {
            tracing::debug!(reason = %msg, "br retryable error, retrying after 50ms");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            match self.run_br_once(&args).await {
                Ok(out) => Ok(out),
                Err(BrCallError::Fatal(e)) => Err(e),
                Err(BrCallError::Retryable(msg2)) => {
                    anyhow::bail!("br retryable error after 2 attempts: {}", msg2)
                }
            }
        }
    }
}

// Existing call sites use run_br; wrap it to add default_actor:
async fn run_br(&self, args: Vec<String>) -> anyhow::Result<String> {
    self.run_br_as(args, None).await
}
```

- [ ] **Step 4: Run the new test — should pass**

```bash
cargo test -p spur-pm --test beads_advanced audit_record_carries_actor_when_set
```

Expected: 1 passed.

- [ ] **Step 5: Run the full spur-pm test suite to confirm no regressions**

```bash
cargo test -p spur-pm
```

Expected: all tests pass (existing + new).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-pm/src/beads.rs crates/spur-pm/tests/beads_advanced.rs
git commit -m "feat(spur-pm): actor threading via connect_with_actor + --actor flag"
```

---

## Task 8: Cursor fix F1 — use `max(updated_at)` instead of `Utc::now()`

**Files:**
- Modify: `crates/spur-pm/src/beads.rs`
- Create: `crates/spur-pm/tests/poll_cursor.rs`

- [ ] **Step 1: Write the regression test**

Create `crates/spur-pm/tests/poll_cursor.rs`:

```rust
//! Regression tests for BeadsAdapter::poll cursor bugs.

use std::path::Path;
use std::process::Command;

use spur_pm::{BeadsAdapter, IssueTracker};
use tempfile::TempDir;

fn br_available() -> bool {
    Command::new("br").arg("--help").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .output()
        .expect("br invocation failed");
    assert!(out.status.success(), "br {:?} failed: {:?}", args, out);
    String::from_utf8(out.stdout).unwrap()
}

/// F1 regression: the original poll() set cursor to Utc::now(), not
/// max(updated_at) of the returned batch. Writes with updated_at
/// between fetch and cursor-write were silently skipped. With F1,
/// two issues written in quick succession must BOTH appear in the
/// first poll that sees either of them.
#[tokio::test]
async fn poll_returns_all_writes_in_same_window() {
    if !br_available() { return; }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let adapter = BeadsAdapter::connect(dir.path()).await.unwrap();

    // Initial poll sets baseline cursor (no events yet).
    let _ = adapter.poll().await.unwrap();

    // Create two issues "simultaneously" (sequentially but fast).
    let a = run_br(dir.path(), &["create", "A", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();
    let b = run_br(dir.path(), &["create", "B", "--silent", "-t", "task"])
        .trim().trim_matches('"').to_string();

    let events = adapter.poll().await.unwrap();
    let ids: Vec<String> = events
        .into_iter()
        .map(|e| match e {
            spur_pm::PmEvent::IssueCreated(s) | spur_pm::PmEvent::IssueUpdated(s) => s.id,
        })
        .collect();
    assert!(ids.contains(&a), "A missing from poll events: {ids:?}");
    assert!(ids.contains(&b), "B missing from poll events: {ids:?}");

    // Second poll with NO intervening writes must return empty.
    let events2 = adapter.poll().await.unwrap();
    assert!(events2.is_empty(), "second poll returned stale events: {events2:?}");
}
```

- [ ] **Step 2: Run test**

```bash
cargo test -p spur-pm --test poll_cursor poll_returns_all_writes_in_same_window
```

The test MAY already pass on a quiet machine because `Utc::now()` happens after all writes. Run it multiple times to attempt to surface the race:

```bash
for i in 1 2 3 4 5; do cargo test -p spur-pm --test poll_cursor -- --test-threads=1; done
```

Either it flakes, or we trust the reasoning in the spec's grounding table and proceed to fix it preemptively.

- [ ] **Step 3: Apply F1 — replace the cursor with a boundary-safe representation**

Do not implement the draft rewrite below unchanged. It still uses an inclusive timestamp boundary and will replay rows at the cursor edge. Replace F1 with a cursor that can distinguish "already emitted at timestamp T" from "new row also at timestamp T", and carry that same representation forward into Task 9's disk-backed cursor.

In `crates/spur-pm/src/beads.rs`, find the existing `poll()` method (around line 465). Replace the cursor-write block at line 508–513 with:

```rust
// Determine new cursor as the max updated_at of the returned batch,
// NOT Utc::now() — prevents races where writes with updated_at between
// fetch and cursor-write are skipped on subsequent polls.
let new_cursor = events
    .iter()
    .filter_map(|e| match e {
        PmEvent::IssueCreated(s) | PmEvent::IssueUpdated(s) => {
            // IssueSummary does not carry updated_at; re-look it up from
            // the raw items vec we already have before the map-into-event step.
            None::<DateTime<Utc>>
        }
    })
    .max();
```

Wait — `IssueSummary` doesn't carry `updated_at`, so we can't compute max from events. Restructure the poll logic: compute `max(updated_at)` BEFORE the `map(IssueSummary::from)` step, from the raw `BrIssueWithCounts` items.

Apply this full-poll rewrite to `crates/spur-pm/src/beads.rs:465-514` (replace the entire method body):

```rust
async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
    let output = self
        .run_br(vec![
            "list".into(),
            "-s".into(),
            "open,in_progress,blocked".into(), // poll all non-terminal statuses
            "--limit".into(),
            "500".into(),
        ])
        .await?;

    let items: Vec<BrIssueWithCounts> = serde_json::from_str(&output)
        .map_err(|e| anyhow::anyhow!("Failed to parse `br list` output: {e}\nRaw: {output}"))?;

    let last_poll = {
        let guard = self
            .last_poll
            .lock()
            .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
        *guard
    };

    // Filter to only items newer than last cursor (if set).
    let kept: Vec<BrIssueWithCounts> = items
        .into_iter()
        .filter(|it| match last_poll {
            Some(c) => it.updated_at >= c,
            None => true,
        })
        .collect();

    // Compute the NEW cursor as max updated_at from the kept batch.
    let new_cursor = kept.iter().map(|it| it.updated_at).max();

    let had_cursor = last_poll.is_some();
    let events: Vec<PmEvent> = kept
        .into_iter()
        .map(|it| {
            let summary = IssueSummary::from(it);
            if had_cursor {
                PmEvent::IssueUpdated(summary)
            } else {
                PmEvent::IssueCreated(summary)
            }
        })
        .collect();

    // Only update the cursor if we observed any events (otherwise keep last_poll
    // unchanged so the next poll still sees "newer than before" correctly).
    if let Some(nc) = new_cursor {
        let mut guard = self
            .last_poll
            .lock()
            .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
        *guard = Some(nc);
    } else if !had_cursor {
        // First poll returned zero events — set cursor to now so subsequent polls
        // don't replay the empty history.
        let mut guard = self
            .last_poll
            .lock()
            .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
        *guard = Some(Utc::now());
    }

    Ok(events)
}
```

- [ ] **Step 4: Run the regression test**

```bash
cargo test -p spur-pm --test poll_cursor
```

Expected: PASS (deterministically now — cursor only advances to max observed updated_at).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/beads.rs crates/spur-pm/tests/poll_cursor.rs
git commit -m "fix(spur-pm): poll() cursor uses max(updated_at) not Utc::now()"
```

---

## Task 9: Cursor fix F2 — optional disk-backed cursor

**Files:**
- Modify: `crates/spur-pm/src/beads.rs`
- Modify: `crates/spur-pm/tests/poll_cursor.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-pm/tests/poll_cursor.rs`:

```rust
#[tokio::test]
async fn disk_cursor_survives_adapter_restart() {
    if !br_available() { return; }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let cursor_file = dir.path().join(".spur-test-cursor");

    // First session: create one issue, poll it.
    {
        let adapter = BeadsAdapter::connect_with_actor(
            dir.path(),
            None,
            Some(cursor_file.clone()),
        )
        .await
        .unwrap();
        run_br(dir.path(), &["create", "Issue1", "--silent", "-t", "task"]);
        let _ = adapter.poll().await.unwrap();
    }

    // Second session: open adapter with SAME cursor file. Poll should return
    // zero events (cursor persisted).
    {
        let adapter = BeadsAdapter::connect_with_actor(
            dir.path(),
            None,
            Some(cursor_file.clone()),
        )
        .await
        .unwrap();
        let events = adapter.poll().await.unwrap();
        assert!(
            events.is_empty(),
            "second session saw {} stale events — disk cursor not persisted",
            events.len()
        );
    }
}
```

- [ ] **Step 2: Run — should fail**

```bash
cargo test -p spur-pm --test poll_cursor disk_cursor_survives_adapter_restart
```

Expected: FAIL — second session sees `Issue1` because cursor is in-memory only.

- [ ] **Step 3: Implement disk-backed cursor**

In `crates/spur-pm/src/beads.rs`, add two private helpers inside `impl BeadsAdapter`:

```rust
    fn load_cursor(&self) -> Option<DateTime<Utc>> {
        let path = self.cursor_path.as_ref()?;
        let contents = std::fs::read_to_string(path).ok()?;
        let parsed: DateTime<Utc> = contents.trim().parse().ok()?;
        Some(parsed)
    }

    fn save_cursor(&self, cursor: DateTime<Utc>) {
        if let Some(path) = self.cursor_path.as_ref() {
            // RFC3339 format is round-trippable via FromStr<DateTime<Utc>>.
            let s = cursor.to_rfc3339();
            if let Err(e) = std::fs::write(path, s) {
                tracing::warn!(?path, "failed to write cursor file: {e}");
            }
        }
    }
```

Modify `connect_with_actor` (from Task 7) to initialize `last_poll` from disk on connect:

```rust
    pub async fn connect_with_actor(
        repo_root: &Path,
        default_actor: Option<String>,
        cursor_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let adapter = Self {
            cwd: repo_root.to_path_buf(),
            last_poll: Mutex::new(None),
            default_actor,
            cursor_path,
        };

        // Hydrate last_poll from disk if cursor_path is set and file exists.
        if let Some(cursor) = adapter.load_cursor() {
            let mut guard = adapter
                .last_poll
                .lock()
                .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
            *guard = Some(cursor);
        }

        // (rest of existing body — version check + stats check — unchanged)
        let version_output = adapter.run_br(vec!["version".into()]).await.map_err(|e| {
            if e.to_string().contains("br binary not found") {
                e
            } else {
                anyhow::anyhow!(
                    "Failed to run `br version`: {e}\n\
                     Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
                )
            }
        })?;
        let version: BrVersion = serde_json::from_str(&version_output).map_err(|e| {
            anyhow::anyhow!("Failed to parse `br version` output: {e}\nRaw: {version_output}")
        })?;
        tracing::info!(version = %version.version, "connected to beads_rust (br)");

        adapter
            .run_br(vec!["stats".into()])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read .beads/ database (`br stats`): {e}"))?;

        Ok(adapter)
    }
```

Modify the `poll()` body (from Task 8) to call `save_cursor` whenever it writes the in-memory cursor. In both the `if let Some(nc) = new_cursor` branch and the `else if !had_cursor` branch, after updating the Mutex, add:

```rust
    // Immediately after *guard = Some(nc) or *guard = Some(Utc::now()):
    self.save_cursor(*guard.as_ref().unwrap());
```

Complete the change so both branches save to disk:

```rust
    if let Some(nc) = new_cursor {
        let mut guard = self
            .last_poll
            .lock()
            .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
        *guard = Some(nc);
        self.save_cursor(nc);
    } else if !had_cursor {
        let now = Utc::now();
        let mut guard = self
            .last_poll
            .lock()
            .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
        *guard = Some(now);
        self.save_cursor(now);
    }
```

- [ ] **Step 4: Run the test — should pass**

```bash
cargo test -p spur-pm --test poll_cursor
```

Expected: 2 passed (race regression + disk persistence).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/beads.rs crates/spur-pm/tests/poll_cursor.rs
git commit -m "feat(spur-pm): disk-backed poll cursor via cursor_path ctor arg"
```

---

## Task 10: Label vocabulary module

**Files:**
- Create: `crates/spur-mcp/src/plan/labels.rs`
- Modify: `crates/spur-mcp/src/plan.rs` (or `plan/mod.rs` — see step 1)

- [ ] **Step 1: Decide module layout**

Check whether `crates/spur-mcp/src/plan.rs` is a single file (3299 lines per the grounding table). If it is a single file, promote it to a module directory by:

```bash
mkdir -p crates/spur-mcp/src/plan
git mv crates/spur-mcp/src/plan.rs crates/spur-mcp/src/plan/mod.rs
```

Verify the crate still builds:

```bash
cargo check -p spur-mcp
```

If it does, the rest of this task adds submodules alongside `mod.rs`. If the move breaks something (unlikely — `mod.rs` is the canonical filename for a single-file-as-directory), revert with `git mv` back and add submodules as `crates/spur-mcp/src/plan_labels.rs` / `plan_signals.rs` / etc. at the crate root. The remaining tasks assume the `plan/mod.rs` layout.

- [ ] **Step 2: Create `crates/spur-mcp/src/plan/labels.rs`**

```rust
//! Label vocabulary for SPUR plan tracking in beads.
//!
//! Every label emitted by brain / worker / reconciler MUST come from a helper
//! in this module. String-typing labels at the call site is a bug waiting to
//! happen — use these constructors instead.
//!
//! See `docs/superpowers/specs/2026-04-20-adaptive-plan-repair-design.md`
//! §Information Flow → Label vocabulary for the authoritative list.

pub fn plan_id(plan_id: &str) -> String {
    format!("spur.plan_id={plan_id}")
}

pub fn plan_task_id(task_id: &str) -> String {
    format!("spur.plan_task_id={task_id}")
}

pub fn delegation_id(delegation_id: &str) -> String {
    format!("delegation-id:{delegation_id}")
}

pub fn signal_kind(kind: &str) -> String {
    format!("signal:{kind}")
}

pub fn signal_kind_bucket(kind: &str, bucket: &str) -> String {
    format!("signal:{kind}:{bucket}")
}

pub const SIGNAL_LATE_ARRIVAL: &str = "signal:late-arrival";
pub const READY_FOR_REVIEW: &str = "ready-for-review";

pub fn mutation_id(mutation_id: &uuid::Uuid) -> String {
    format!("mutation-id:{mutation_id}")
}

pub fn superseded_by(child_ids: &[String]) -> String {
    format!("superseded-by:{}", child_ids.join(","))
}

/// Returns `Some(task_id)` if the given label is a `spur.plan_task_id=<id>` label.
pub fn parse_plan_task_id(label: &str) -> Option<&str> {
    label.strip_prefix("spur.plan_task_id=")
}

/// Returns `Some(plan_id)` if the given label is a `spur.plan_id=<id>` label.
pub fn parse_plan_id(label: &str) -> Option<&str> {
    label.strip_prefix("spur.plan_id=")
}

/// Returns `Some(kind)` if the given label is a `signal:<kind>` label
/// (not a bucketed variant `signal:<kind>:<bucket>`).
pub fn parse_signal_kind(label: &str) -> Option<&str> {
    let rest = label.strip_prefix("signal:")?;
    if rest.contains(':') {
        None
    } else {
        Some(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_produce_expected_strings() {
        assert_eq!(plan_id("P1"), "spur.plan_id=P1");
        assert_eq!(plan_task_id("T1"), "spur.plan_task_id=T1");
        assert_eq!(delegation_id("del-A"), "delegation-id:del-A");
        assert_eq!(signal_kind("scope-drift"), "signal:scope-drift");
        assert_eq!(
            signal_kind_bucket("scope-drift", "high"),
            "signal:scope-drift:high"
        );
        assert_eq!(SIGNAL_LATE_ARRIVAL, "signal:late-arrival");
    }

    #[test]
    fn parsers_invert_constructors() {
        let p = plan_task_id("T1");
        assert_eq!(parse_plan_task_id(&p), Some("T1"));
        assert_eq!(parse_plan_task_id("unrelated"), None);
        let plan = plan_id("P1");
        assert_eq!(parse_plan_id(&plan), Some("P1"));
        assert_eq!(parse_signal_kind("signal:scope-drift"), Some("scope-drift"));
        assert_eq!(parse_signal_kind("signal:scope-drift:high"), None);
    }

    #[test]
    fn superseded_by_joins_ids_with_comma() {
        assert_eq!(
            superseded_by(&["bd-1".into(), "bd-2".into(), "bd-3".into()]),
            "superseded-by:bd-1,bd-2,bd-3"
        );
    }
}
```

- [ ] **Step 3: Register the submodule in `crates/spur-mcp/src/plan/mod.rs`**

At the very top of `mod.rs`, add:

```rust
pub mod labels;
```

- [ ] **Step 4: Run unit tests**

```bash
cargo test -p spur-mcp --lib plan::labels::tests
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/
git commit -m "feat(spur-mcp): label vocabulary module for plan tracking in beads"
```

---

## Task 11: Sentinel comment parser

**Files:**
- Create: `crates/spur-mcp/src/plan/signals.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs`
- Modify: `crates/spur-mcp/Cargo.toml` (add `uuid` if not already)

- [ ] **Step 1: Ensure `uuid` is a dep of `spur-mcp`**

In `crates/spur-mcp/Cargo.toml` under `[dependencies]`, add (in alphabetical order):

```toml
uuid = { workspace = true, features = ["v4", "serde"] }
```

- [ ] **Step 2: Create `crates/spur-mcp/src/plan/signals.rs`**

```rust
//! Worker signal encoding + parsing.
//!
//! Workers emit structured signals as sentinel-fenced JSON inside a beads
//! comment, plus a `signal:<kind>` label. v0a defines the format; v0b adds
//! the `report_signal` MCP tool that produces them and the brain-side
//! consumer. Shipping the parser in v0a locks the format before consumption.
//!
//! See spec §Information Flow → Signal schema.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SENTINEL_PREFIX: &str = "[[spur-signal v1]]";

/// The full worker-signal enum. v0 ships `ScopeDrift` only; future variants
/// land as additional `#[non_exhaustive]` entries.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerSignal {
    ScopeDrift {
        signal_id: Uuid,
        severity: f32,
        reason: String,
        #[serde(default)]
        estimated_subtasks: Option<u8>,
    },
}

impl WorkerSignal {
    /// Returns the `signal_id` regardless of variant.
    pub fn signal_id(&self) -> Uuid {
        match self {
            WorkerSignal::ScopeDrift { signal_id, .. } => *signal_id,
        }
    }

    /// Returns the kind-string used for `signal:<kind>` labels.
    pub fn kind_label(&self) -> &'static str {
        match self {
            WorkerSignal::ScopeDrift { .. } => "scope-drift",
        }
    }
}

/// Encode a `WorkerSignal` as a full sentinel comment body ready for
/// `br comments add`.
pub fn encode_comment(signal: &WorkerSignal) -> String {
    let json =
        serde_json::to_string(signal).expect("WorkerSignal always serializes");
    format!("{SENTINEL_PREFIX}\n{json}")
}

/// Parse a comment body. Returns `None` if the body does not begin with the
/// sentinel prefix. Returns `Some(Err(_))` if the sentinel is present but
/// the JSON is malformed or the variant is unknown.
pub fn parse_comment(body: &str) -> Option<Result<WorkerSignal, ParseError>> {
    let rest = body.trim_start().strip_prefix(SENTINEL_PREFIX)?;
    let json = rest.trim_start();
    Some(serde_json::from_str(json).map_err(ParseError::Json))
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("sentinel JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_parse_round_trips() {
        let sig = WorkerSignal::ScopeDrift {
            signal_id: Uuid::nil(),
            severity: 0.82,
            reason: "auth refactor pulls in token store".to_string(),
            estimated_subtasks: Some(3),
        };
        let body = encode_comment(&sig);
        assert!(body.starts_with(SENTINEL_PREFIX));
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, sig);
    }

    #[test]
    fn parse_returns_none_for_non_sentinel_comment() {
        let got = parse_comment("ordinary human comment");
        assert!(got.is_none());
    }

    #[test]
    fn parse_returns_err_for_malformed_sentinel() {
        let body = format!("{SENTINEL_PREFIX}\nnot json");
        let got = parse_comment(&body).unwrap();
        assert!(got.is_err());
    }

    #[test]
    fn parse_tolerates_leading_whitespace() {
        let sig = WorkerSignal::ScopeDrift {
            signal_id: Uuid::nil(),
            severity: 0.1,
            reason: "r".into(),
            estimated_subtasks: None,
        };
        let body = format!("   \n  {}", encode_comment(&sig));
        let parsed = parse_comment(&body).unwrap().unwrap();
        assert_eq!(parsed, sig);
    }

    #[test]
    fn signal_id_accessor_returns_value() {
        let id = Uuid::new_v4();
        let sig = WorkerSignal::ScopeDrift {
            signal_id: id,
            severity: 0.1,
            reason: "r".into(),
            estimated_subtasks: None,
        };
        assert_eq!(sig.signal_id(), id);
        assert_eq!(sig.kind_label(), "scope-drift");
    }
}
```

- [ ] **Step 3: Register submodule**

In `crates/spur-mcp/src/plan/mod.rs`, add below the existing `pub mod labels;`:

```rust
pub mod signals;
```

- [ ] **Step 4: Run unit tests**

```bash
cargo test -p spur-mcp --lib plan::signals::tests
```

Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/Cargo.toml crates/spur-mcp/src/plan/
git commit -m "feat(spur-mcp): [[spur-signal v1]] sentinel comment parser"
```

---

## Task 12: Audit emission helper

**Files:**
- Create: `crates/spur-mcp/src/plan/audit.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs`

- [ ] **Step 1: Create `crates/spur-mcp/src/plan/audit.rs`**

Before implementing this helper, add the missing correlation layer from the review addendum above. The helper must consume real beads issue IDs already present on `PlanState`, not reconstruct them indirectly from response text.

```rust
//! Helper for emitting `br audit record` entries from plan-executor paths.
//!
//! Every plan-affecting action (submit, dispatch, completion, approval,
//! rejection) MUST emit an audit record per spec Principle P4
//! ("instrument now, analyze later"). This module centralizes the
//! data-payload shape so changes stay consistent.

use spur_pm::{AuditEntryType, AuditRecordInput, BeadsAdvanced};

pub struct PlanSubmitPayload<'a> {
    pub plan_id: &'a str,
    pub epic_issue_id: &'a str,
    pub task_ids: &'a [String],
}

pub struct DispatchPayload<'a> {
    pub delegation_id: &'a str,
    pub worker: &'a str,
    pub attempt: u32,
}

pub struct CompletionPayload<'a> {
    pub delegation_id: &'a str,
    pub worker_branch: Option<&'a str>,
    pub diff_summary: Option<&'a str>,
}

pub struct RejectionPayload<'a> {
    pub delegation_id: &'a str,
    pub feedback: &'a str,
}

pub struct ApprovalPayload<'a> {
    pub delegation_id: &'a str,
}

/// Emit an audit record; logs and swallows errors. Audit failures MUST NOT
/// block plan execution — they are advisory per the spec's durability
/// contract (state store is authoritative, audit log is analytical).
async fn emit(
    advanced: &dyn BeadsAdvanced,
    issue_id: &str,
    entry_type: AuditEntryType,
    data: serde_json::Value,
) {
    let input = AuditRecordInput { entry_type, data };
    if let Err(e) = advanced.audit_record(issue_id, input).await {
        tracing::warn!(%issue_id, "audit_record failed: {e}");
    }
}

pub async fn emit_plan_submit(advanced: &dyn BeadsAdvanced, payload: PlanSubmitPayload<'_>) {
    emit(
        advanced,
        payload.epic_issue_id,
        AuditEntryType::PlanSubmit,
        serde_json::json!({
            "plan_id": payload.plan_id,
            "epic_issue_id": payload.epic_issue_id,
            "task_ids": payload.task_ids,
        }),
    )
    .await;
}

pub async fn emit_dispatch(
    advanced: &dyn BeadsAdvanced,
    task_issue_id: &str,
    payload: DispatchPayload<'_>,
) {
    emit(
        advanced,
        task_issue_id,
        AuditEntryType::Dispatch,
        serde_json::json!({
            "delegation_id": payload.delegation_id,
            "worker": payload.worker,
            "attempt": payload.attempt,
        }),
    )
    .await;
}

pub async fn emit_completion(
    advanced: &dyn BeadsAdvanced,
    task_issue_id: &str,
    payload: CompletionPayload<'_>,
) {
    emit(
        advanced,
        task_issue_id,
        AuditEntryType::Completion,
        serde_json::json!({
            "delegation_id": payload.delegation_id,
            "worker_branch": payload.worker_branch,
            "diff_summary": payload.diff_summary,
        }),
    )
    .await;
}

pub async fn emit_approval(
    advanced: &dyn BeadsAdvanced,
    task_issue_id: &str,
    payload: ApprovalPayload<'_>,
) {
    emit(
        advanced,
        task_issue_id,
        AuditEntryType::Approval,
        serde_json::json!({ "delegation_id": payload.delegation_id }),
    )
    .await;
}

pub async fn emit_rejection(
    advanced: &dyn BeadsAdvanced,
    task_issue_id: &str,
    payload: RejectionPayload<'_>,
) {
    emit(
        advanced,
        task_issue_id,
        AuditEntryType::Rejection,
        serde_json::json!({
            "delegation_id": payload.delegation_id,
            "feedback": payload.feedback,
        }),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal test-only fake for BeadsAdvanced — verifies the helper
    // constructs the expected AuditRecordInput payload shape.

    struct RecordingAdvanced {
        calls: tokio::sync::Mutex<Vec<(String, AuditEntryType, serde_json::Value)>>,
    }

    #[async_trait::async_trait]
    impl BeadsAdvanced for RecordingAdvanced {
        async fn list_ready(
            &self,
            _f: spur_pm::ReadyFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            Ok(vec![])
        }
        async fn list_comments(&self, _id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
            Ok(vec![])
        }
        async fn add_comment(
            &self,
            _id: &str,
            _body: &str,
        ) -> anyhow::Result<spur_pm::CommentId> {
            Ok("c".into())
        }
        async fn audit_record(
            &self,
            issue_id: &str,
            entry: AuditRecordInput,
        ) -> anyhow::Result<spur_pm::AuditId> {
            self.calls.lock().await.push((
                issue_id.to_string(),
                entry.entry_type,
                entry.data,
            ));
            Ok("a".into())
        }
        async fn audit_log(&self, _id: &str) -> anyhow::Result<Vec<spur_pm::AuditEntry>> {
            Ok(vec![])
        }
        async fn remove_dependency(&self, _a: &str, _b: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn emit_plan_submit_writes_expected_payload() {
        let adv = RecordingAdvanced { calls: tokio::sync::Mutex::new(vec![]) };
        let tasks = vec!["bd-2".to_string(), "bd-3".to_string()];
        emit_plan_submit(
            &adv,
            PlanSubmitPayload {
                plan_id: "P1",
                epic_issue_id: "bd-1",
                task_ids: &tasks,
            },
        )
        .await;
        let calls = adv.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "bd-1");
        assert_eq!(calls[0].1, AuditEntryType::PlanSubmit);
        assert_eq!(calls[0].2["plan_id"], "P1");
        assert_eq!(calls[0].2["epic_issue_id"], "bd-1");
    }

    #[tokio::test]
    async fn emit_dispatch_writes_expected_payload() {
        let adv = RecordingAdvanced { calls: tokio::sync::Mutex::new(vec![]) };
        emit_dispatch(
            &adv,
            "bd-2",
            DispatchPayload {
                delegation_id: "del-A",
                worker: "gemini-acp",
                attempt: 1,
            },
        )
        .await;
        let calls = adv.calls.lock().await;
        assert_eq!(calls[0].1, AuditEntryType::Dispatch);
        assert_eq!(calls[0].2["delegation_id"], "del-A");
        assert_eq!(calls[0].2["worker"], "gemini-acp");
        assert_eq!(calls[0].2["attempt"], 1);
    }
}
```

- [ ] **Step 2: Register submodule**

In `crates/spur-mcp/src/plan/mod.rs` add:

```rust
pub mod audit;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p spur-mcp --lib plan::audit::tests
```

Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan/audit.rs crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): plan audit emission helpers"
```

---

## Task 13: Wire audit emission into `handle_submit_plan`

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (around line 1734)

- [ ] **Step 1: Locate the plan-submit handler**

Before adding audit emission here, change the persisted-plan success path so `EpicSubgraph.task_map` is written back onto the matching `PlanTaskEntry.spec.issue_id` values. Without that, downstream task-level audit sites still have no reliable beads IDs to target.

Read `crates/spur-mcp/src/server.rs:1734` and scan the body of `handle_submit_plan` (~50-100 lines after that). Identify:
- Where the epic issue is created in beads (call to `pm.create_issue` or similar)
- Where child task issues are created
- Where the handler returns success to the caller

The audit emission goes AFTER all issues have been created successfully, BEFORE returning to the caller.

- [ ] **Step 2: Add the audit emission at the success path**

At the end of the successful-creation branch (just before returning the JSON-RPC response), add:

```rust
// Emit plan-submit audit breadcrumb. Advisory — failure logs, doesn't abort.
if let Some(advanced) = self.pm.advanced() {
    crate::plan::audit::emit_plan_submit(
        advanced,
        crate::plan::audit::PlanSubmitPayload {
            plan_id: &plan_id,
            epic_issue_id: &epic_id,
            task_ids: &task_issue_ids,
        },
    )
    .await;
}
```

The exact variable names (`plan_id`, `epic_id`, `task_issue_ids`) depend on what's in scope in the existing handler — use whatever that handler calls them. If the handler does not currently compute `task_issue_ids` as a `Vec<String>`, collect them from the create-loop's return values.

- [ ] **Step 3: Run the existing submit-plan integration test to confirm no regression**

```bash
cargo test -p spur-mcp --test submit_plan_persist
```

Expected: existing test passes.

- [ ] **Step 4: Add a new integration test asserting the audit record lands**

Create `crates/spur-mcp/tests/submit_plan_audit.rs`:

```rust
//! Asserts submit_plan emits a PlanSubmit audit record on the epic issue.
//!
//! Uses the existing test harness in submit_plan_persist.rs as a template.
//! If the harness module is not reusable, inline what is needed.

// NOTE: This test requires the spur-mcp server + beads harness.
// See submit_plan_persist.rs (crates/spur-mcp/tests/) for setup pattern.
// Copy the minimum subset:
//   1. spawn the server
//   2. call submit_plan via the JSON-RPC interface
//   3. assert br audit log on the returned epic issue contains PlanSubmit

// Pseudocode — adapt to the actual harness:
// #[tokio::test]
// async fn submit_plan_emits_audit_record() {
//     let harness = TestHarness::new().await;
//     let resp = harness.submit_plan(sample_plan()).await.unwrap();
//     let epic_id = resp.epic_issue_id;
//     let log = harness.pm.advanced().unwrap().audit_log(&epic_id).await.unwrap();
//     assert!(log.iter().any(|e| e.entry_type == AuditEntryType::PlanSubmit));
// }
```

Complete this test by following the same pattern as `crates/spur-mcp/tests/submit_plan_persist.rs`. If that file's helpers are private, duplicate the minimum setup (spawn server, submit plan) inline.

- [ ] **Step 5: Run the new test**

```bash
cargo test -p spur-mcp --test submit_plan_audit
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/submit_plan_audit.rs
git commit -m "feat(spur-mcp): emit PlanSubmit audit record from submit_plan"
```

---

## Task 14: Wire audit emission into dispatch / completion / review paths

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` and/or `crates/spur-mcp/src/plan/mod.rs` (existing plan executor code)

- [ ] **Step 1: Locate the four emission sites**

Search for the existing paths:

```bash
grep -n "PlanTaskStatus::Dispatched" crates/spur-mcp/src/plan/mod.rs
grep -n "PlanTaskStatus::AwaitingReview" crates/spur-mcp/src/plan/mod.rs
grep -n "review_task" crates/spur-mcp/src/server.rs
```

You are looking for:
- **Dispatch** — where `PlanTaskStatus::Dispatched { delegation_id }` is assigned to a task. Usually in the plan-executor dispatch loop. Emit after the transition.
- **Completion** — where `PlanTaskStatus::AwaitingReview { .. }` is assigned. Usually in the delegation-result-collector callback. Emit after the transition.
- **Approval / Rejection** — inside the `review_task` MCP tool handler in `server.rs`, in each of the `approve` and `request_changes` branches.

- [ ] **Step 2: Emit on Dispatch**

At the point where a task transitions to `Dispatched`, add:

```rust
if let Some(advanced) = self.pm.advanced() {
    crate::plan::audit::emit_dispatch(
        advanced,
        &task_issue_id, // the beads issue ID stored on the PlanTask
        crate::plan::audit::DispatchPayload {
            delegation_id: &delegation_id,
            worker: &agent_name,
            attempt: current_attempt,
        },
    )
    .await;
}
```

Variable names (`task_issue_id`, `delegation_id`, `agent_name`, `current_attempt`) depend on scope — match existing names in the dispatch path.

- [ ] **Step 3: Emit on Completion**

In the result-collector callback where the task transitions to `AwaitingReview`:

```rust
if let Some(advanced) = self.pm.advanced() {
    crate::plan::audit::emit_completion(
        advanced,
        &task_issue_id,
        crate::plan::audit::CompletionPayload {
            delegation_id: &delegation_id,
            worker_branch: worker_branch.as_deref(),
            diff_summary: diff_summary.as_deref(),
        },
    )
    .await;
}
```

- [ ] **Step 4: Emit on Approval**

In `handle_review_task` (or wherever `review_task` dispatches), in the `approve` branch:

```rust
if let Some(advanced) = self.pm.advanced() {
    crate::plan::audit::emit_approval(
        advanced,
        &task_issue_id,
        crate::plan::audit::ApprovalPayload {
            delegation_id: &delegation_id,
        },
    )
    .await;
}
```

- [ ] **Step 5: Emit on Rejection**

In the `request_changes` branch:

```rust
if let Some(advanced) = self.pm.advanced() {
    crate::plan::audit::emit_rejection(
        advanced,
        &task_issue_id,
        crate::plan::audit::RejectionPayload {
            delegation_id: &delegation_id,
            feedback: &feedback_str,
        },
    )
    .await;
}
```

- [ ] **Step 6: Add an integration test asserting all four records appear in sequence**

Create `crates/spur-mcp/tests/plan_audit_coverage.rs`:

```rust
//! Asserts that a normal plan-task lifecycle emits: PlanSubmit, Dispatch,
//! Completion, Approval audit records on the relevant issues.

// Follow the existing test harness pattern from submit_plan_persist.rs.
// Steps:
//   1. Submit a single-task plan
//   2. Drive the task to awaiting_review (fake worker completion)
//   3. Approve via review_task
//   4. Fetch `br audit log` for the task issue
//   5. Assert entries in order: Dispatch, Completion, Approval
//   6. Fetch audit log for the epic issue
//   7. Assert entry: PlanSubmit

// Adapt from harness in submit_plan_persist.rs. If the harness cannot drive
// a fake worker completion end-to-end, write the test against the plan/mod
// internal APIs directly (unit test) rather than the MCP tool surface.
```

Implement this test concretely by copying the needed setup helpers from `crates/spur-mcp/tests/submit_plan_persist.rs`. Assertions:

```rust
use spur_pm::AuditEntryType;
// After driving the lifecycle:
let task_log = harness.pm.advanced().unwrap().audit_log(&task_issue_id).await.unwrap();
let task_types: Vec<AuditEntryType> = task_log.iter().map(|e| e.entry_type.clone()).collect();
assert!(task_types.iter().any(|t| *t == AuditEntryType::Dispatch));
assert!(task_types.iter().any(|t| *t == AuditEntryType::Completion));
assert!(task_types.iter().any(|t| *t == AuditEntryType::Approval));

let epic_log = harness.pm.advanced().unwrap().audit_log(&epic_id).await.unwrap();
let epic_types: Vec<AuditEntryType> = epic_log.iter().map(|e| e.entry_type.clone()).collect();
assert!(epic_types.iter().any(|t| *t == AuditEntryType::PlanSubmit));
```

- [ ] **Step 7: Run the new test**

```bash
cargo test -p spur-mcp --test plan_audit_coverage
```

Expected: PASS. Also run the broader suite to confirm no regressions:

```bash
cargo test -p spur-mcp
```

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/tests/plan_audit_coverage.rs
git commit -m "feat(spur-mcp): emit audit records on dispatch/completion/approval/rejection"
```

---

## Task 15: Reconciler skeleton — `tokio::spawn` with adaptive cadence

**Files:**
- Create: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs`
- Modify: `crates/spur-mcp/src/server.rs` (spawn on startup)

- [ ] **Step 1: Create the reconciler module (skeleton with tick loop)**

`crates/spur-mcp/src/plan/reconciler.rs`:

```rust
//! Level-triggered reconciler for beads-backed plans.
//!
//! Ticks on an adaptive cadence: fast when there is activity, backing off
//! toward an idle ceiling when there is not. In v0a the reconciler only
//! observes/parity-checks beads state; it does NOT dispatch ACP work.
//! This module owns the loop and the cadence state machine.
//!
//! See spec §Architecture → Reconciler.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use spur_pm::BeadsAdvanced;

/// Tunable cadence parameters. Values chosen per spec §Scope & Phasing
/// and marked LOW-LEVERAGE (L12) — tune post-ship without spec revision.
pub struct ReconcilerConfig {
    pub base_interval: Duration,
    pub idle_ceiling: Duration,
    pub backoff_factor: u32,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            base_interval: Duration::from_secs(3),
            idle_ceiling: Duration::from_secs(30),
            backoff_factor: 2,
        }
    }
}

pub struct Reconciler {
    config: ReconcilerConfig,
    advanced: Arc<dyn BeadsAdvanced>,
    /// Notify the reconciler to fast-forward — e.g., brain just wrote a mutation.
    fast_forward: Arc<Notify>,
    /// Optional plan-id filter: if set, queries restrict to the persisted
    /// runtime label namespace chosen for v0a (currently expected to remain
    /// `spur.plan_id=<id>` unless a migration is explicitly added).
    plan_id: Option<String>,
}

impl Reconciler {
    pub fn new(
        config: ReconcilerConfig,
        advanced: Arc<dyn BeadsAdvanced>,
        fast_forward: Arc<Notify>,
        plan_id: Option<String>,
    ) -> Self {
        Self { config, advanced, fast_forward, plan_id }
    }

    /// Run the reconciler tick loop until `cancel` is awaited.
    pub async fn run(self, cancel: tokio::sync::oneshot::Receiver<()>) {
        let mut interval = self.config.base_interval;
        tokio::pin!(cancel);

        loop {
            tokio::select! {
                _ = &mut cancel => {
                    tracing::info!("reconciler received cancel");
                    break;
                }
                _ = self.fast_forward.notified() => {
                    tracing::debug!("reconciler fast-forward triggered");
                    interval = self.config.base_interval;
                }
                _ = tokio::time::sleep(interval) => {}
            }

            let did_work = match self.tick_once().await {
                Ok(work) => work,
                Err(e) => {
                    tracing::warn!("reconciler tick failed: {e}");
                    false
                }
            };

            if did_work {
                interval = self.config.base_interval;
            } else {
                let scaled = interval.saturating_mul(self.config.backoff_factor);
                interval = std::cmp::min(scaled, self.config.idle_ceiling);
            }
        }
    }

    /// One reconcile cycle. Returns `true` if any ready work was observed
    /// (used by the cadence controller to reset to base interval).
    /// Task 16 fills in the body.
    async fn tick_once(&self) -> anyhow::Result<bool> {
        // Task 16 fills this in with the `br ready` → dispatch logic.
        let _ = self.advanced; // suppress unused
        let _ = self.plan_id.as_deref();
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cadence_backs_off_on_idle_ticks() {
        // Verify the cadence backoff formula without touching beads.
        let cfg = ReconcilerConfig {
            base_interval: Duration::from_secs(1),
            idle_ceiling: Duration::from_secs(8),
            backoff_factor: 2,
        };

        let mut interval = cfg.base_interval;
        let mut history = vec![interval];
        for _ in 0..6 {
            interval = std::cmp::min(
                interval.saturating_mul(cfg.backoff_factor),
                cfg.idle_ceiling,
            );
            history.push(interval);
        }
        assert_eq!(
            history,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
    }
}
```

- [ ] **Step 2: Register submodule**

In `crates/spur-mcp/src/plan/mod.rs`, add:

```rust
pub mod reconciler;
```

- [ ] **Step 3: Spawn the reconciler from server startup**

In `crates/spur-mcp/src/server.rs`, find the existing server-start path (look for `tokio::spawn` patterns around where other long-running tasks are launched — search `grep -n "task_tracker.spawn\|tokio::spawn" crates/spur-mcp/src/server.rs`). Spawn the reconciler alongside them:

```rust
// During server init, after PmService is constructed:
if let Some(advanced) = self.pm.advanced_arc() {
    use std::sync::Arc;

    let fast_forward = Arc::new(tokio::sync::Notify::new());
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    self.reconciler_cancel = Some(cancel_tx);
    self.reconciler_fast_forward = Some(fast_forward.clone());

    let reconciler = crate::plan::reconciler::Reconciler::new(
        crate::plan::reconciler::ReconcilerConfig::default(),
        advanced,
        fast_forward,
        None, // no plan filter — tick covers only plans admitted by the chosen runtime gate
    );
    self.task_tracker.spawn(reconciler.run(cancel_rx));
}
```

Add `PmService::advanced_arc()`:

In `crates/spur-pm/src/service.rs`, change the backend inner to store an `Arc<BeadsAdapter>` instead of `BeadsAdapter`:

```rust
use std::sync::Arc;

enum PmBackendInner {
    Beads {
        beads: Arc<BeadsAdapter>,
        github: Option<GitHubAdapter>,
    },
    GitHub {
        adapter: GitHubAdapter,
    },
}
```

Update `try_new` to wrap the constructed `BeadsAdapter` in `Arc::new(...)`. Update `advanced()` and all other accessors to clone or deref the `Arc` as appropriate. Add a new accessor:

```rust
pub fn advanced_arc(&self) -> Option<Arc<dyn crate::advanced::BeadsAdvanced>> {
    match &self.inner {
        PmBackendInner::Beads { beads, .. } => {
            Some(beads.clone() as Arc<dyn crate::advanced::BeadsAdvanced>)
        }
        PmBackendInner::GitHub { .. } => None,
    }
}
```

`BeadsAdapter` must implement `BeadsAdvanced` on `Arc<Self>` — the `#[async_trait]` on the trait handles this when the struct is inside an `Arc`. If compile fails on `Arc<BeadsAdapter> as Arc<dyn BeadsAdvanced>`, explicitly implement `BeadsAdvanced` for `Arc<BeadsAdapter>` with a delegating impl:

```rust
#[async_trait]
impl BeadsAdvanced for Arc<BeadsAdapter> {
    async fn list_ready(&self, f: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
        (**self).list_ready(f).await
    }
    // ... delegate all methods ...
}
```

(Alternatively, skip the `Arc<dyn>` coercion by keeping `PmService::advanced()` returning `&dyn BeadsAdvanced` and passing ownership some other way. The `Arc` approach keeps the reconciler decoupled from `PmService` lifetime.)

- [ ] **Step 4: Run unit tests**

```bash
cargo test -p spur-mcp --lib plan::reconciler::tests
```

Expected: 1 passed (cadence-backoff formula).

Also make sure the overall crate still builds:

```bash
cargo check -p spur-mcp
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/service.rs crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): reconciler skeleton with adaptive cadence + Arc-accessible BeadsAdvanced"
```

---

## Task 16: Reconciler `tick_once` — `br ready` observation/parity scan

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Create: `crates/spur-mcp/tests/reconciler_tick.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/spur-mcp/tests/reconciler_tick.rs`:

```rust
//! Integration test for reconciler tick_once using real beads.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_pm::{BeadsAdapter, BeadsAdvanced, IssueCreate};
use tempfile::TempDir;

fn br_available() -> bool {
    Command::new("br").arg("--help").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("br").args(args).arg("--json").current_dir(repo)
        .output().expect("br");
    assert!(out.status.success(), "br {:?} failed: {:?}", args, out);
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn reconciler_tick_dispatches_only_ready_tasks() {
    if !br_available() { return; }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let adapter = Arc::new(
        BeadsAdapter::connect_with_actor(dir.path(), Some("reconciler".into()), None)
            .await.unwrap(),
    );

    // Create an epic + 2 tasks, one blocked.
    use spur_pm::IssueTracker;
    let epic = adapter.create_issue(IssueCreate {
        title: "Epic".into(),
        issue_type: Some("epic".into()),
        labels: vec!["spur.plan_id=P1".into()],
        ..Default::default()
    }).await.unwrap();
    let a = adapter.create_issue(IssueCreate {
        title: "A".into(),
        issue_type: Some("task".into()),
        labels: vec!["spur.plan_id=P1".into(), "spur.plan_task_id=A".into()],
        parent: Some(epic.clone()),
        ..Default::default()
    }).await.unwrap();
    let b = adapter.create_issue(IssueCreate {
        title: "B".into(),
        issue_type: Some("task".into()),
        labels: vec!["spur.plan_id=P1".into(), "spur.plan_task_id=B".into()],
        parent: Some(epic.clone()),
        depends_on: vec![a.clone()],
        ..Default::default()
    }).await.unwrap();

    // Directly call ready_for_test via a Reconciler instance — must only surface A.
    use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig};
    let advanced: Arc<dyn BeadsAdvanced> = adapter.clone();
    let fast = Arc::new(tokio::sync::Notify::new());
    let recon = Reconciler::new(
        ReconcilerConfig::default(),
        advanced.clone(),
        fast,
        Some("P1".to_string()),
    );

    let ready_ids = recon.ready_for_test().await.unwrap();
    assert!(ready_ids.contains(&a), "A should be ready");
    assert!(!ready_ids.contains(&b), "B should be blocked");
}
```

Note: this test uses a NEW `ready_for_test()` method on Reconciler to inspect what `tick_once` observes, rather than driving real dispatch (which remains owned by the current executor in v0a). Keep the test aligned with the actual persisted runtime label namespace; do not hard-code a new namespace that production code does not yet emit.

Also: `BeadsAdapter: BeadsAdvanced` via the `Arc` delegation from Task 15 must be in place before this test compiles.

- [ ] **Step 2: Run — should fail: `ready_for_test` doesn't exist, `tick_once` stubbed to return false**

```bash
cargo test -p spur-mcp --test reconciler_tick
```

Expected: compile error.

- [ ] **Step 3: Implement `tick_once` and add `ready_for_test`**

Replace the stub `tick_once` in `crates/spur-mcp/src/plan/reconciler.rs`:

```rust
    async fn tick_once(&self) -> anyhow::Result<bool> {
        let ready = self.ready_for_test().await?;
        if ready.is_empty() {
            return Ok(false);
        }

        // Task 16 v0a scope: the reconciler ONLY detects ready tasks; actual
        // dispatch of ACP delegations remains in the existing plan executor
        // path. This loop logs each ready task as a parity/observation signal.
        for id in &ready {
            tracing::debug!(task_id = %id, "reconciler observed ready task");
        }
        Ok(true)
    }

    /// Test accessor: returns the list of ready-task issue IDs per current config.
    /// Public so integration tests can exercise the `br ready` filter logic
    /// without needing the full dispatch wiring.
    pub async fn ready_for_test(&self) -> anyhow::Result<Vec<String>> {
        let filter = spur_pm::ReadyFilter {
            labels_all: self
                .plan_id
                .as_ref()
                .map(|p| vec![format!("spur.plan_id={p}")])
                .unwrap_or_default(),
            limit: Some(50),
            ..Default::default()
        };
        let summaries = self.advanced.list_ready(filter).await?;
        Ok(summaries.into_iter().map(|s| s.id).collect())
    }
```

- [ ] **Step 4: Run the integration test**

```bash
cargo test -p spur-mcp --test reconciler_tick
```

Expected: PASS (1 test).

- [ ] **Step 5: Run the full test suite**

```bash
cargo test -p spur-pm -p spur-mcp
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "feat(spur-mcp): reconciler tick_once surfaces ready tasks via br ready"
```

---

## Verification

At the end of v0a, run the full verification sweep:

- [ ] **Step 1: Lint**

```bash
cargo clippy -p spur-pm -p spur-mcp --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 2: Format**

```bash
cargo fmt --all -- --check
```

Expected: no diff.

- [ ] **Step 3: Full test suite**

```bash
cargo test -p spur-pm -p spur-mcp
```

Expected: all green.

- [ ] **Step 4: Manual smoke test — observe the chosen breadcrumb transport on a real plan**

Invoke SPUR end-to-end with a small plan (via the MCP tool interface or a CLI harness that exists). Then inspect:

```bash
# If v0a.2 chose br audit:
br audit log <epic-issue-id>

# If v0a.2 chose SPUR-owned breadcrumbs instead:
br comments list <epic-issue-id>
```

Expected: the chosen transport shows `plan-submit`; each child task shows `dispatch`, `completion`, and `approval` (or `rejection`) depending on review outcome.

- [ ] **Step 5: Update AGENTS.md with SPUR signal convention**

Run `br agents --update` if needed, then review the AGENTS.md delta. Document the `[[spur-signal v1]]` sentinel format and the runtime label vocabulary actually used in v0a (`signal:*`, `mutation-id:*`, and if retained, `spur.plan_id=*` / `spur.plan_task_id=*`) so other `br`-aware agents understand the convention.

- [ ] **Step 6: Final commit**

```bash
git add AGENTS.md
git commit -m "docs(agents): document SPUR signal + label convention for br-aware agents"
```

---

## Self-review

**Spec coverage check.** Walking the spec §Goals/v0a section:

- ✅ `BeadsAdvanced` trait + methods (list_ready / list_comments / add_comment / audit_record / audit_log / remove_dependency / dep_cycles) — Tasks 1–5
- ✅ Actor threading (`--actor` on every call, `default_actor` ctor field) — Task 7
- ✅ Establish `br ready` observation/parity path for persisted beads-backed plans — Tasks 15–16
- ⚠️ Audit transport for submit/dispatch/completion/approval/rejection remains gated pending the v0a.2 transport decision — Tasks 12–14
- ✅ Fix F1 (cursor race) — Task 8
- ✅ Fix F2 (disk-backed cursor) — Task 9
- ✅ AGENTS.md entry — Task Verification-Step 5

**Execution note.** The safest cut line after this review is:

- **v0a.1**: adapter extensions, actor threading, correlation fixes, and boundary-safe cursor redesign.
- **v0a.2**: audit transport after schema validation, plus reconciler observation/parity work gated to persisted plans.

Do not start v0a.2 until v0a.1 lands and the runtime label/correlation story is explicit.

**Placeholder scan.** No TBDs or `todo!()` placeholders remain in action steps.

**Type consistency.**
- `AuditEntryType::PlanSubmit` used in Tasks 1, 4, 12, 13 — consistent.
- `ReadyFilter` shape used in Tasks 1, 2, 7, 16 — consistent.
- `WorkerSignal::ScopeDrift` defined in Task 11; not consumed in v0a (v0b concern) — correct.
- `BeadsAdvanced` trait methods match the spec's §Interfaces section.
- `default_actor: Option<String>` field introduced in Task 7, consumed by cursor fixes in Task 9 — consistent.

No issues found.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-20-adaptive-plan-repair-v0a.md`.**

**Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Each task's TDD loop (test → fail → implement → pass → commit) is one subagent turn.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`, with checkpoints after Tasks 5 / 10 / 14 / 16 for review.

**Which approach?**

---

*v0b (adaptive mutation — layers δ + ε) gets its own spec/plan cycle after v0a ships and we have real `br audit` trails from production use to inform the mutation design.*
