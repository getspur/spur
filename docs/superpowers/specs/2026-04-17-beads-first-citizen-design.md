# Local-First beads_rust as First Citizen in spur-pm

> Spec produced 2026-04-17. L9-validated through MCTS + first-principles + second-order thinking across 46 rounds of sequential evaluation.

## 1. Problem Statement

spur-pm is hardcoded to GitHub Issues via `gh` CLI shelling. The `handle_pm_operation()` free function creates a fresh `GitHubAdapter` per call, ignores config, never emits PM events (`PrCreated`/`IssueUpdated` defined but never fired), and routes through `__pm_*` pseudo-agent sentinel strings. `list_issues` exists on the trait but has no MCP tool. The type model is GitHub-centric with dead fields (`linked_prs` always empty, `priority` always None, `auto_label` never read).

beads_rust (`br`) is a local-first, agent-oriented issue tracker with SQLite storage, rich CLI (`--format json`), 30+ issue fields, dependency tracking, full-text search, and ~5ms invocation latency. It should be the default PM backend for SPUR.

## 2. Strategy Selection (MCTS Result)

**Strategy Phi** — scored 41/50, Pareto-optimal on the observability-coupling frontier.

| Dimension | Decision | Score |
|---|---|---|
| Adapter mechanism | CLI shelling to `br --format json` | P1: 9/10 |
| TUI integration | Read-only IssuesPanel, auto-visible | P4: 7/10 |
| Orchestrator coupling | Bidirectional workflow (claim/comment/revert) | P5: 9/10 |
| GitHub relationship | Demoted to PR-only service | P2: 9/10 |
| Coupling budget | PmService singleton, enum dispatch | P3: 7/10 |

Runner-up Strategy Psi (full event-sourced issue lifecycle) scored 42/50 but costs 3x to build — rejected per YAGNI.

## 3. Trait Architecture

### 3.1 IssueTracker (4 methods)

```rust
#[async_trait]
pub trait IssueTracker: Send + Sync {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue>;
    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>>;
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()>;
    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>>;
}
```

### 3.2 PrService (1 method)

```rust
#[async_trait]
pub trait PrService: Send + Sync {
    async fn create_pr(&self, params: PrParams) -> anyhow::Result<String>;
}
```

### 3.3 Design Decisions

| Decision | Rationale |
|---|---|
| No `connect(&mut self)` on traits | Async constructors on concrete types. Eliminates `&mut`/`&self` phase split. Enables `Arc<PmService>` without RwLock. |
| No `source()` on traits | Caller (PmService) already knows. Associated constant disguised as method. |
| No `search()` on traits | Subsumed by `IssueFilter.text_search`. More composable — search + filter in one call. |
| Keep `async_trait` | dyn-compatible, matches codebase idiom. Ready for native async trait migration when `async_fn_in_dyn_trait` stabilizes. |
| Keep `poll()` | Generality for remote backends. PmService does NOT run a poll loop — issue state flows through workflow coupling events. |
| Traits as contracts, not dispatch | Traits enforce method signatures on adapter authors. PmService uses internal enum dispatch (no `Box<dyn>`). |

### 3.4 Implementors

- `BeadsAdapter`: `impl IssueTracker` only
- `GitHubAdapter`: `impl IssueTracker + PrService`

## 4. Type Model

### 4.1 Issue (12 fields, 0 dead weight)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,                          // "bd-abc" (beads) or "42" (github)
    pub source: PmSource,
    pub title: String,
    pub body: String,
    pub status: String,                      // beads: open/in_progress/blocked/closed/deferred/draft
                                             // github: open/closed
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub url: String,                         // beads: "beads://{id}" github: https URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,               // beads: 0-4 (critical→backlog). GitHub: None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,          // beads: task/bug/feature/epic. GitHub: None
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,             // beads: blocking issue IDs. GitHub: empty
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,           // required — both backends provide
    pub updated_at: DateTime<Utc>,           // required — both backends provide
}
```

### 4.2 IssueSummary

```rust
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
```

### 4.3 IssueFilter

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub priority_min: Option<i32>,
    pub priority_max: Option<i32>,
    pub issue_type: Option<String>,
    pub text_search: Option<String>,         // maps to br list --title-contains
    /// None = backend default (typically 50)
    pub limit: Option<usize>,
}
```

Unsupported filter fields are silently ignored by backends (additive filtering semantics — superset returned, caller can post-filter).

### 4.4 IssueUpdate

```rust
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
```

No `claim: bool` — use explicit `status: Some("in_progress")` + `assignee: Some(name)`. Eliminates semantic overlap and magic booleans.

### 4.5 PmSource

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmSource {
    Beads,      // first citizen
    GitHub,
    Linear,     // phase 3
    Plane,      // phase 3
}
```

PmSource stays in spur-pm. SpurEventBody uses `source: String` to avoid crate-layer inversion.

### 4.6 PrParams, PmEvent (unchanged)

```rust
pub struct PrParams {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: Option<String>,
    pub repo: Option<String>,
}

pub enum PmEvent {
    IssueCreated(IssueSummary),
    IssueUpdated(IssueSummary),
}
```

### 4.7 Type Design Principles

- **Don't encode external domain models** — `status: String`, not an enum mirroring beads' `Status`
- **Design for richest source, degrade for poorest** — beads-native fields with `Option` for GitHub
- **No dead fields** — removed `linked_prs` (never populated), renamed `dependencies` → `blocked_by` (unambiguous direction)
- **Required timestamps** — `created_at`/`updated_at` are not Option (both backends provide them)
- **Flat struct with Options** — beat `serde_json::Value` (no safety), `Issue<S: Source>` (generics infection), two-tier model (unnecessary)

## 5. PmService

### 5.1 Internal Enum Dispatch

```rust
enum PmBackendInner {
    Beads {
        beads: BeadsAdapter,
        github: Option<GitHubAdapter>,  // PR-only
    },
    GitHub {
        adapter: GitHubAdapter,         // single instance, issues + PRs
    },
}

pub struct PmService {
    inner: PmBackendInner,
}
```

No `Box<dyn>`. No heap-allocated trait objects. Exhaustive match forces handling all backends. Compiler can inline adapter calls.

### 5.2 Construction

```rust
impl PmService {
    /// Returns None if no PM backend available. Errors only for misconfiguration
    /// (e.g., .beads/ exists but br binary is missing).
    pub async fn try_new(config: &PmConfig, repo_root: &Path) -> anyhow::Result<Option<Self>> {
        let beads_dir = repo_root.join(".beads");

        if beads_dir.is_dir() {
            // .beads/ exists — beads SHOULD work. Failure here is an error.
            let beads = BeadsAdapter::connect(repo_root).await?;
            let github = Self::try_github(config, repo_root).await; // best-effort PR service
            return Ok(Some(Self { inner: PmBackendInner::Beads { beads, github } }));
        }

        if let Some(gh) = Self::try_github(config, repo_root).await {
            return Ok(Some(Self { inner: PmBackendInner::GitHub { adapter: gh } }));
        }

        Ok(None) // No PM backend — session continues without PM
    }

    async fn try_github(config: &PmConfig, repo_root: &Path) -> Option<GitHubAdapter> {
        let repo = config.github.as_ref().and_then(|g| g.repo.clone());
        match GitHubAdapter::connect(repo, repo_root).await {
            Ok(gh) => Some(gh),
            Err(e) => { tracing::debug!("GitHub unavailable: {e}"); None }
        }
    }
}
```

### 5.3 Delegating Methods

```rust
impl PmService {
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
            PmBackendInner::Beads { github: Some(gh), .. } => gh.create_pr(params).await,
            PmBackendInner::Beads { github: None, .. } => {
                anyhow::bail!("No PR service. Configure [pm.github] for PR creation.")
            }
            PmBackendInner::GitHub { adapter } => adapter.create_pr(params).await,
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

### 5.4 Design Decisions

| Decision | Rationale |
|---|---|
| `try_new() -> Result<Option<Self>>` | PM is optional. `.beads/` + missing br = error. No PM = None, session continues. |
| Enum dispatch, not Box<dyn> | No double-connect for GitHub-only. No lifetime issues across await. Single GitHubAdapter instance per mode. |
| Pure data access layer | No event emission, no workflow logic. Callers handle SpurEvent emission. spur-pm has no spur-acp dependency. |
| `Arc<PmService>` in Orchestrator | Shared across concurrent delegation tasks. All IssueTracker methods are `&self`. Interior mutability only for `poll()` tracking. |
| Last-write-wins for concurrent ops | No locking. Brain is authoritative. Orchestrator's auto-claim is convenience, not correctness. |

## 6. BeadsAdapter

### 6.1 Structure

```rust
pub struct BeadsAdapter {
    cwd: PathBuf,
    last_poll: std::sync::Mutex<Option<DateTime<Utc>>>,
}
```

### 6.2 Async Constructor

```rust
impl BeadsAdapter {
    pub async fn connect(repo_root: &Path) -> anyhow::Result<Self> {
        let adapter = Self {
            cwd: repo_root.to_path_buf(),
            last_poll: Mutex::new(None),
        };

        // Verify binary
        let version_json = adapter.run_br(vec!["version".into()]).await
            .context("br binary not found or not working")?;
        let version: BrVersion = serde_json::from_str(&version_json)?;

        // Verify database is readable (validates binary + DB + schema)
        adapter.run_br(vec!["stats".into()]).await
            .context(format!(
                "Failed to read .beads/ at {}. Run `br doctor` to diagnose.",
                repo_root.display()
            ))?;

        tracing::info!(br_version = %version.version, "Connected to beads_rust");
        Ok(adapter)
    }
}
```

### 6.3 Shell Helper with Bounded Retry

```rust
async fn run_br(&self, args: Vec<String>) -> anyhow::Result<String> {
    for attempt in 0..2u8 {
        match self.run_br_once(&args).await {
            Ok(stdout) => return Ok(stdout),
            Err(BrCallError::Retryable(msg)) if attempt == 0 => {
                tracing::debug!(cmd = ?args.first(), "br retryable error, retry in 50ms: {msg}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(BrCallError::Retryable(msg)) => {
                anyhow::bail!("br {:?}: {msg} (after retry)", args.first());
            }
            Err(BrCallError::Fatal(e)) => return Err(e),
        }
    }
    unreachable!()
}

async fn run_br_once(&self, args: &[String]) -> Result<String, BrCallError> {
    let output = tokio::process::Command::new("br")
        .args(args)
        .arg("--format").arg("json")
        .current_dir(&self.cwd)
        .env("RUST_LOG", "error")  // suppress tracing leakage into stdout
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BrCallError::Fatal(anyhow::anyhow!(
                    "br binary not found. Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
                ))
            } else {
                BrCallError::Fatal(anyhow::anyhow!("Failed to spawn br: {e}"))
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Ok(br_err) = serde_json::from_str::<BrErrorEnvelope>(&stderr) {
            if br_err.error.retryable {
                return Err(BrCallError::Retryable(br_err.error.message));
            }
            return Err(BrCallError::Fatal(anyhow::anyhow!(
                "br {:?}: {} ({})", args.first(), br_err.error.message, br_err.error.code
            )));
        }
        return Err(BrCallError::Fatal(anyhow::anyhow!(
            "br {:?}: {}", args.first(), stderr.trim()
        )));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| BrCallError::Fatal(anyhow::anyhow!("br output not UTF-8: {e}")))
}

enum BrCallError {
    Retryable(String),
    Fatal(anyhow::Error),
}
```

### 6.4 Private Deserialization Structs

```rust
#[derive(Deserialize)]
struct BrVersion { version: String }

#[derive(Deserialize)]
struct BrErrorEnvelope { error: BrErrorInner }

#[derive(Deserialize)]
struct BrErrorInner { code: String, message: String, retryable: bool }

#[derive(Deserialize)]
struct BrListPage {
    issues: Vec<BrIssueWithCounts>,
    total: usize,
}

#[derive(Deserialize)]
struct BrIssueWithCounts {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    priority: i32,
    issue_type: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct BrIssueDetails {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    priority: i32,
    issue_type: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    dependencies: Vec<BrDependency>,
}

#[derive(Deserialize)]
struct BrDependency {
    depends_on_id: String,
    #[serde(rename = "type", default = "default_dep_type")]
    dep_type: String,
}

fn default_dep_type() -> String { "blocks".into() }
```

### 6.5 From Conversions

```rust
const BLOCKING_TYPES: &[&str] = &["blocks", "parent-child", "conditional-blocks", "waits-for"];

impl From<BrIssueDetails> for Issue {
    fn from(br: BrIssueDetails) -> Self {
        Self {
            id: br.id.clone(),
            source: PmSource::Beads,
            title: br.title,
            body: br.description.unwrap_or_default(),
            status: br.status,
            labels: br.labels,
            assignee: br.assignee,
            url: format!("beads://{}", br.id),
            priority: Some(br.priority),
            issue_type: Some(br.issue_type),
            blocked_by: br.dependencies.iter()
                .filter(|d| BLOCKING_TYPES.contains(&d.dep_type.as_str()))
                .map(|d| d.depends_on_id.clone())
                .collect(),
            due_at: br.due_at,
            created_at: br.created_at,
            updated_at: br.updated_at,
        }
    }
}

impl From<BrIssueWithCounts> for IssueSummary {
    fn from(br: BrIssueWithCounts) -> Self {
        Self {
            id: br.id.clone(),
            source: PmSource::Beads,
            title: br.title,
            status: br.status,
            labels: br.labels,
            url: format!("beads://{}", br.id),
            priority: Some(br.priority),
            issue_type: Some(br.issue_type),
            assignee: br.assignee,
        }
    }
}
```

### 6.6 IssueTracker Implementation

`get_issue` → `br show {id} --format json` → deserialize `BrIssueDetails` → `Into<Issue>`

`list_issues` → build args from IssueFilter:
- `status` → `-s {status}`
- `labels` → `-l {label}` per label
- `priority_min/max` → `--priority-min {n}` / `--priority-max {n}`
- `issue_type` → `-t {type}`
- `assignee` → `--assignee {name}`
- `text_search` → `--title-contains {text}`
- `limit` → `--limit {n}` (default 50)

Deserialize `BrListPage`, convert `issues` via `Into<IssueSummary>`.

`update_issue` → sequential br calls:
1. `br update {id} -s {status} -p {priority} --assignee {assignee}` (if any field set)
2. `br comments add {id} {comment}` (if comment set)
3. `br label add {id} {labels}` (if add_labels non-empty)
4. `br label remove {id} {labels}` (if remove_labels non-empty)

`poll` → `br list -s open --limit 20 --format json` → diff against `last_poll` timestamp → emit `PmEvent::IssueCreated` or `PmEvent::IssueUpdated`.

### 6.7 Key Implementation Notes

- `run_br` uses `Vec<String>` for fully-owned arg vectors (no `&str` lifetime issues)
- `RUST_LOG=error` env var on all `br` invocations suppresses tracing leakage
- Bounded retry (2 attempts, 50ms delay) for WAL contention (br errors with `retryable: true`)
- `br stats` during connect validates binary + DB + schema in one call

## 7. MCP Surface Changes

### 7.1 Tool Updates

| Tool | Change |
|---|---|
| `get_issue` | `source` becomes optional (defaults to configured backend). Error if explicit source doesn't match configured backend. |
| `list_issues` | **NEW** — full IssueFilter exposed. Default limit 20, cap 100 for brain context pressure. |
| `update_issue` | Add `priority`, `assignee`, `add_labels`, `remove_labels` fields. |
| `create_pr` | Unchanged schema. Routes to PrService (GitHubAdapter). |
| `delegate_to_worker` | Add `issue_id: Option<String>` for workflow coupling. |
| `delegate_async` | Add `issue_id: Option<String>`. |
| `delegate_parallel` | Add `issue_id: Option<String>`. |

### 7.2 Routing Change

**Before**: MCP server sends `DelegationRequest { agent: "__pm_get_issue", ... }` through mpsc channel → Orchestrator → `handle_pm_operation()` free function → fresh GitHubAdapter.

**After**: MCP server calls `self.pm_service.get_issue(id).await` directly. No sentinel routing. No DelegationRequest for PM ops. No fresh adapter per call.

### 7.3 MCP Server Construction

```rust
pub struct McpCallbackServer {
    // ... existing fields ...
    pm_service: Option<Arc<PmService>>,  // NEW
}
```

Constructed in bootstrap with the same `Arc<PmService>` as Orchestrator.

### 7.4 Crate Dependency

spur-mcp gains `spur-pm` dependency (downward dep from Orchestration to Support — clean layering).

## 8. Orchestrator Workflow Coupling

### 8.1 DelegationRequest Change

```rust
pub struct DelegationRequest {
    // ... existing fields ...
    pub issue_id: Option<String>,  // NEW: links delegation to tracked issue
}
```

### 8.2 Session Start — Issue Loading

```rust
// In Orchestrator::run(), at session start:
if let Some(pm) = &self.pm_service {
    match pm.list_issues(IssueFilter { status: Some("open".into()), ..Default::default() }).await {
        Ok(issues) => {
            let event_issues: Vec<IssueSummaryEvent> = issues.iter().map(|i| IssueSummaryEvent {
                id: i.id.clone(),
                source: pm.source_str().into(),
                title: i.title.clone(),
                status: i.status.clone(),
                priority: i.priority,
                issue_type: i.issue_type.clone(),
                assignee: i.assignee.clone(),
            }).collect();
            self.emit(SpurEventBody::IssuesLoaded { issues: event_issues });
        }
        Err(e) => tracing::warn!("Failed to load issues: {e}"),
    }
}
```

### 8.3 Delegation Lifecycle Hooks

| Event | Issue Transition | Notes |
|---|---|---|
| Delegation starts | `status: "in_progress"`, `assignee: worker_name` | Auto. Warn-on-fail, never blocks delegation. |
| Delegation succeeds + approved | Add comment: "Completed by delegation {id}" | Auto. No status change — brain decides when to close. |
| Delegation rejected | `status: "open"`, comment: "Rejected" | Auto. |
| Delegation failed | `status: "open"`, comment: "Failed: {error}" | Auto. |
| Delegation cancelled | `status: "open"`, comment: "Cancelled" | Auto. |
| All work complete | `status: "closed"` | **Brain decides** — explicit `update_issue` MCP call. |

After each PM operation, the Orchestrator emits `SpurEventBody::IssueUpdated { source, id, status, assignee }`.

**PM failures never block the delegation lifecycle.** Errors are logged at `warn` level.

### 8.4 Removed

- `handle_pm_operation()` free function — replaced by PmService
- `__pm_*` sentinel agent routing — replaced by direct PmService calls
- `fetch_issue_context()` — replaced by `pm_service.get_issue()`

## 9. SpurEventBody Changes

### 9.1 New Variant

```rust
/// Emitted once at session start with all tracked issues.
IssuesLoaded {
    issues: Vec<IssueSummaryEvent>,
}
```

### 9.2 New Type in spur-acp

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

### 9.3 Enriched Variant

```rust
IssueUpdated {
    source: String,
    id: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee: Option<String>,  // NEW — carries assignee changes
}
```

### 9.4 Unchanged

- `IssueReceived { source: String, id: String }` — stays slim
- `PrCreated { url: String }` — unchanged

All new/changed fields use `#[serde(default)]` for backward-compatible NDJSON deserialization.

## 10. TUI IssuesPanel

### 10.1 New Component

File: `crates/spur-tui/src/components/issues_panel.rs`

Stateless table renderer modeled after `WorkersPanel`:
- Columns: ID | P | Type | Status | Assignee | Title
- Priority rendered as colored P0-P4 (P0=red, P1=yellow, P2=white, P3/P4=dark gray)
- Status as colored text (open=green, in_progress/wip=cyan, blocked=red, closed=dark gray)
- Responsive height: `min(issue_count + 3, terminal_height / 4)`

### 10.2 DashboardView Integration

```rust
pub struct DashboardView {
    // ... existing fields ...
    tracked_issues: Vec<IssueSummary>,  // populated from IssuesLoaded event
}
```

Event handling:
- `IssuesLoaded`: replace `tracked_issues` with full list, sort by priority then status
- `IssueUpdated`: find by id, update status and assignee

Layout: auto-visible when `tracked_issues` is non-empty. Inserted between AgentsTree and ActivityLog/DetailPane.

### 10.3 StatusBar

Add `{N} issues` counter with cyan color alongside existing running/pending_review counts.

## 11. Config Model

### 11.1 Updated PmConfig

```rust
pub struct PmConfig {
    pub github: Option<GitHubPmConfig>,
    pub beads: Option<BeadsPmConfig>,   // NEW
}

pub struct BeadsPmConfig {
    pub enabled: bool,        // default true
    pub auto_sync: bool,      // run br sync --flush-only after mutations, default false
}

pub struct GitHubPmConfig {
    pub enabled: bool,        // default true
    pub repo: Option<String>, // "owner/repo", auto-detected if omitted
}
```

### 11.2 Config File

```toml
# .spur/config.toml
[pm.beads]
enabled = true
auto_sync = false

[pm.github]
repo = "owner/repo"
```

### 11.3 Detection Priority

1. If `.beads/` directory exists AND `pm.beads.enabled` is not explicitly `false` → BeadsAdapter
2. Else if `[pm.github]` configured and `pm.github.enabled` is not explicitly `false` → GitHubAdapter
3. Else → no PM (session continues without issue tracking)

`enabled = false` is an explicit opt-out. If `[pm.beads]` is absent from config, `enabled` defaults to `true` — the `.beads/` directory presence is sufficient.

## 12. File Layout

```
crates/spur-pm/src/
├── lib.rs              # Re-exports
├── adapter.rs          # IssueTracker + PrService traits
├── types.rs            # Issue, IssueSummary, IssueFilter, IssueUpdate, PmSource, PrParams, PmEvent
├── service.rs          # NEW: PmService (PmBackendInner enum, config-driven factory)
├── github.rs           # GitHubAdapter (impl IssueTracker + PrService)
└── beads.rs            # NEW: BeadsAdapter (impl IssueTracker, shells to br CLI)

crates/spur-tui/src/components/
└── issues_panel.rs     # NEW: IssuesPanel (stateless table renderer)
```

## 13. Phased Delivery

### Phase 1 (this spec)
- BeadsAdapter + PmService + trait split + enriched types
- MCP direct routing (eliminate `__pm_*` hack) + `list_issues` tool
- Orchestrator workflow coupling (claim/comment/revert)
- TUI IssuesPanel (read-only, auto-visible)
- Config model + detection priority

### Phase 2 (future)
- Interactive TUI (keyboard CRUD on issues)
- Periodic issue refresh in TUI
- MCP `search_issues` tool (routes to `br search` for full-text)
- Stale claim recovery at startup

### Phase 3 (future, if justified)
- Event-sourced issue lifecycle (Strategy Psi)
- Issues as nodes in AgentsTree
- Linear/Plane adapters

## 14. Risk Matrix

| Risk | Severity | Mitigation |
|---|---|---|
| `br` not installed + `.beads/` exists | Medium | `connect()` returns clear error with install URL |
| `br --json` schema changes (0.1.x) | Medium | `#[serde(default)]` on all optional deser fields. `br stats` health check. |
| WAL contention (concurrent br access) | Low | Bounded retry (2 attempts, 50ms). br's own retry handles most cases. |
| Large issue count (>1000) | Low | Default limit 50 (adapter) / 20 (MCP). TUI panel responsive height. |
| Multi-delegation premature close | N/A | Orchestrator does NOT auto-close. Brain decides. |
| Stale in_progress from crashed session | Low | Manual revert for v1. Automated recovery in v2. |
| MCP source mismatch | Low | Clear error: "Source 'X' is not active tracker." |
| Brain context flood from list_issues | Low | MCP hard cap at 100 results. |
