# /explore Phase 2 Implementation Plan — ExploreView TUI + plan/loop materialization + Manage lenses

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Build/test ONLY through `scripts/spur-cargo` (remote-default). TDD cadence: `test(...)` commit first, then `feat(...)`.

**Goal:** Complete the /explore feature per spec `docs/superpowers/specs/2026-07-07-explore-command-design.md` §11 Phase 2: `ExploreBrowser` TUI view (six-stage journey), `/explore` slash + palette entry, `skills` threading on the plan-task and loop-generation dispatch paths, per-dispatch materialization records powering the Manage "Last materialization" lens, and the two e2e journeys.

**Architecture:** Phase-1 engine (`crates/spur-core/src/explore/`) is reused as-is; the TUI calls it synchronously (spur-tui already depends on spur-core; catalog/manifest are small local files, no network per spec §9 — this deliberately avoids new SpurEvent plumbing and the broadcast-sizing invariants). Plan-path skills ride the existing `PlanTask` → reconciler → `DelegationRequest` pipe; loops get coverage for free because loop generations dispatch through `PlanTask` + `submit_plan_normalize_tasks` (`plan/loops/scheduler.rs:429-432`).

**Tech Stack:** Rust 2021, ratatui (`TestBackend` + golden files), serde/schemars, existing e2e layer (vhs + shell-use).

---

## Locked shared vocabulary (all tasks MUST use these exact names)

```rust
// spur-tui
ViewId::ExploreBrowser                                   // new variant, action.rs
pub struct ExploreBrowserView { ... }                    // views/explore/mod.rs
pub enum ExploreTab { Skills, Agents }
pub enum ExploreStage { Browse, Gate, Manage }
pub enum ManageLens { Pool, LastMaterialization }

// spur-core (crates/spur-core/src/explore/)
pub struct MaterializationRecord {                       // materialize.rs
    pub recorded_at_epoch: u64,
    pub delegation_id: String,
    pub agent: String,
    pub worktree: String,
    pub items: Vec<String>,
}
pub struct MaterializeMeta<'a> { pub request_id: &'a str, pub agent: &'a str }
pub fn append_materialization_record(repo_root: &Path, record: &MaterializationRecord) -> anyhow::Result<()>;
pub fn read_recent_materializations(repo_root: &Path, limit: usize) -> Vec<MaterializationRecord>;
// record file: <repo_root>/.spur/explore/cache/materializations.jsonl  (dir already gitignored)

pub fn validate_skill_names(repo_root: Option<&Path>, skills: &[String]) -> Result<(), String>;
// hoisted from mcp/delegation.rs `validate_explore_skills` (currently private, delegation.rs:717)

// spur-core plan types
PlanTask.skills: Option<Vec<String>>                     // plan/mod.rs:44 struct, after `profile`
```

Verdict strings accepted for materialization stay `"clean" | "overridden" | "replaced-bundled"` (matches `should_materialize`, materialize.rs).

## Verified substrate (re-grounded 2026-07-08 against main post-b3e9620e5)

**Engine API (phase 1, reuse as-is):**
- `Catalog::load/save`, `CatalogEntry`, `ItemKind` — `explore/catalog.rs:6,16,28,34,46`
- `Manifest::load/save`, `ManifestItem`, `GateRecord`, `SourceSpec`, `StatusReport`, `pool_dir`, `vendor`, `status`, `item_from_entry` — `explore/pool.rs:10-140`
- `gate::{Verdict{Clean,Flagged{reasons},Conflict{bundled_id}}, scan_body, scan_scripts, check_conflict, evaluate}` — `explore/gate.rs:19-72`
- `apply::{Resolution{Accept,Override{justification},ReplaceBundled,Skip}, Selection{entry,resolution}, ApplyOutcome{installed,skipped}, apply(root,&mut Manifest,&[Selection],&[String]), remove}` — `explore/apply.rs:9-82`
- `materialize::{adapter_for_kind, materialize_pool_skills}` — `explore/materialize.rs:13,31` (call site `orchestrator/delegation/worker_attempt.rs:694-701`)

**Dispatch paths:**
- `PlanTask` — `plan/mod.rs:44` (`profile` field :48-49); submit_plan JSON schema `profile` property — `mcp/plan.rs:438`
- Reconciler `skills: None` — `plan/reconciler/mod.rs:1502`
- Loop generations → `Vec<PlanTask>` + `submit_plan_normalize_tasks` — `plan/loops/scheduler.rs:429-432`; template built from `PlanTask` — `plan/loops/doctor.rs:255`
- `parse_parallel_tasks` `skills: None` — `server/types.rs:535`; `DelegateToWorkerInput.skills` precedent — `tool_schemas.rs:65-72`
- `validate_explore_skills` (private) — `mcp/delegation.rs:717`; delegate call site :152-156

**TUI wiring precedents (LoopBrowser is the freshest, landed with UX plan 689837c0):**
- `ViewId` enum — `crates/spur-tui/src/action.rs:317-331`
- `NavigateTo` lazy-create pattern — `app/action_routing/nav.rs:91-96`
- Per-view input routing arms — `app/input.rs:250,254,365,559`
- Render/tick dispatch arms — `app/mod.rs:562,675`; status-bar hints — `components/status_bar.rs:301`
- Slash+palette entry — `commands/spur_local.rs:56-159` (`configure`/`sprints` use `Dispatch::SpurLocal(Action::NavigateTo(ViewId::...))`); the Ctrl+K palette lists the same `CommandRegistry` entries (`app/overlays.rs:154`), so ONE `CommandEntry` covers both surfaces
- Navigation test — `app/tests/navigation_tests.rs:165`
- Golden idiom — `crates/spur-tui/tests/render_golden.rs` (committed `.txt` goldens, re-record `UPDATE_GOLDEN=1`)
- Directory-module view precedents — `views/dashboard/`, `views/session_detail/`

**Known debt this plan clears first:** spur-tui fails `clippy -D warnings` with 7 pre-existing lints (tracked bd-sm0hp): `clippy::large_enum_variant` on `Action` (action.rs:31), `UserInput` (app/mod.rs:91), `Dispatch` (commands/entry.rs:38), `SubmitDecision` (commands/submit_router.rs:33), `PalettePayload` (components/palette.rs:29), `PaletteIntent` (components/palette.rs:366); plus one `clippy::option_as_ref_cloned` (run clippy to locate, ~line 473 of its file). Because cargo lints all path deps, this blocks `-D warnings` for every crate that links spur-tui — it must be task exp2-1.

---

### Task exp2-1: clear spur-tui clippy debt (unblocks -D warnings for the whole plan)

**Files:**
- Modify: `crates/spur-tui/src/action.rs:31`, `crates/spur-tui/src/app/mod.rs:91`, `crates/spur-tui/src/commands/entry.rs:38`, `crates/spur-tui/src/commands/submit_router.rs:33`, `crates/spur-tui/src/components/palette.rs:29,366`, plus the one `option_as_ref_cloned` site.

- [ ] **Step 1: reproduce** — `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings`. Expected: FAIL with exactly the 7 errors listed above (6× large_enum_variant + 1× option_as_ref_cloned).

- [ ] **Step 2: fix the `option_as_ref_cloned` site mechanically** — replace `.as_ref().cloned()` with `.clone()` on the `Option` exactly as the clippy suggestion prints.

- [ ] **Step 3: resolve the six `large_enum_variant` errors with a scoped allow + justification** (do NOT Box variants — that ripples through hundreds of construction sites owned by parallel work). On each of the six enums add:

```rust
#[expect(
    clippy::large_enum_variant,
    reason = "transient UI action/payload enums; instances are short-lived and never stored in bulk, boxing would churn every construction site"
)]
```

Use `#[expect(...)]` (not `allow`) so the suppression self-reports if the lint ever stops firing. If any of the six does NOT currently fire (clippy output is the source of truth), skip that enum — an unfulfilled expectation is itself a `-D warnings` error.

- [ ] **Step 4: verify** — `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings` → exit 0, and `scripts/spur-cargo test -p spur-tui` → green.

- [ ] **Step 5: commit** `chore(spur-tui): exp2-1 clear clippy debt with expects and clone fix` (body: "Resolves bd-sm0hp: option_as_ref_cloned fixed; six action-enum large_enum_variant lints suppressed with justification — boxing deferred, see issue.")

---

### Task exp2-2: materialization records + validation hoist (spur-core engine)

**Files:**
- Modify: `crates/spur-core/src/explore/materialize.rs` (records + `MaterializeMeta` param)
- Modify: `crates/spur-core/src/explore/mod.rs` (add `validate_skill_names`)
- Modify: `crates/spur-core/src/mcp/delegation.rs:152-156,717` (rewire to the hoisted fn)
- Modify: `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs:694-701` call site + its `attempt_materializes_pool_skills` test (pass meta)

- [ ] **Step 1: failing tests** (in `materialize.rs` tests module and `explore/mod.rs` tests):

```rust
// materialize.rs tests — extend materialize_writes_subset_and_registers_excludes-style setup
#[tokio::test]
async fn materialize_appends_record_readable_by_reader() {
    // same repo/worktree fixture as materialize_writes_subset_and_registers_excludes
    // call with meta:
    materialize_pool_skills(
        &manager, worktree.path(), AgentKind::CodexAcp, repo.path(), None,
        Some(MaterializeMeta { request_id: "del-42", agent: "codex" }),
    ).await;
    let records = read_recent_materializations(repo.path(), 10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].delegation_id, "del-42");
    assert_eq!(records[0].agent, "codex");
    // items = every accepted pool item rendered in this dispatch, sorted
    assert_eq!(records[0].items, vec!["clean-a", "clean-b", "reviewed"]);
    assert!(records[0].recorded_at_epoch > 0);
}

#[tokio::test]
async fn materialize_without_meta_or_items_writes_no_record() {
    // meta: None → no record file; also Some(meta) but zero accepted items → no record
    materialize_pool_skills(&manager, wt.path(), AgentKind::CodexAcp, repo.path(), None, None).await;
    assert!(read_recent_materializations(repo.path(), 10).is_empty());
}

#[test]
fn read_recent_returns_newest_first_and_respects_limit() {
    for i in 0..5 {
        append_materialization_record(root, &MaterializationRecord {
            recorded_at_epoch: 100 + i, delegation_id: format!("d{i}"),
            agent: "codex".into(), worktree: "/w".into(), items: vec!["s".into()],
        }).unwrap();
    }
    let r = read_recent_materializations(root, 3);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].delegation_id, "d4"); // newest first
}

// explore/mod.rs tests
#[test]
fn validate_skill_names_matches_delegation_semantics() {
    // manifest with one "blocked" item (write via pool::Manifest::save fixture)
    assert!(validate_skill_names(Some(root), &[]).is_ok());
    assert!(validate_skill_names(None, &["x".into()]).unwrap_err().contains("repository root unavailable"));
    let e = validate_skill_names(Some(root), &["not-in-pool".into()]).unwrap_err();
    assert!(e.contains("not-in-pool") && e.contains("spur explore"));
    let e = validate_skill_names(Some(root), &["blocked-skill".into()]).unwrap_err();
    assert!(e.contains("blocked-skill") && e.contains("blocked"));
}
```

- [ ] **Step 2: run → fail; commit** `test(spur-core): exp2-2a add materialization record and validation tests`

- [ ] **Step 3: implement**

```rust
// materialize.rs — signature change (update ALL call sites: worker_attempt.rs:694,
// existing materialize.rs tests pass None, worker_attempt attempt_materializes_pool_skills passes Some)
pub async fn materialize_pool_skills(
    worktrees: &spur_worktree::manager::WorktreeManager,
    worktree_path: &Path,
    kind: spur_acp::types::AgentKind,
    repo_root: &Path,
    requested: Option<&[String]>,
    meta: Option<MaterializeMeta<'_>>,
) {
    // ... existing body unchanged until after successful add_worktree_excludes ...
    // on the success path (excludes registered), when meta is Some and rendered items non-empty:
    //   let mut items: Vec<String> = <names of items written this call>; items.sort();
    //   let record = MaterializationRecord { recorded_at_epoch: SystemTime::now()
    //       .duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), ... };
    //   if let Err(error) = append_materialization_record(repo_root, &record) {
    //       tracing::warn!(target: WARN_TARGET, error = %error, "materialization record write failed");
    //   }
}

pub fn append_materialization_record(repo_root: &Path, record: &MaterializationRecord) -> anyhow::Result<()> {
    let dir = repo_root.join(".spur/explore/cache");
    std::fs::create_dir_all(&dir)?;
    let line = serde_json::to_string(record)?;
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().create(true).append(true)
        .open(dir.join("materializations.jsonl"))?;
    writeln!(f, "{line}")?;
    Ok(())
}

pub fn read_recent_materializations(repo_root: &Path, limit: usize) -> Vec<MaterializationRecord> {
    let path = repo_root.join(".spur/explore/cache/materializations.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else { return Vec::new() };
    let mut records: Vec<MaterializationRecord> = raw.lines()
        .filter_map(|l| serde_json::from_str(l).ok()).collect();
    records.reverse(); // newest last on disk → newest first out
    records.truncate(limit);
    records
}
```

`MaterializationRecord` derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`.

```rust
// explore/mod.rs — hoist: move the body of mcp/delegation.rs validate_explore_skills here,
// message text UNCHANGED, returning Result<(), String> (the plain message string).
pub fn validate_skill_names(repo_root: Option<&Path>, skills: &[String]) -> Result<(), String> { ... }

// mcp/delegation.rs — validate_explore_skills becomes a thin wrapper:
fn validate_explore_skills(repo_root: Option<&std::path::Path>, skills: &[String]) -> Result<(), McpError> {
    crate::explore::validate_skill_names(repo_root, skills)
        .map_err(|message| McpError::invalid_params(message, None))
}
```

The existing `delegate_to_worker_rejects_unknown_or_ungated_skill` test in `mcp/delegation_tests.rs` must pass unchanged — it pins the message contract.

- [ ] **Step 4: run + clippy; commit** — `scripts/spur-cargo test -p spur-core explore::` and `-p spur-core mcp::` green; `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core -- -D warnings` exit 0. Commit `feat(spur-core): exp2-2b add materialization records and hoist skill validation`

---

### Task exp2-3: plan-path skills — PlanTask field, submit validation, reconciler threading, loop pass-through

**Files:**
- Modify: `crates/spur-core/src/plan/mod.rs:44` (PlanTask field)
- Modify: `crates/spur-core/src/mcp/plan.rs:438` region (submit_plan task JSON schema + submit-time validation)
- Modify: `crates/spur-core/src/plan/reconciler/mod.rs:1502`
- Test: reconciler/loop tests near existing dispatch tests in those files

- [ ] **Step 1: failing tests**

```rust
// plan/mod.rs tests (or the module's existing test file): serde round-trip
#[test]
fn plan_task_skills_roundtrip_and_default() {
    let json = r#"{"task_id":"t1","agent":"codex","task":"do","skills":["clean-a"]}"#;
    let t: PlanTask = serde_json::from_str(json).unwrap();
    assert_eq!(t.skills.as_deref(), Some(&["clean-a".to_string()][..]));
    let none: PlanTask = serde_json::from_str(r#"{"task_id":"t1","agent":"codex","task":"do"}"#).unwrap();
    assert!(none.skills.is_none());
    assert!(!serde_json::to_string(&none).unwrap().contains("skills")); // skip_serializing_if
}

// reconciler test, following the existing dispatch-construction test pattern in reconciler/mod.rs:
// build a plan whose task has skills=["clean-a"], drive one dispatch, assert the
// DelegationRequest sent on the channel carries skills == Some(vec!["clean-a"]).

// loops pass-through test in plan/loops/scheduler.rs tests: template task JSON containing
// "skills":["clean-a"] survives the collect at scheduler.rs:429 —
// deserialize the generated tasks and assert tasks[0].skills == Some(vec!["clean-a"]).
```

- [ ] **Step 2: run → fail; commit** `test(spur-core): exp2-3a add plan and loop skills threading tests`

- [ ] **Step 3: implement**

```rust
// plan/mod.rs — after `profile`:
#[serde(default, skip_serializing_if = "Option::is_none")]
pub skills: Option<Vec<String>>,
```

- `mcp/plan.rs`: add the schema property beside `profile` (:438):

```json
"skills": {
    "type": "array",
    "items": { "type": "string" },
    "description": "Explore pool skill names to materialize into this task's worker worktree before its session starts. Each name must exist in the explore manifest with an accepted gate verdict."
}
```

  and at submit parse time validate each task: `if let Some(skills) = task.skills.as_deref() { crate::explore::validate_skill_names(repo_root, skills) ... }` returning invalid_params naming the task_id AND the offending skill (prefix the hoisted message with `task '<task_id>': `). Locate the existing per-task validation section in the submit path (where agent names/profiles are checked) and add alongside.
- `plan/reconciler/mod.rs:1502`: `skills: None,` → `skills: task.spec.skills.clone(),`
- Loops: NO code change expected — the scheduler collects `PlanTask` via serde, so the field flows. The test from Step 1 proves it. If the collect at scheduler.rs:429 strips it (test fails), fix the template/task mapping minimally and note it in the commit body.
- No spur-acp domain type changes → no ACP round-trip test; state which in the commit body (repo rule).

- [ ] **Step 4: run + clippy; commit** — `scripts/spur-cargo test -p spur-core plan::` green, full `-p spur-core` green, clippy `-D warnings` exit 0. Commit `feat(spur-core): exp2-3b thread skills through plan tasks and loops`

---

### Task exp2-4: parallel-path skills — delegate_parallel task entries

**Files:**
- Modify: `crates/spur-core/src/tool_schemas.rs` (the parallel-task input struct beside its `profile` field — find it via `rg -n 'profile' crates/spur-core/src/tool_schemas.rs`)
- Modify: `crates/spur-core/src/server/types.rs:535`
- Modify: the delegate_parallel MCP handler in `crates/spur-core/src/mcp/delegation.rs` (validate each task's skills exactly like delegate_to_worker does at :152-156)
- Test: `crates/spur-core/src/mcp/delegation_tests.rs`

- [ ] **Step 1: failing test** — clone `delegate_to_worker_rejects_unknown_or_ungated_skill` into a `delegate_parallel_rejects_unknown_skill` variant: parallel args with one task carrying `"skills": ["not-in-pool"]` → error names the skill, suggests `spur explore`, nothing dispatched.

- [ ] **Step 2: run → fail; commit** `test(spur-core): exp2-4a add parallel skills validation test`

- [ ] **Step 3: implement** — add `skills: Option<Vec<String>>` (serde/schemars idiom copied from `DelegateToWorkerInput.skills`, tool_schemas.rs:65-72) to the parallel task input; `server/types.rs:535` `skills: None,` → `skills: task.skills,`; validate in the parallel handler before dispatch.

- [ ] **Step 4: run + clippy; commit** `feat(spur-core): exp2-4b thread skills through delegate_parallel`

---

### Task exp2-5: ExploreBrowser skeleton — ViewId, Browse tab, wiring, slash/palette entry

**Files:**
- Modify: `crates/spur-tui/src/action.rs:317-331` (ViewId variant)
- Create: `crates/spur-tui/src/views/explore/mod.rs` (+ declare in `views/mod.rs`)
- Modify: `crates/spur-tui/src/app/action_routing/nav.rs` (~:96, after LoopBrowser arm), `app/input.rs` (each `ViewId::LoopBrowser` arm at :254,366,560 gains an `ExploreBrowser` sibling), `app/mod.rs:562,675` render/tick arms, `components/status_bar.rs:301` hint arm
- Modify: `crates/spur-tui/src/commands/spur_local.rs` (append entry)
- Test: `crates/spur-tui/src/app/tests/navigation_tests.rs`, golden in `crates/spur-tui/tests/` following `render_golden.rs`

View state (Browse stage only in this task; Gate/Manage stubs render "coming in exp2-6/7" placeholders is NOT allowed — instead the Tab-cycle is restricted to Browse until later tasks add stages):

```rust
// views/explore/mod.rs
use spur_core::explore::{catalog::{Catalog, CatalogEntry, ItemKind}, pool::Manifest};

pub struct ExploreBrowserView {
    pub(crate) repo_root: std::path::PathBuf,
    pub(crate) tab: ExploreTab,
    pub(crate) stage: ExploreStage,
    pub(crate) catalog: Catalog,          // Catalog::load(&repo_root) — empty default on error
    pub(crate) manifest: Manifest,        // Manifest::load(&repo_root) — empty default on error
    pub(crate) selected: usize,           // catalog list cursor
    pub(crate) starred: std::collections::BTreeSet<String>, // pending pool additions (Select stage)
    pub(crate) load_error: Option<String>,
}

impl ExploreBrowserView {
    pub fn new(repo_root: std::path::PathBuf) -> Self { /* loads catalog+manifest, warn-not-panic */ }
    pub fn visible_entries(&self) -> Vec<&CatalogEntry> { /* filter by tab: Skills→ItemKind::Skill, Agents→ItemKind::Agent */ }
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<crate::action::Action> { /* j/k move, Tab switches tab, space toggles star, r reloads, Esc → NavigateTo(Dashboard) */ }
    pub fn render(&mut self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) { /* three panes: Sources | Catalog | Preview; badges: "in pool" (manifest hit), "★" (starred), "synced Xh ago"/"never synced" banner from catalog.synced_at_epoch */ }
}
```

repo_root: use `std::env::current_dir().unwrap_or_default()` at view creation (the TUI runs at the repo root; same assumption the theme loader makes). Preview pane: description always; body excerpt only when a vendored copy exists at `pool_dir(...)/SKILL.md`, otherwise the line `sync to fetch bodies — spur explore sync`.

- [ ] **Step 1: failing tests**

```rust
// app/tests/navigation_tests.rs — mirror the LoopBrowser test at :165
#[test]
fn navigate_to_explore_browser_and_back() {
    // app.process_action(Action::NavigateTo(ViewId::ExploreBrowser));
    // assert_eq!(app.current_view(), &ViewId::ExploreBrowser);
    // Esc → back to Dashboard
}

// commands test (same file as SpurLocalSource tests or submit_router sessions_slash_tests):
#[test]
fn slash_explore_routes_to_navigate() {
    // find entry named "explore" in SpurLocalSource::entries()
    // assert dispatch == Dispatch::SpurLocal(Action::NavigateTo(ViewId::ExploreBrowser))
}

// golden: new test file crates/spur-tui/tests/explore_browser_golden.rs following render_golden.rs:
// build ExploreBrowserView::new(tempdir with a 2-entry committed catalog fixture + 1-item manifest),
// render into TestBackend(100x30), join lines, compare to tests/goldens/explore_browser_browse.txt
// (UPDATE_GOLDEN=1 to record).
```

- [ ] **Step 2: run → fail; commit** `test(spur-tui): exp2-5a add explore browser navigation and golden tests`

- [ ] **Step 3: implement** — ViewId variant; view module; wiring:

```rust
// nav.rs, after the LoopBrowser arm (:91-96 pattern):
Action::NavigateTo(ViewId::ExploreBrowser) => {
    if self.explore_browser.is_none() {
        self.explore_browser = Some(ExploreBrowserView::new(
            std::env::current_dir().unwrap_or_default(),
        ));
    }
    self.navigate_to(ViewId::ExploreBrowser);
    None
}
```

```rust
// spur_local.rs entries(), after "sprints":
CommandEntry {
    name: "explore".into(),
    description: "Browse and adopt ecosystem skills & agent personas".into(),
    hint: None,
    source: CommandSource::Spur,
    dispatch: Dispatch::SpurLocal(Action::NavigateTo(crate::action::ViewId::ExploreBrowser)),
    arg_picker_spec: None,
},
```

Each `match` over `ViewId` that the compiler flags after adding the variant gets an `ExploreBrowser` arm mirroring its `LoopBrowser` sibling (input routing at input.rs:254/366/560, render+tick at app/mod.rs:562/675, status_bar.rs:301 with hint text `explore — Tab tabs · space select · r reload`).

- [ ] **Step 4: run + clippy; commit** — `scripts/spur-cargo test -p spur-tui` green (golden recorded and committed), `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings` exit 0 (exp2-1 made this meaningful). Commit `feat(spur-tui): exp2-5b add explore browser view with browse tab`

---

### Task exp2-6: Gate cards + Apply from the view

**Files:**
- Create: `crates/spur-tui/src/views/explore/gate.rs`
- Modify: `crates/spur-tui/src/views/explore/mod.rs`
- Test: extend `crates/spur-tui/tests/explore_browser_golden.rs`

Behavior: `Enter` on Browse with ≥1 starred item moves `stage = ExploreStage::Gate`. Gate renders one card per starred entry: pin sha7, license (or `unknown ⚠`), live `gate::evaluate(name, vendored_or_cache_path, &bundled_ids)` verdict, and a resolution selector. Key map on a card: `a` Accept (only when verdict is `Clean`), `o` Override (opens a one-line justification input; empty justification is rejected), `b` ReplaceBundled (only when verdict is `Conflict`), `s` Skip. `A` (shift) runs Apply over all resolved cards:

```rust
let selections: Vec<Selection> = resolved_cards.iter().map(|c| Selection {
    entry: c.entry.clone(),
    resolution: c.resolution.clone().expect("resolved"),
}).collect();
match spur_core::explore::apply::apply(&self.repo_root, &mut self.manifest, &selections, &bundled_ids) {
    Ok(outcome) => { self.apply_log = Some(outcome); self.manifest = Manifest::load(&self.repo_root).unwrap_or_default(); self.stage = ExploreStage::Browse; }
    Err(error) => { self.load_error = Some(format!("apply failed: {error:#}")); }
}
```

`bundled_ids`: obtain the same way `spur explore add` does in `crates/spur-cli/src/commands/explore.rs` (read that file and reuse its source of bundled skill ids via spur-core; do not invent a second list). Unresolved cards are excluded from Apply and the apply-log pane lists `installed` and `skipped(reason)` — spec §9.

- [ ] **Step 1: failing tests** — unit tests on the view (no golden yet): starring two fixture entries then `Enter` yields two gate cards; `o` with empty justification keeps the card unresolved; card resolutions map to the exact `Resolution` variants; `A` with one Accept + one Skip calls apply and the outcome lands in `apply_log` (use a tempdir repo fixture with a vendorable cache checkout like `explore/apply.rs` own tests do — copy their fixture helper pattern). Plus one golden `explore_browser_gate.txt`.

- [ ] **Step 2: run → fail; commit** `test(spur-tui): exp2-6a add gate card and apply tests`

- [ ] **Step 3: implement** per above.

- [ ] **Step 4: run + clippy; commit** `feat(spur-tui): exp2-6b add gate cards and apply flow`

---

### Task exp2-7: Manage lenses — Pool + Last materialization

**Files:**
- Create: `crates/spur-tui/src/views/explore/manage.rs`
- Modify: `crates/spur-tui/src/views/explore/mod.rs`
- Test: extend `crates/spur-tui/tests/explore_browser_golden.rs`

Behavior: `m` from Browse toggles `stage = ExploreStage::Manage`; `l` toggles `ManageLens::Pool ↔ LastMaterialization`.
- **Pool lens:** rows from `Manifest.items` (name, kind, sha7, verdict, license) + the `pool::status(&repo_root, &manifest)` `StatusReport` findings (missing body / sha mismatch) rendered as `⚠` rows; `x` on a row calls `apply::remove(&self.repo_root, &mut self.manifest, name)` then reloads the manifest; staleness banner from `catalog.synced_at_epoch`.
- **Last materialization lens:** `spur_core::explore::materialize::read_recent_materializations(&self.repo_root, 20)` rendered newest-first: `HH:MM · <agent> · <delegation_id sha-ish> · N skills: a, b, c` (epoch formatted with the view's existing time helper or plain `%H:%M` via chrono if spur-tui already depends on it — check Cargo.toml; if not, render the raw epoch, do NOT add a dependency).

- [ ] **Step 1: failing tests** — unit: manage lens lists manifest items and toggles lenses; remove (`x`) deletes the item from the manifest on disk; last-materialization lens shows records appended via `append_materialization_record` in a tempdir fixture, newest first. Golden: `explore_browser_manage_pool.txt` + `explore_browser_manage_lastmat.txt` (records with fixed epochs so goldens are stable).

- [ ] **Step 2: run → fail; commit** `test(spur-tui): exp2-7a add manage lens tests`

- [ ] **Step 3: implement** per above. **Depends on exp2-2** (reader API).

- [ ] **Step 4: run + clippy; commit** `feat(spur-tui): exp2-7b add manage pool and last-materialization lenses`

---

### Task exp2-8: e2e journeys — one behavioral + one visual

**Files:**
- Create: `scripts/e2e/shell-use/journeys/explore-browser-open.sh` (copy the structure of `scripts/e2e/shell-use/journeys/loop-browser-open.sh` — same fixture and shape)
- Create: `scripts/e2e/vhs/tapes/explore-browser-open.tape` (copy the structure of an existing `no-agents` tape, e.g. `scripts/e2e/vhs/tapes/cold-launch.tape`)
- Modify: `scripts/e2e/JOURNEYS.md` (two rows)

Journey story: from the `no-agents` dashboard, type `/explore` + Enter, assert the Explore browser renders (wait strings: the view title and the never-synced banner your exp2-5 render emits — read the implemented view to pick exact stable strings, e.g. `Explore` and `never synced`), Esc back, quit cleanly (`Quit spur?`). JOURNEYS.md rows follow the table's exact column format; authoring rule: looks → vhs tape, does → shell-use journey.

- [ ] **Step 1: author both files + JOURNEYS.md rows.**
- [ ] **Step 2: run** — `SPUR_E2E_ONLY=behavioral scripts/spur-cargo e2e` green (remote; failure artifacts sync to `scripts/e2e/.artifacts/`). Then `SPUR_E2E_ONLY=visual scripts/spur-cargo e2e` to record/verify the tape golden per the vhs layer's recording convention (read `scripts/e2e/vhs/` README/runner for how goldens are recorded on first run).
- [ ] **Step 3: commit** `test(e2e): exp2-8 add explore browser open journeys`

---

## Dependency graph

```
exp2-1 (tui lint debt)  ──────────────► exp2-5 (view skeleton) ──► exp2-6 (gate+apply) ──► exp2-8 (e2e)
exp2-2 (records+hoist) ──► exp2-3 (plan/loops)                └──► exp2-7 (manage) ──────► exp2-8
                       └──► exp2-4 (parallel)
```

## Self-review results (spec coverage)

- Spec §2 stages: Entry (exp2-5 slash/palette), Browse (exp2-5), Select (exp2-5 starring), Gate (exp2-6), Apply (exp2-6), Manage (exp2-7). ✓
- Spec §7 plan/loop materialization: exp2-3 (loops ride PlanTask — verified scheduler.rs:429); parallel path exp2-4. Subset re-evaluated between generations = generation authoring writes per-generation `skills`; within a generation tasks are fixed at submit. ✓
- Spec §8 TUI view + keybindings + offline banner: exp2-5/6/7. ✓
- Spec §9 error handling: offline banner (exp2-5), status report repair surfacing (exp2-7 Pool lens), gate-blocked exclusion visible (exp2-6 apply log). ✓
- Spec §10 testing: engine unit tests (exp2-2/3/4), TestBackend goldens (exp2-5/6/7), e2e pair (exp2-8). ✓
- Manage "Last materialization" data source: created in exp2-2 (records), consumed in exp2-7. ✓
- Deliberate scope holds (non-goals §12): no LLM scan, no auto-update, no palette quick-add, no human-IDE distribution. Sync stays CLI-only; the TUI never fetches. ✓
