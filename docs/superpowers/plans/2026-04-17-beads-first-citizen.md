# Local-First beads_rust as First Citizen — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make beads_rust (`br` CLI) the default issue tracker in spur-pm, with a TUI IssuesPanel, orchestrator workflow coupling (claim/comment/revert), and GitHub demoted to PR-only.

**Architecture:** Split `PmAdapter` into `IssueTracker` + `PrService` traits. Create `BeadsAdapter` (shells to `br --format json`). Replace `Box<dyn>` with internal `PmBackendInner` enum in `PmService`. MCP server calls `PmService` directly (eliminates `__pm_*` sentinel routing). Orchestrator auto-claims issues on delegation start, reverts on failure.

**Tech Stack:** Rust (stable), async-trait, tokio::process::Command, serde/serde_json, ratatui, chrono

**Spec:** `docs/superpowers/specs/2026-04-17-beads-first-citizen-design.md`

---

## File Map

### New Files
| File | Responsibility |
|---|---|
| `crates/spur-pm/src/beads.rs` | `BeadsAdapter` — shells to `br` CLI, implements `IssueTracker` |
| `crates/spur-pm/src/service.rs` | `PmService` — `PmBackendInner` enum dispatch, `try_new()` factory |
| `crates/spur-tui/src/components/issues_panel.rs` | `IssuesPanel` — stateless table renderer |

### Modified Files
| File | Change |
|---|---|
| `crates/spur-pm/src/adapter.rs` | Replace `PmAdapter` with `IssueTracker` + `PrService` traits |
| `crates/spur-pm/src/types.rs` | Enrich `Issue`, `IssueSummary`, `IssueFilter`, `IssueUpdate`; add `PmSource::Beads`; remove `linked_prs` |
| `crates/spur-pm/src/lib.rs` | Add `pub mod beads; pub mod service;` and update re-exports |
| `crates/spur-pm/src/github.rs` | Update `GitHubAdapter` to impl `IssueTracker + PrService` (replacing `PmAdapter`), add async constructor, add `created_at`/`updated_at` parsing |
| `crates/spur-pm/Cargo.toml` | No changes needed (deps already sufficient) |
| `crates/spur-mcp/Cargo.toml` | Add `spur-pm` dependency |
| `crates/spur-mcp/src/tools.rs` | Add `list_issues_def()`, update `get_issue_def` (source optional), update `update_issue_def` (add fields), add `issue_id` to delegation tools |
| `crates/spur-mcp/src/server.rs` | Add `pm_service: Option<Arc<PmService>>` field, replace `__pm_*` handlers with direct PmService calls, add `handle_list_issues` |
| `crates/spur-core/src/orchestrator.rs` | Add `pm_service: Option<Arc<PmService>>`, remove `handle_pm_operation()`, add workflow coupling hooks (claim/revert), emit `IssuesLoaded` at start |
| `crates/spur-acp/src/domain/events.rs` | Add `IssuesLoaded` variant, add `IssueSummaryEvent` type, add `assignee` to `IssueUpdated` |
| `crates/spur-acp/src/config/mod.rs` | Add `BeadsPmConfig` to `PmConfig` |
| `crates/spur-tui/src/views/dashboard.rs` | Add `tracked_issues` field, handle `IssuesLoaded`/`IssueUpdated`, render `IssuesPanel` |
| `crates/spur-tui/src/components/mod.rs` | Add `pub mod issues_panel;` |
| `crates/spur-tui/src/components/status_bar.rs` | Add issues count to status bar props |

---

## Task 1: Enrich spur-pm Type Model

**Files:**
- Modify: `crates/spur-pm/src/types.rs`

- [ ] **Step 1: Replace types.rs with beads-native type model**

Replace the entire content of `crates/spur-pm/src/types.rs` with:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmSource {
    Beads,
    GitHub,
    Linear,
    Plane,
}

impl std::fmt::Display for PmSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PmSource::Beads => write!(f, "beads"),
            PmSource::GitHub => write!(f, "github"),
            PmSource::Linear => write!(f, "linear"),
            PmSource::Plane => write!(f, "plane"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub body: String,
    pub status: String,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub status: String,
    pub labels: Vec<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub priority_min: Option<i32>,
    pub priority_max: Option<i32>,
    pub issue_type: Option<String>,
    pub text_search: Option<String>,
    /// None = backend default (typically 50)
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueUpdate {
    pub status: Option<String>,
    pub comment: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub priority: Option<i32>,
    /// Some("alice") = assign, Some("") = unassign, None = no change
    pub assignee: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrParams {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PmEvent {
    IssueCreated(IssueSummary),
    IssueUpdated(IssueSummary),
}
```

- [ ] **Step 2: Verify spur-pm compiles (expect errors in github.rs and adapter.rs)**

Run: `cargo check -p spur-pm 2>&1 | head -30`

Expected: Compilation errors in `github.rs` (missing `linked_prs`, changed field types) and `adapter.rs` (old `PmAdapter` references `types::*` which changed). This is expected — we fix these in Tasks 2 and 3.

- [ ] **Step 3: Commit type model**

```bash
git add crates/spur-pm/src/types.rs
git commit -m "refactor(spur-pm): enrich type model for beads-first citizen

- Add PmSource::Beads variant (first position)
- Add priority, issue_type, blocked_by, due_at, created_at, updated_at to Issue
- Add priority, issue_type, assignee to IssueSummary
- Add priority_min/max, issue_type, text_search, limit to IssueFilter
- Add priority, assignee to IssueUpdate
- Remove linked_prs (dead field, never populated)
- Remove claim: bool (use explicit status + assignee)
- Make created_at/updated_at required (both backends provide)"
```

---

## Task 2: Split PmAdapter into IssueTracker + PrService Traits

**Files:**
- Modify: `crates/spur-pm/src/adapter.rs`

- [ ] **Step 1: Replace adapter.rs with split traits**

Replace the entire content of `crates/spur-pm/src/adapter.rs`:

```rust
use async_trait::async_trait;

use crate::types::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PrParams};

#[async_trait]
pub trait IssueTracker: Send + Sync {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue>;
    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>>;
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()>;
    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>>;
}

#[async_trait]
pub trait PrService: Send + Sync {
    async fn create_pr(&self, params: PrParams) -> anyhow::Result<String>;
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/spur-pm/src/adapter.rs
git commit -m "refactor(spur-pm): split PmAdapter into IssueTracker + PrService

IssueTracker: 4 methods (get, list, update, poll)
PrService: 1 method (create_pr)
No connect() — async constructors on concrete types.
No source() — caller knows. No search() — subsumed by filter."
```

---

## Task 3: Update GitHubAdapter for New Traits

**Files:**
- Modify: `crates/spur-pm/src/github.rs`

- [ ] **Step 1: Update GitHubAdapter imports and trait impls**

The existing `GitHubAdapter` implements `PmAdapter`. Change it to implement `IssueTracker + PrService`. Key changes:

1. Replace `use crate::adapter::PmAdapter;` with `use crate::adapter::{IssueTracker, PrService};`
2. Remove `connect(&mut self)` from the trait impl — move it to an async constructor `pub async fn connect(repo: Option<String>, cwd: &Path) -> anyhow::Result<Self>`
3. Change `impl PmAdapter for GitHubAdapter` to two blocks: `impl IssueTracker for GitHubAdapter` and `impl PrService for GitHubAdapter`
4. Add `created_at` and `updated_at` parsing in the `From<GhIssueView> for Issue` conversion (parse from `createdAt`/`updatedAt` fields in gh JSON)
5. Remove `linked_prs: Vec::new()` from the From conversion
6. Add `blocked_by: Vec::new()`, `due_at: None`, `issue_type: None` to the From conversion
7. In `From<GhIssueListItem> for IssueSummary`, add `priority: None`, `issue_type: None`, `assignee: None`

The `GhIssueView` and `GhIssueListItem` deser structs need `created_at` and `updated_at` fields:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssueView {
    number: u64,
    title: String,
    body: Option<String>,
    labels: Vec<GhLabel>,
    state: String,
    assignees: Vec<GhAssignee>,
    url: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}
```

Parse timestamps in `From<GhIssueView> for Issue`:
```rust
fn parse_gh_time(s: &Option<String>) -> DateTime<Utc> {
    s.as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}
```

- [ ] **Step 2: Verify spur-pm compiles**

Run: `cargo check -p spur-pm`
Expected: Success (or only downstream errors in spur-core/spur-mcp which reference old `PmAdapter`)

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/src/github.rs
git commit -m "refactor(spur-pm): update GitHubAdapter for IssueTracker + PrService

- Async constructor replaces connect(&mut self)
- Impl IssueTracker + PrService separately
- Parse created_at/updated_at from gh JSON
- Add beads-native fields (None/empty for GitHub)"
```

---

## Task 4: Update lib.rs Re-exports

**Files:**
- Modify: `crates/spur-pm/src/lib.rs`

- [ ] **Step 1: Update lib.rs**

Replace content of `crates/spur-pm/src/lib.rs`:

```rust
pub mod adapter;
pub mod beads;
pub mod github;
pub mod service;
pub mod types;

pub use adapter::{IssueTracker, PrService};
pub use beads::BeadsAdapter;
pub use github::GitHubAdapter;
pub use service::PmService;
pub use types::*;
```

- [ ] **Step 2: Create empty beads.rs and service.rs stubs**

Create `crates/spur-pm/src/beads.rs`:
```rust
// BeadsAdapter — implemented in Task 5
```

Create `crates/spur-pm/src/service.rs`:
```rust
// PmService — implemented in Task 6
```

- [ ] **Step 3: Verify spur-pm compiles**

Run: `cargo check -p spur-pm`
Expected: Success

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/lib.rs crates/spur-pm/src/beads.rs crates/spur-pm/src/service.rs
git commit -m "refactor(spur-pm): update lib.rs re-exports for new module layout"
```

---

## Task 5: Implement BeadsAdapter

**Files:**
- Create: `crates/spur-pm/src/beads.rs`

- [ ] **Step 1: Write BeadsAdapter**

Replace the stub `crates/spur-pm/src/beads.rs` with the full implementation. The file should contain:

1. `BeadsAdapter` struct with `cwd: PathBuf` and `last_poll: std::sync::Mutex<Option<DateTime<Utc>>>`
2. `pub async fn connect(repo_root: &Path) -> anyhow::Result<Self>` — runs `br version` + `br stats`
3. `async fn run_br(&self, args: Vec<String>) -> anyhow::Result<String>` — bounded retry (2 attempts, 50ms)
4. `async fn run_br_once(&self, args: &[String]) -> Result<String, BrCallError>` — spawns `tokio::process::Command::new("br")` with `--format json`, `RUST_LOG=error`
5. `enum BrCallError { Retryable(String), Fatal(anyhow::Error) }` (private)
6. Private deser structs: `BrVersion`, `BrErrorEnvelope`, `BrErrorInner`, `BrListPage`, `BrIssueWithCounts`, `BrIssueDetails`, `BrDependency`
7. `From<BrIssueDetails> for Issue` and `From<BrIssueWithCounts> for IssueSummary` conversions
8. `impl IssueTracker for BeadsAdapter` — all 4 methods
9. `const BLOCKING_TYPES: &[&str] = &["blocks", "parent-child", "conditional-blocks", "waits-for"];`

See spec sections 6.1 through 6.7 for the exact code for each component. The full file is approximately 300 lines.

- [ ] **Step 2: Verify spur-pm compiles**

Run: `cargo check -p spur-pm`
Expected: Success

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/src/beads.rs
git commit -m "feat(spur-pm): implement BeadsAdapter

Shells to br CLI with --format json. Bounded retry for WAL
contention. Parses BrIssueDetails/BrIssueWithCounts into
spur-pm Issue/IssueSummary types. RUST_LOG=error suppresses
tracing leakage."
```

---

## Task 6: Implement PmService

**Files:**
- Create: `crates/spur-pm/src/service.rs`

- [ ] **Step 1: Write PmService**

Replace the stub `crates/spur-pm/src/service.rs` with:

```rust
use std::path::Path;

use crate::beads::BeadsAdapter;
use crate::github::GitHubAdapter;
use crate::types::*;

enum PmBackendInner {
    Beads {
        beads: BeadsAdapter,
        github: Option<GitHubAdapter>,
    },
    GitHub {
        adapter: GitHubAdapter,
    },
}

pub struct PmService {
    inner: PmBackendInner,
}

impl PmService {
    /// Returns None if no PM backend available. Errors only for misconfiguration
    /// (e.g., .beads/ exists but br binary is missing).
    pub async fn try_new(
        github_repo: Option<String>,
        repo_root: &Path,
    ) -> anyhow::Result<Option<Self>> {
        let beads_dir = repo_root.join(".beads");

        if beads_dir.is_dir() {
            let beads = BeadsAdapter::connect(repo_root).await?;
            let github = Self::try_github(github_repo, repo_root).await;
            return Ok(Some(Self {
                inner: PmBackendInner::Beads { beads, github },
            }));
        }

        if let Some(gh) = Self::try_github(github_repo, repo_root).await {
            return Ok(Some(Self {
                inner: PmBackendInner::GitHub { adapter: gh },
            }));
        }

        Ok(None)
    }

    async fn try_github(repo: Option<String>, repo_root: &Path) -> Option<GitHubAdapter> {
        match GitHubAdapter::connect(repo, repo_root).await {
            Ok(gh) => Some(gh),
            Err(e) => {
                tracing::debug!("GitHub PM unavailable: {e}");
                None
            }
        }
    }

    pub async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.get_issue(id).await,
            PmBackendInner::GitHub { adapter } => adapter.get_issue(id).await,
        }
    }

    pub async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.list_issues(filter).await,
            PmBackendInner::GitHub { adapter } => adapter.list_issues(filter).await,
        }
    }

    pub async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.update_issue(id, update).await,
            PmBackendInner::GitHub { adapter } => adapter.update_issue(id, update).await,
        }
    }

    pub async fn create_pr(&self, params: PrParams) -> anyhow::Result<String> {
        match &self.inner {
            PmBackendInner::Beads {
                github: Some(gh), ..
            } => gh.create_pr(params).await,
            PmBackendInner::Beads { github: None, .. } => {
                anyhow::bail!("No PR service. Configure [pm.github] for PR creation.")
            }
            PmBackendInner::GitHub { adapter } => adapter.create_pr(params).await,
        }
    }

    pub async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.poll().await,
            PmBackendInner::GitHub { adapter } => adapter.poll().await,
        }
    }

    pub fn source_str(&self) -> &'static str {
        match &self.inner {
            PmBackendInner::Beads { .. } => "beads",
            PmBackendInner::GitHub { .. } => "github",
        }
    }
}
```

- [ ] **Step 2: Verify spur-pm compiles**

Run: `cargo check -p spur-pm`
Expected: Success

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/src/service.rs
git commit -m "feat(spur-pm): implement PmService with enum dispatch

PmBackendInner enum holds concrete adapter types. No Box<dyn>.
try_new() returns Option<Self> — PM is optional.
.beads/ directory → BeadsAdapter + optional GitHub for PRs.
No .beads/ → GitHubAdapter fallback."
```

---

## Task 7: Add SpurEventBody Changes (spur-acp)

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/src/config/mod.rs`

- [ ] **Step 1: Add IssueSummaryEvent type and IssuesLoaded variant to events.rs**

In `crates/spur-acp/src/domain/events.rs`, add the `IssueSummaryEvent` struct (before the `SpurEventBody` enum):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummaryEvent {
    pub id: String,
    pub source: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}
```

Add `IssuesLoaded` variant to `SpurEventBody` (after the existing PM variants around line 250):

```rust
    IssuesLoaded {
        issues: Vec<IssueSummaryEvent>,
    },
```

Add `assignee` field to existing `IssueUpdated` variant (line 246-250):

```rust
    IssueUpdated {
        source: String,
        id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
    },
```

- [ ] **Step 2: Add BeadsPmConfig to config/mod.rs**

In `crates/spur-acp/src/config/mod.rs`, add `BeadsPmConfig` struct and update `PmConfig`:

After `GitHubPmConfig` (around line 466), add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadsPmConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_sync: bool,
}
```

Update `PmConfig` (lines 449-453) to include beads:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PmConfig {
    #[serde(default)]
    pub github: Option<GitHubPmConfig>,
    #[serde(default)]
    pub beads: Option<BeadsPmConfig>,
}
```

- [ ] **Step 3: Fix all match arms that reference IssueUpdated or new variants**

Search the entire workspace for `IssueUpdated` and `IssueReceived` match patterns. Update any that destructure without `..` to include the new `assignee` field or use `..`:

```bash
cargo check --workspace 2>&1 | grep "error" | head -20
```

Fix each compilation error by adding `..` to match arms or handling new variants.

- [ ] **Step 4: Verify workspace compiles**

Run: `cargo check --workspace`
Expected: Success (or only errors in spur-core/spur-mcp for the old PmAdapter references — those are fixed in later tasks)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-acp/src/config/mod.rs
git commit -m "feat(spur-acp): add IssuesLoaded event, BeadsPmConfig, enriched IssueUpdated

- New IssueSummaryEvent type for bulk issue data
- New IssuesLoaded SpurEventBody variant
- Add assignee to IssueUpdated (serde default for compat)
- Add BeadsPmConfig to PmConfig"
```

---

## Task 8: Update MCP Tool Definitions (spur-mcp)

**Files:**
- Modify: `crates/spur-mcp/Cargo.toml`
- Modify: `crates/spur-mcp/src/tools.rs`

- [ ] **Step 1: Add spur-pm dependency to spur-mcp**

In `crates/spur-mcp/Cargo.toml`, add under `[dependencies]`:

```toml
spur-pm = { workspace = true }
```

- [ ] **Step 2: Update get_issue_def — source becomes optional**

In `crates/spur-mcp/src/tools.rs`, update `get_issue_def()` (line 151): change `"required": ["source", "id"]` to `"required": ["id"]`. Add description noting source defaults to configured backend.

- [ ] **Step 3: Add list_issues_def**

Add new function in `crates/spur-mcp/src/tools.rs`:

```rust
fn list_issues_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_issues".into(),
        description: Some("List issues matching filter criteria from the configured tracker".into()),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "description": "Filter: open, in_progress, blocked, closed" },
                "labels": { "type": "array", "items": { "type": "string" } },
                "assignee": { "type": "string" },
                "priority_min": { "type": "integer", "description": "Min priority (0=critical)" },
                "priority_max": { "type": "integer", "description": "Max priority (4=backlog)" },
                "issue_type": { "type": "string", "description": "task, bug, feature, epic" },
                "text_search": { "type": "string", "description": "Search issue titles" },
                "limit": { "type": "integer", "description": "Max results (default 20, max 100)" }
            }
        }),
    }
}
```

- [ ] **Step 4: Update update_issue_def — add priority, assignee, label fields**

Update `update_issue_def()` (line 173) to add `priority`, `assignee`, `add_labels`, `remove_labels` to the schema properties. Remove `source` from required (use configured backend).

- [ ] **Step 5: Add issue_id to delegate_to_worker, delegate_async, delegate_parallel**

Find each delegation tool's schema definition and add:
```json
"issue_id": { "type": "string", "description": "Optional beads issue ID to auto-track" }
```

- [ ] **Step 6: Add list_issues_def() to tools_list()**

In `tools_list()` (line 361), add `list_issues_def()` to the returned Vec.

- [ ] **Step 7: Verify spur-mcp compiles**

Run: `cargo check -p spur-mcp`
Expected: Success (tool definitions are just JSON — no type dependencies yet)

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/Cargo.toml crates/spur-mcp/src/tools.rs
git commit -m "feat(spur-mcp): add list_issues tool, enrich PM tool schemas

- list_issues: new MCP tool with full filter support
- get_issue: source now optional (defaults to configured backend)
- update_issue: add priority, assignee, add_labels, remove_labels
- delegate_to_worker/async/parallel: add issue_id parameter
- spur-mcp now depends on spur-pm"
```

---

## Task 9: Replace MCP PM Handlers with Direct PmService Calls

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Add pm_service field to McpCallbackServer**

Add `pm_service: Option<Arc<PmService>>` to the server struct. Update the constructor to accept it. Import `spur_pm::{PmService, IssueFilter, IssueUpdate, PrParams}` and `std::sync::Arc`.

- [ ] **Step 2: Replace handle_get_issue (lines 767-807)**

Replace the `DelegationRequest`-based implementation with:

```rust
async fn handle_get_issue(&self, _id: Uuid, arguments: Value) -> anyhow::Result<CallToolResult> {
    let pm = self.pm_service.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No issue tracker configured"))?;
    let issue_id = arguments.get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: id"))?;
    let issue = pm.get_issue(issue_id).await?;
    Ok(CallToolResult {
        content: vec![ToolContent::Text {
            text: serde_json::to_string_pretty(&issue)?,
        }],
        is_error: None,
    })
}
```

- [ ] **Step 3: Add handle_list_issues**

```rust
async fn handle_list_issues(&self, _id: Uuid, arguments: Value) -> anyhow::Result<CallToolResult> {
    let pm = self.pm_service.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No issue tracker configured"))?;
    let filter = IssueFilter {
        status: arguments.get("status").and_then(|v| v.as_str()).map(String::from),
        assignee: arguments.get("assignee").and_then(|v| v.as_str()).map(String::from),
        priority_min: arguments.get("priority_min").and_then(|v| v.as_i64()).map(|n| n as i32),
        priority_max: arguments.get("priority_max").and_then(|v| v.as_i64()).map(|n| n as i32),
        issue_type: arguments.get("issue_type").and_then(|v| v.as_str()).map(String::from),
        text_search: arguments.get("text_search").and_then(|v| v.as_str()).map(String::from),
        limit: Some(arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(100) as usize),
        ..Default::default()
    };
    let issues = pm.list_issues(filter).await?;
    Ok(CallToolResult {
        content: vec![ToolContent::Text {
            text: serde_json::to_string_pretty(&issues)?,
        }],
        is_error: None,
    })
}
```

- [ ] **Step 4: Replace handle_update_issue (lines 809-857)**

Replace with direct PmService call using the enriched IssueUpdate type.

- [ ] **Step 5: Replace handle_create_pr (lines 859-901)**

Replace with direct `pm.create_pr(params)` call.

- [ ] **Step 6: Add list_issues to dispatch match**

In `handle_tool_call` (line 371), add:
```rust
"list_issues" => self.handle_list_issues(id, arguments).await,
```

- [ ] **Step 7: Pass issue_id through delegation tools**

In `handle_delegate_to_worker`, `handle_delegate_async`, `handle_delegate_parallel`: extract `issue_id` from arguments and include it in the `DelegationRequest`.

- [ ] **Step 8: Verify spur-mcp compiles**

Run: `cargo check -p spur-mcp`

- [ ] **Step 9: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): replace __pm_* sentinel routing with direct PmService calls

- handle_get_issue calls pm_service.get_issue() directly
- handle_list_issues: new handler for list_issues tool
- handle_update_issue: enriched with priority/assignee/labels
- handle_create_pr: routes through pm_service.create_pr()
- Delegation tools pass issue_id through to DelegationRequest"
```

---

## Task 10: Update Orchestrator — PmService Integration + Workflow Coupling

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

This is the largest task. It involves:

- [ ] **Step 1: Add pm_service to Orchestrator struct**

Add `pm_service: Option<Arc<PmService>>` field. Update the constructor to accept it. Add `use spur_pm::PmService;` and `use std::sync::Arc;`.

- [ ] **Step 2: Add issue_id to DelegationRequest**

Find the `DelegationRequest` struct (likely in spur-acp or spur-core) and add `pub issue_id: Option<String>`. Update all construction sites to include `issue_id: None` (or the passed value from MCP).

- [ ] **Step 3: Emit IssuesLoaded at session start**

In the `run()` method (around where `fetch_issue_context` is called at line ~316), add:

```rust
if let Some(pm) = &self.pm_service {
    match pm.list_issues(spur_pm::IssueFilter {
        status: Some("open".into()),
        ..Default::default()
    }).await {
        Ok(issues) => {
            let event_issues: Vec<_> = issues.iter().map(|i| {
                spur_acp::domain::events::IssueSummaryEvent {
                    id: i.id.clone(),
                    source: pm.source_str().into(),
                    title: i.title.clone(),
                    status: i.status.clone(),
                    priority: i.priority,
                    issue_type: i.issue_type.clone(),
                    assignee: i.assignee.clone(),
                }
            }).collect();
            self.emit(SpurEventBody::IssuesLoaded { issues: event_issues });
            tracing::info!(count = issues.len(), "Loaded open issues from {}", pm.source_str());
        }
        Err(e) => tracing::warn!("Failed to load issues: {e}"),
    }
}
```

- [ ] **Step 4: Add workflow coupling in execute_delegation**

In `execute_delegation` (line ~2176), BEFORE spawning the worker, add issue claiming:

```rust
if let (Some(issue_id), Some(pm)) = (&request.issue_id, &self.pm_service) {
    let worker_name = format!("spur-worker-{}", request.id);
    if let Err(e) = pm.update_issue(issue_id, spur_pm::IssueUpdate {
        status: Some("in_progress".into()),
        assignee: Some(worker_name.clone()),
        ..Default::default()
    }).await {
        tracing::warn!(issue = %issue_id, "Failed to claim issue: {e}");
    } else {
        self.emit(SpurEventBody::IssueUpdated {
            source: pm.source_str().into(),
            id: issue_id.clone(),
            status: "in_progress".into(),
            assignee: Some(worker_name),
        });
    }
}
```

- [ ] **Step 5: Add issue transition in finalize_delegation**

In the finalization path (after review gate resolution), add issue transition logic:

```rust
if let (Some(issue_id), Some(pm)) = (&request.issue_id, &self.pm_service) {
    let (new_status, comment) = match &result.status {
        DelegationStatus::Success => (
            None, // No status change — brain decides when to close
            format!("Completed by SPUR delegation {}", request.id),
        ),
        DelegationStatus::Rejected => (
            Some("open"),
            format!("Delegation {} rejected", request.id),
        ),
        DelegationStatus::Failed { error } => (
            Some("open"),
            format!("Delegation {} failed: {}", request.id, error),
        ),
        _ => (Some("open"), format!("Delegation {} ended", request.id)),
    };

    let update = spur_pm::IssueUpdate {
        status: new_status.map(String::from),
        comment: Some(comment),
        ..Default::default()
    };

    if let Err(e) = pm.update_issue(issue_id, update).await {
        tracing::warn!(issue = %issue_id, "Failed to transition issue: {e}");
    } else if let Some(status) = new_status {
        self.emit(SpurEventBody::IssueUpdated {
            source: pm.source_str().into(),
            id: issue_id.clone(),
            status: status.into(),
            assignee: None,
        });
    }
}
```

- [ ] **Step 6: Remove handle_pm_operation free function (lines 3416-3506)**

Delete the entire `handle_pm_operation` function.

- [ ] **Step 7: Remove __pm_* routing (lines 2188-2192)**

Remove the `if agent.starts_with("__pm_")` block from `execute_delegation`.

- [ ] **Step 8: Update fetch_issue_context to use PmService**

Replace the `fetch_issue_context` method (line 2023) to use `self.pm_service` instead of creating a fresh `GitHubAdapter`.

- [ ] **Step 9: Verify workspace compiles**

Run: `cargo check --workspace`
Expected: Success

- [ ] **Step 10: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): integrate PmService, add workflow coupling

- Add pm_service: Option<Arc<PmService>> to Orchestrator
- Emit IssuesLoaded at session start
- Auto-claim issue on delegation start (in_progress + assignee)
- Comment on success, revert to open on failure/rejection
- Remove handle_pm_operation() and __pm_* sentinel routing
- PM failures never block delegation lifecycle"
```

---

## Task 11: Implement TUI IssuesPanel

**Files:**
- Create: `crates/spur-tui/src/components/issues_panel.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Create issues_panel.rs**

Create `crates/spur-tui/src/components/issues_panel.rs`:

```rust
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style, Stylize},
    widgets::{Block, Cell, Row, Table},
    Frame,
};

use spur_pm::IssueSummary;

pub struct IssuesPanel;

impl IssuesPanel {
    pub fn render(issues: &[IssueSummary], frame: &mut Frame, area: Rect) {
        if issues.is_empty() {
            return;
        }

        let header = Row::new(["ID", "P", "Type", "Status", "Assignee", "Title"])
            .style(Style::default().bold());

        let rows: Vec<Row> = issues
            .iter()
            .map(|issue| {
                let priority_cell = match issue.priority {
                    Some(0) => Cell::from("P0").fg(Color::Red),
                    Some(1) => Cell::from("P1").fg(Color::Yellow),
                    Some(2) => Cell::from("P2").fg(Color::White),
                    Some(3) => Cell::from("P3").fg(Color::DarkGray),
                    Some(4) => Cell::from("P4").fg(Color::DarkGray),
                    _ => Cell::from("--").fg(Color::DarkGray),
                };

                let status_cell = match issue.status.as_str() {
                    "open" => Cell::from("open").fg(Color::Green),
                    "in_progress" => Cell::from("wip").fg(Color::Cyan),
                    "blocked" => Cell::from("blk").fg(Color::Red),
                    "closed" => Cell::from("done").fg(Color::DarkGray),
                    other => Cell::from(other).fg(Color::White),
                };

                Row::new([
                    Cell::from(issue.id.as_str()),
                    priority_cell,
                    Cell::from(issue.issue_type.as_deref().unwrap_or("--")),
                    status_cell,
                    Cell::from(issue.assignee.as_deref().unwrap_or("--")),
                    Cell::from(issue.title.as_str()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(8),
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Min(20),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(" Issues "));

        frame.render_widget(table, area);
    }

    pub fn computed_height(issue_count: usize, available_height: u16) -> u16 {
        let max_panel = (available_height / 4).max(3);
        (issue_count as u16 + 3).min(max_panel)
    }
}
```

- [ ] **Step 2: Add pub mod issues_panel to components/mod.rs**

In `crates/spur-tui/src/components/mod.rs`, add:
```rust
pub mod issues_panel;
```

- [ ] **Step 3: Verify spur-tui compiles**

Run: `cargo check -p spur-tui`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/issues_panel.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): add IssuesPanel component

Stateless table renderer with colored priority (P0-P4) and
status (open/wip/blk/done). Responsive height (max 25% of
terminal). Modeled after WorkersPanel pattern."
```

---

## Task 12: Integrate IssuesPanel into DashboardView

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/components/status_bar.rs`

- [ ] **Step 1: Add tracked_issues field to DashboardView**

Add `tracked_issues: Vec<spur_pm::IssueSummary>` field to `DashboardView` struct. Initialize as `Vec::new()` in the constructor.

- [ ] **Step 2: Handle IssuesLoaded and enriched IssueUpdated events**

In `handle_spur_event` (around line 906), add handlers:

```rust
SpurEventBody::IssuesLoaded { issues } => {
    self.tracked_issues = issues.iter().map(|i| spur_pm::IssueSummary {
        id: i.id.clone(),
        source: spur_pm::PmSource::Beads, // or parse from i.source
        title: i.title.clone(),
        status: i.status.clone(),
        labels: Vec::new(),
        url: String::new(),
        priority: i.priority,
        issue_type: i.issue_type.clone(),
        assignee: i.assignee.clone(),
    }).collect();
    // Sort: by priority (ascending), then by status (open first)
    self.tracked_issues.sort_by(|a, b| {
        a.priority.unwrap_or(99).cmp(&b.priority.unwrap_or(99))
    });
    self.activity_log.push(LogEntry {
        timestamp: Self::now_stamp(),
        prefix: "[pm]".into(),
        message: format!("{} issues loaded", self.tracked_issues.len()),
        kind: LogEntryKind::Info,
    });
}
```

Update existing `IssueUpdated` handler to also update `tracked_issues`:

```rust
SpurEventBody::IssueUpdated { source, id, status, assignee } => {
    if let Some(issue) = self.tracked_issues.iter_mut().find(|i| i.id == *id) {
        issue.status = status.clone();
        if let Some(a) = assignee {
            issue.assignee = Some(a.clone());
        }
    }
    self.activity_log.push(LogEntry {
        timestamp: Self::now_stamp(),
        prefix: "[pm]".into(),
        message: format!("Issue {} ({}) updated: {}", id, source, status),
        kind: LogEntryKind::Info,
    });
}
```

- [ ] **Step 3: Update render layout to include IssuesPanel**

In `render_with_lineage` (line ~334), modify the layout to include an IssuesPanel row when issues exist:

```rust
// After agents_height computation, before building chunks:
let issues_height = if self.tracked_issues.is_empty() {
    0
} else {
    IssuesPanel::computed_height(self.tracked_issues.len(), area.height)
};

// Build layout with conditional issues row
let mut constraints = vec![Constraint::Length(agents_height)];
if issues_height > 0 {
    constraints.push(Constraint::Length(issues_height));
}
constraints.push(Constraint::Min(4));       // activity log / detail pane
constraints.push(Constraint::Length(input_height));
constraints.push(Constraint::Length(1));     // status bar
```

Render the IssuesPanel in the appropriate chunk:
```rust
if issues_height > 0 {
    IssuesPanel::render(&self.tracked_issues, frame, chunks[chunk_idx]);
    chunk_idx += 1;
}
```

- [ ] **Step 4: Add issues count to StatusBar**

In `crates/spur-tui/src/components/status_bar.rs`, add `issue_count: usize` to `StatusBarProps`. In the render method, add a span before the running count:

```rust
if props.issue_count > 0 {
    spans.push(Span::styled(
        format!("{} issues", props.issue_count),
        Style::default().fg(Color::Cyan),
    ));
    spans.push(Span::raw(" · "));
}
```

Update all StatusBar render call sites to pass `issue_count: self.tracked_issues.len()`.

- [ ] **Step 5: Verify spur-tui compiles**

Run: `cargo check -p spur-tui`

- [ ] **Step 6: Verify entire workspace compiles**

Run: `cargo check --workspace`
Expected: Success

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/components/status_bar.rs
git commit -m "feat(spur-tui): integrate IssuesPanel into Dashboard

- tracked_issues populated from IssuesLoaded events
- IssueUpdated events update cached issue state
- IssuesPanel auto-visible when issues exist
- StatusBar shows issue count with cyan color
- Issues sorted by priority (critical first)"
```

---

## Task 13: Wire PmService Through Bootstrap

**Files:**
- Modify: Bootstrap code in `crates/spur-cli` or wherever the Orchestrator and McpServer are constructed

- [ ] **Step 1: Find the bootstrap code**

Search for where `Orchestrator::new` and `McpCallbackServer::new` are called:

```bash
rg "Orchestrator::new" crates/spur-cli/
rg "McpCallbackServer::new" crates/
```

- [ ] **Step 2: Create PmService and pass Arc to both**

At the bootstrap site, add:

```rust
let pm_service = spur_pm::PmService::try_new(
    config.pm.github.as_ref().and_then(|g| g.repo.clone()),
    &repo_root,
).await?;
let pm_arc = pm_service.map(Arc::new);
```

Pass `pm_arc.clone()` to both Orchestrator and McpCallbackServer constructors.

- [ ] **Step 3: Verify full build**

Run: `cargo build --workspace`
Expected: Success

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/
git commit -m "feat(spur-cli): wire PmService through bootstrap

Create PmService at startup, pass Arc to both Orchestrator
and McpCallbackServer. PM is optional — None if no backend
configured."
```

---

## Task 14: Final Integration Verification

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: Clean build

- [ ] **Step 2: Run existing tests**

Run: `cargo test --workspace`
Expected: All existing tests pass (no regressions)

- [ ] **Step 3: Verify with clippy**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | head -30`
Fix any warnings.

- [ ] **Step 4: Final commit if any fixes**

```bash
git add -A
git commit -m "fix: address clippy warnings and test fixes from beads integration"
```

---

## Dependency Order

```
Task 1 (types) → Task 2 (traits) → Task 3 (github adapter) → Task 4 (lib.rs)
    ↓
Task 5 (BeadsAdapter) → Task 6 (PmService)
    ↓
Task 7 (spur-acp events/config)
    ↓
Task 8 (MCP tools) → Task 9 (MCP handlers)
    ↓
Task 10 (Orchestrator)
    ↓
Task 11 (TUI IssuesPanel) → Task 12 (Dashboard integration)
    ↓
Task 13 (Bootstrap wiring) → Task 14 (Final verification)
```

Tasks 1-6 are spur-pm internal. Tasks 7-10 are cross-crate integration. Tasks 11-12 are TUI. Task 13 wires everything together.
