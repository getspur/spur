# TUI-Beads Collaboration Design

> **Strategy Alpha+K+N** — Interactive IssuesPanel + Issue-Directed Brain + Linked Graph.
> Validated through 8-round MCTS with L9 adversarial review.

**Goal:** Transform the read-only IssuesPanel into an interactive collaboration surface where human and brain agent coordinate work through beads issues.

**Architecture:** Interactive TUI panel (mirrors AgentsTree pattern) with `W` key for brain assignment, issue-executor linkage in ExecutorLineage, quick-keys for status changes, and phased extension to create_issue + sub-issue hierarchy.

**Tech Stack:** ratatui (TUI), spur-pm (adapter), spur-acp (events), spur-core (orchestrator), spur-mcp (MCP tools)

---

## 1. Strategy Selection

### MCTS Evaluation (6 candidates → 1 winner)

| Strategy | Score (/130) | Verdict |
|---|---|---|
| **Alpha+K+N: Interactive Panel + Brain Collab + Linked Graph** | **118** | **Winner** |
| Delta: Popup/Overlay Modal | 95 | Subsumed by Alpha |
| Beta: Slash Commands Only | 78 | Low discoverability |
| Gamma: Dedicated IssueView | 75 | Heavy context switch |
| Epsilon: Arc\<PmService\> in TUI | — | Eliminated (breaks unidirectional flow) |
| Zeta: Inline expansion | — | Eliminated (layout complexity) |

### Core Invariant Preserved

```
TUI → UserInput (mpsc) → Orchestrator → SpurEvent (broadcast) → TUI
```

TUI never calls PmService directly. All issue operations flow through the orchestrator.

---

## 2. Collaboration Model

Beads becomes the shared workspace between human (TUI) and brain agent (MCP).

```
Human (TUI)                    Beads Issue Board                Brain Agent (MCP)
--------------------           ------------------               -----------------
Browse issues (j/k)            bd-abc123: P1 bug
Select + [W] ──────────────► status → in_progress ◄──────────── auto-claim
Watch progress                 ◆ linked to worker-1 ────────► delegate_to_worker(issue_id)
Quick-key [d] ─────────────► status → closed                   update_issue on success
/issue create ──────────────► bd-new789: P2 task ◄────────────── create_issue (sub-issues)
```

### What Already Exists

| Capability | Status |
|---|---|
| Brain reads/writes issues via MCP tools (get_issue, update_issue, list_issues) | Done |
| `issue_id` field in delegation MCP schema | Done |
| Orchestrator auto-claims issue on delegation start | Done |
| Orchestrator comments on success, reverts on failure | Done |
| IssuesLoaded event at session start | Done |
| IssuesPanel (read-only, no selection) | Done |

### What's New (This Spec)

| Capability | Phase |
|---|---|
| Interactive IssuesPanel with selection (j/k/Enter) | 1 |
| Issue detail view (async fetch + render) | 1 |
| `W` key = "Work on this issue" (prompt → brain) | 1 |
| `/issues` refresh command | 1 |
| Issue badge in DetailPane header (`◆ id P1 title`) | 2 |
| Issue↔Executor linkage in ExecutorLineage | 2 |
| Quick status keys (o/w/b/d) + `I` hotkey | 2 |
| `create_issue` trait method + MCP tool | 3 |
| Brain context injection at session start | 4 |
| Sub-issue hierarchy + optional Issues tab in DetailPane | 4 |

---

## 3. Data Flow

### 3.1 Issue Detail Fetch

```
User presses Enter on issue in IssuesPanel
  → DashboardView sets issue_focus = IssueFocus::Loading { id }
  → App::process_action sends UserInput::GetIssueDetail { id }
  → Orchestrator calls pm_service.get_issue(id)
  → Orchestrator emits SpurEvent(IssueDetailFetched { requested_id, issue })
  → TUI receives event:
      if requested_id == focused issue id:
        issue_focus = IssueFocus::Loaded { id, issue }
      else:
        drop (stale response)
  → IssueDetailPane renders full issue
```

### 3.2 "Work on This Issue" (W Key)

```
User presses W on selected issue
  → TUI constructs prompt from cached IssueSummary:
      "Work on this issue:

       Issue: {id} — {title}
       Priority: P{n} | Type: {type} | Status: {status}
       {if blocked_by} Blocked by: {blocked_by} {endif}

       Use `get_issue` tool to read full details if needed.
       Use `delegate_to_worker` with issue_id="{id}" for delegations.
       Update issue status as you progress."
  → Sent as UserInput::NewSessionWithMessage (no brain) or UserInput::Message (brain exists)
  → Brain receives prompt, calls get_issue for full body, plans, delegates
```

No new protocol. Prompt construction on existing infrastructure. Brain uses existing MCP tools.

### 3.3 Issue Status Update (Quick-Key)

```
User presses 'w' on focused issue (IssueFocus::Loaded { id, .. })
  → App::process_action sends UserInput::UpdateIssue { id, update: { status: "in_progress" } }
  → Orchestrator calls pm_service.update_issue(id, update)
  → Orchestrator emits SpurEvent(IssueUpdated { source, id, status: "in_progress", assignee })
  → TUI receives event:
      1. Updates tracked_issues cache (status + assignee)
      2. Re-sorts by priority
      3. Updates IssueFocus::Loaded if same issue
  → IssuesPanel + IssueDetailPane re-render
```

### 3.4 Issue↔Executor Linkage

```
Brain calls delegate_to_worker(task="Fix timeout", issue_id="bd-abc123")
  → MCP server includes issue_id in DelegationRequest
  → Orchestrator emits DelegationRequested { ..., issue_id: Some("bd-abc123") }
  → ExecutorLineage.apply():
      node.issue_id = Some("bd-abc123")
  → TUI reads linkage from ViewContext.lineage:
      lineage.executors_for_issue("bd-abc123") → [executor-1]
  → IssuesPanel renders ◆ on linked issues
  → AgentsTree renders issue badge on linked executors
```

Linkage lives in ExecutorLineage (single source of truth), NOT in DashboardView.

---

## 4. TUI Components

### 4.1 IssuesPanel (Refactored — Stateful)

**Current:** Unit struct, two static methods, zero state.

**New:**

```rust
pub struct IssuesPanel {
    selected_idx: Option<usize>,
    focused: bool,
}
```

Methods:
- `new()` → `IssuesPanel { selected_idx: None, focused: false }`
- `set_focused(bool)` — border highlight style
- `select_next(issue_count: usize)` — wrapping increment
- `select_prev(issue_count: usize)` — wrapping decrement
- `selected_id<'a>(&self, issues: &'a [IssueSummary]) -> Option<&'a str>`
- `render(&self, issues: &[IssueSummary], frame, area, lineage: &ExecutorLineage)` — with selection highlight row + ◆ for linked issues
- `computed_height(count, available)` — unchanged

Rendering changes:
- Selected row: `Style::default().bg(Color::DarkGray)` (matches AgentsTree selection)
- Focused border: `Block::bordered().border_style(Style::default().fg(Color::Cyan))`
- Linked indicator: `◆` appended to ID cell when `lineage.executors_for_issue(id).any(|n| n.phase.is_active())`

### 4.2 Panel::Issues in Tab Cycle

```rust
pub enum Panel {
    Agents,
    Issues,  // NEW — skipped when tracked_issues is empty
    Log,
}
```

Tab cycle logic in `handle_key_inner`:

```rust
KeyCode::Tab => {
    self.focused_panel = match self.focused_panel {
        Panel::Agents => {
            if self.tracked_issues.is_empty() { Panel::Log } else { Panel::Issues }
        }
        Panel::Issues => Panel::Log,
        Panel::Log => Panel::Agents,
    };
    // Update focus flags on panels...
}
```

### 4.3 IssueFocus State Machine

```rust
pub enum IssueFocus {
    /// No issue focused. Log area shows ActivityLog or DetailPane (existing behavior).
    None,
    /// Issue selected, detail being fetched from backend.
    Loading { id: String },
    /// Full issue loaded. Detail pane shows IssueDetailPane.
    Loaded { id: String, issue: Box<spur_pm::Issue> },
}
```

Invalid states unrepresentable. `Box<Issue>` because Issue is 14 fields (~300 bytes) and IssueFocus is stored inline in DashboardView.

Transitions:
- `Enter` on selected issue → `None → Loading { id }`
- `IssueDetailFetched` event (matching id) → `Loading → Loaded`
- `Esc` → `Loaded/Loading → None`
- `IssueUpdated` event (matching id) → update issue in `Loaded` variant

### 4.4 IssueDetailPane (New Component)

Renders full `Issue` in the log area when `IssueFocus::Loaded`.

```
┌─ Issue: bd-abc123 ─────────────────────────────────────┐
│ Fix authentication timeout                              │
│ Status: in_progress   Priority: P1   Type: bug          │
│ Assignee: alice       Due: 2026-04-20                   │
│ Blocked by: bd-def456                                   │
│ Labels: backend, auth                                   │
│─────────────────────────────────────────────────────────│
│ The auth service times out after 30 seconds when the    │
│ token refresh endpoint is unreachable. This causes...   │
│                                                         │
│                                                         │
│ [o]pen [w]ip [b]locked [d]one   [W]ork  [Esc] back     │
└─────────────────────────────────────────────────────────┘
```

- Title: bold, full width
- Metadata: two columns, color-coded (priority, status colors match IssuesPanel)
- Body: scrollable with j/k when issue is focused (re-use scroll logic from DetailPane)
- Footer: action hints — single-char quick-keys for status, `W` for brain assignment

### 4.5 Dashboard Layout (Updated)

When agents exist AND issues exist AND issue is focused:

```
┌─────────────────────────────┐
│  AgentsTree (up to 12 rows) │  ← j/k when Panel::Agents
├─────────────────────────────┤
│  IssuesPanel (up to 25%)    │  ← j/k when Panel::Issues, selected row highlighted
├─────────────────────────────┤
│  IssueDetailPane            │  ← replaces ActivityLog when IssueFocus::Loaded
├─────────────────────────────┤
│  InputBar                   │
└─────────────────────────────┘
  StatusBar
```

When no issue focused: ActivityLog/DetailPane renders in the middle area (existing behavior).

### 4.6 Issue Badge in DetailPane Header (Phase 2)

When an executor is focused and has `issue_id`, a right-aligned badge appears in the
DetailPane border title. Zero extra vertical space, always visible regardless of active tab.

```rust
// In DashboardView, before calling detail_pane.render():
let issue_badge = self.focused_node.as_ref()
    .and_then(|id| lineage.node(id))
    .and_then(|n| n.issue_id.as_ref())
    .map(|iid| format_issue_badge(iid, &self.tracked_issues));

self.detail_pane.render(frame, area, node, issue_badge.as_deref());
```

DetailPane renders it as a right-aligned `Title` on the `Block`:

```rust
let mut block = Block::bordered();
block = block.title(Title::from(tab_labels).alignment(Alignment::Left));
if let Some(badge) = issue_badge {
    block = block.title(Title::from(format!(" {} ", badge)).alignment(Alignment::Right));
}
```

Badge format: `◆ bd-abc1 P1 Fix auth timeout...` — ID truncated to 8 chars, title
truncated to fit available width.

When user presses `I` while DetailPane is focused, it opens the linked issue in
IssueDetailPane (same as Enter on the issue in IssuesPanel).

**Why badge over a tab:** "What issue is this worker working on?" is a glance question.
A tab forces a switch; the badge answers instantly. A dedicated Issues tab is deferred
to Phase 4 when sub-issue hierarchy data justifies a tree view.

---

## 5. Key Bindings (Issues Panel Focused)

| Key | Action | When |
|---|---|---|
| `j` | Select next issue | Panel::Issues focused |
| `k` | Select previous issue | Panel::Issues focused |
| `Enter` | View issue detail (async fetch) | Issue selected |
| `Esc` | Close issue detail / unfocus panel | Detail open / panel focused |
| `o` | Set status: open | IssueFocus::Loaded |
| `w` | Set status: in_progress | IssueFocus::Loaded |
| `b` | Set status: blocked | IssueFocus::Loaded |
| `d` | Set status: closed | IssueFocus::Loaded |
| `W` | Work on this issue (send to brain) | Issue selected (panel or detail) |
| `I` | Open linked issue detail from executor | DetailPane focused, executor has issue_id |
| `Tab` | Cycle to Log panel | Any |

---

## 6. Slash Commands

### `/issues` — Refresh Issue List

```rust
// In SpurLocalSource::entries()
CommandEntry {
    name: "issues".into(),
    description: "Refresh issue list from tracker".into(),
    hint: None,
    source: CommandSource::Spur,
    dispatch: Dispatch::SpurLocal(Action::RefreshIssues),
}
```

### `/issue <subcommand>` — Issue Operations

Parsed in `submit_router.rs` before command registry lookup:

```
/issue show <id>              → Action::Issue(IssueAction::ViewDetail { id })
/issue update <id> -s <status> → Action::Issue(IssueAction::UpdateStatus { id, status })
/issue create <title>         → Action::Issue(IssueAction::Create { title })
/work <id>                    → Action::Issue(IssueAction::WorkOn { id })
```

---

## 7. Action & UserInput Extensions

### Action Enum

```rust
pub enum Action {
    // ... existing variants ...
    RefreshIssues,
    Issue(IssueAction),
}

pub enum IssueAction {
    ViewDetail { id: String },
    UpdateStatus { id: String, status: String },
    WorkOn { id: String },
    Create { title: String },
}
```

### UserInput Enum

```rust
pub enum UserInput {
    // ... existing variants ...
    RefreshIssues,
    GetIssueDetail { id: String },
    UpdateIssue { id: String, update: spur_pm::IssueUpdate },
    CreateIssue { title: String, body: Option<String>, priority: Option<i32>, issue_type: Option<String> },
}
```

---

## 8. SpurEventBody Extensions

```rust
pub enum SpurEventBody {
    // ... existing variants ...

    /// Response to UserInput::GetIssueDetail. Broadcast on event bus.
    /// Matches SessionsListed / IssuesLoaded precedent for request-response
    /// on the broadcast channel.
    IssueDetailFetched {
        /// The ID that was requested — TUI checks against focused issue
        /// to discard stale responses from races.
        requested_id: String,
        issue: spur_pm::Issue,
    },

    /// A new issue was created (by TUI or brain via MCP).
    IssueCreated {
        issue: spur_pm::IssueSummary,
    },

    /// Feedback for a failed issue operation.
    IssueCommandError {
        operation: String,
        error: String,
    },
}
```

### DelegationRequested Extension

```rust
SpurEventBody::DelegationRequested {
    from: SessionId,
    to_agent: String,
    task: String,
    request_id: String,
    delegation_plan: Option<String>,
    issue_id: Option<String>,  // NEW — propagated from MCP tool args
}
```

---

## 9. ExecutorLineage Extension

### ExecutorNode

```rust
pub struct ExecutorNode {
    // ... existing fields ...
    pub issue_id: Option<String>,  // NEW — set from DelegationRequested.issue_id
}
```

### New Method

```rust
impl ExecutorLineage {
    /// Return executor IDs linked to the given issue.
    pub fn executors_for_issue(&self, issue_id: &str) -> impl Iterator<Item = &ExecutorId> {
        self.nodes()
            .filter(move |n| n.issue_id.as_deref() == Some(issue_id))
            .map(|n| &n.id)
    }
}
```

---

## 10. IssueTracker Trait Extension (Phase 3)

```rust
#[async_trait]
pub trait IssueTracker: Send + Sync {
    // ... existing 4 methods ...
    async fn create_issue(&self, params: CreateIssueParams) -> anyhow::Result<Issue>;
}

#[derive(Debug, Clone, Default)]
pub struct CreateIssueParams {
    pub title: String,
    pub body: Option<String>,
    pub priority: Option<i32>,
    pub issue_type: Option<String>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
}
```

### BeadsAdapter Implementation

```
br add "{title}" --description "{body}" --priority {n} --type {type} --format json
```

If `parent_id` is set:
```
br deps add {new_id} --depends-on {parent_id} --type parent-child --format json
```

### GitHubAdapter Implementation

```
gh issue create --title "{title}" --body "{body}" --label "{labels}" --repo {repo} --json ...
```

Priority/issue_type mapped to labels (`priority:P1`, `type:bug`). parent_id ignored (GitHub has no native parent-child).

### MCP Tool Definition

```json
{
  "name": "create_issue",
  "description": "Create a new issue in the project tracker",
  "inputSchema": {
    "type": "object",
    "properties": {
      "title": { "type": "string" },
      "body": { "type": "string" },
      "priority": { "type": "integer", "minimum": 0, "maximum": 4 },
      "issue_type": { "type": "string", "enum": ["task", "bug", "feature", "improvement"] },
      "labels": { "type": "array", "items": { "type": "string" } },
      "parent_id": { "type": "string", "description": "Parent issue ID for sub-issues" }
    },
    "required": ["title"]
  }
}
```

---

## 11. beads.rs Bug Fix

### `since` Filter Not Wired in list_issues

`IssueFilter.since` is silently ignored. Fix by adding `--since` argument:

```rust
if let Some(since) = filter.since {
    args.push("--since".into());
    args.push(since.to_rfc3339());
}
```

---

## 12. Phased Delivery

### Phase 1: Interactive Panel + Brain Assignment (MVP)

**Value:** Human can browse issues, view details, direct brain to work on specific issues.

| Component | Changes |
|---|---|
| `issues_panel.rs` | Refactor to stateful struct with selection + focus |
| `issue_detail_pane.rs` | NEW — renders full Issue with action hints |
| `dashboard.rs` | Panel::Issues, IssueFocus enum, Tab cycle, key routing |
| `action.rs` | IssueAction enum, RefreshIssues |
| `app.rs` | UserInput variants, process_action handlers |
| `spur_local.rs` | `/issues` command |
| `submit_router.rs` | `/work <id>` prefix parsing |
| `events.rs` | IssueDetailFetched, IssueCommandError |
| `orchestrator.rs` | Handle RefreshIssues, GetIssueDetail, WorkOnIssue |

Estimated: ~500 lines

### Phase 2: Badge + Linkage + Quick Actions

**Value:** Issue badge in DetailPane header for instant context. Visual issue-executor linkage. Quick status updates from TUI.

| Component | Changes |
|---|---|
| `events.rs` | DelegationRequested gains issue_id |
| ExecutorNode | `issue_id: Option<String>` field |
| ExecutorLineage | `executors_for_issue()` method |
| `issues_panel.rs` | ◆ linked indicator, lineage-aware rendering |
| `detail_pane.rs` | `issue_badge: Option<&str>` param, right-aligned Title |
| `dashboard.rs` | Badge formatting, quick-key routing (o/w/b/d), `I` hotkey, IssueUpdated handling |
| `app.rs` | UserInput::UpdateIssue |
| `orchestrator.rs` | Handle UpdateIssue, propagate issue_id in events |

Estimated: ~350 lines

### Phase 3: Create Issue

**Value:** Brain can decompose work into sub-issues. TUI can create quick notes.

| Component | Changes |
|---|---|
| `adapter.rs` | `create_issue` method on IssueTracker |
| `types.rs` | CreateIssueParams struct |
| `beads.rs` | Implementation via `br add` |
| `github.rs` | Implementation via `gh issue create` |
| `service.rs` | Delegating `create_issue()` |
| `tools.rs` | create_issue MCP tool definition |
| `server.rs` | create_issue handler |
| `submit_router.rs` | `/issue create` parsing |
| `orchestrator.rs` | Handle CreateIssue, emit IssueCreated |

Estimated: ~400 lines

### Phase 4: Brain Context + Sub-Issue Hierarchy + Issues Tab

**Value:** Brain is proactive about issues. Sub-issue tree mirrors delegation tree. Issues tab in DetailPane shows hierarchy with progress.

| Component | Changes |
|---|---|
| `orchestrator.rs` | Issue context injection at brain session start |
| `config/mod.rs` | `[pm.brain_context]` config section |
| `issues_panel.rs` | Indented sub-issue rendering |
| `detail_pane.rs` | Optional Issues tab (hierarchy tree view) |
| `beads.rs` | Parent-child dependency creation in create_issue |

Estimated: ~350 lines

---

## 13. Risk Matrix

| Risk | Severity | Mitigation |
|---|---|---|
| Stale detail fetch (race) | Low | `requested_id` check in IssueDetailFetched handler |
| Refresh spam | Low | AtomicBool guard in orchestrator debounces |
| Empty panel Tab skip | Low | Check `tracked_issues.is_empty()` in Tab handler |
| Concurrent TUI + brain updates | Low | SQLite WAL + bounded retry, last-write-wins |
| Prompt injection via issue body | Low (beads) / Med (GitHub) | Local-first beads = user-created. Sanitize GitHub bodies. |
| Brain over-creates sub-issues | Medium | Rate limit in create_issue MCP handler (max 10/session) |
| Multiple executors per issue | Low | `executors_for_issue()` returns iterator, TUI highlights most recent active |

---

## 14. File Map

```
crates/spur-pm/src/
  adapter.rs         Phase 3: +create_issue method
  types.rs           Phase 3: +CreateIssueParams
  beads.rs           Phase 1: fix since filter; Phase 3: +create_issue impl
  github.rs          Phase 3: +create_issue impl
  service.rs         Phase 3: +delegating create_issue

crates/spur-acp/src/domain/
  events.rs          Phase 1: +IssueDetailFetched, +IssueCommandError, +IssueCreated
                     Phase 2: DelegationRequested +issue_id

crates/spur-core/src/
  orchestrator.rs    Phase 1: handle RefreshIssues, GetIssueDetail
                     Phase 2: handle UpdateIssue, propagate issue_id
                     Phase 3: handle CreateIssue
                     Phase 4: brain context injection
  lineage.rs         Phase 2: ExecutorNode.issue_id, executors_for_issue()

crates/spur-mcp/src/
  tools.rs           Phase 3: create_issue tool definition
  server.rs          Phase 3: create_issue handler

crates/spur-tui/src/
  action.rs          Phase 1: +IssueAction enum, +RefreshIssues
  app.rs             Phase 1: +UserInput variants, process_action
  commands/
    spur_local.rs    Phase 1: +/issues command
    submit_router.rs Phase 1: +/work prefix; Phase 3: +/issue create
  components/
    issues_panel.rs  Phase 1: refactor stateful; Phase 2: ◆ linkage; Phase 4: indent
    issue_detail_pane.rs  Phase 1: NEW
    detail_pane.rs   Phase 2: +issue_badge param, right-aligned Title; Phase 4: +Issues tab
  views/
    dashboard.rs     Phase 1: Panel::Issues, IssueFocus, key routing
                     Phase 2: badge formatting, quick-keys, I hotkey, IssueUpdated handling
```

---

## 15. ASCII Wireframes

### 15.1 Dashboard — Issues Panel Visible, No Focus

Default state when issues are loaded. IssuesPanel is read-only until Tab-focused.
`◆` next to an issue ID means an active executor is linked to that issue.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (streaming ▸▸▸)                                             │
│    ├─ worker-1 (running)     auth-handler        ◆ bd-abc1           │
│    ├─ worker-2 (done ✓)      api-tests                               │
│    └─ worker-3 (reviewing)   db-migration        ◆ bd-def4           │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─────────────────────────────────────────────────────────┐
│  ID        P  Type    Sts   Assignee   Title                         │
│  bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│  bd-ghi7   P2 task    open  --         Update API documentation      │
└──────────────────────────────────────────────────────────────────────┘
┌─ Activity ───────────────────────────────────────────────────────────┐
│  14:32:01 [brain]     Delegating to worker-1: Fix auth timeout       │
│  14:32:03 [worker-1]  Tool: Read src/auth/handler.rs                 │
│  14:32:15 [worker-1]  Tool: Edit src/auth/handler.rs                 │
│  14:33:01 [pm]        Issue bd-abc1 (beads) updated: in_progress     │
│                                                                       │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘
 [/] command · [@] mention · [!] interrupt · [Alt+I] vim · ? for help
 2 running · 1 review · $0.42 · 3m 21s · 3 issues ·               ? help
```

### 15.2 Issues Panel Focused (Tab → Panel::Issues)

Border turns cyan. Hint bar shows available keys. Selected row has `▌` gutter marker.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (streaming ▸▸▸)                                             │
│    ├─ worker-1 (running)     auth-handler        ◆ bd-abc1           │
│    ├─ worker-2 (done ✓)      api-tests                               │
│    └─ worker-3 (reviewing)   db-migration        ◆ bd-def4           │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─── [Tab] cycle · [j/k] select · [Enter] detail ────────┐
│  ID        P  Type    Sts   Assignee   Title                     ◆   │
│▌ bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │ ◄── selected (bg highlight)
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│  bd-ghi7   P2 task    open  --         Update API documentation      │
└──────────────────────────────────────────────────────────────────────┘
┌─ Activity ───────────────────────────────────────────────────────────┐
│  14:32:01 [brain]     Delegating to worker-1: Fix auth timeout       │
│  14:32:03 [worker-1]  Tool: Read src/auth/handler.rs                 │
│  14:32:15 [worker-1]  Tool: Edit src/auth/handler.rs                 │
│  14:33:01 [pm]        Issue bd-abc1 (beads) updated: in_progress     │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘

 2 running · 1 review · $0.42 · 3m 21s · 3 issues ·               ? help
```

### 15.3 Issue Detail Open (Enter on Selected Issue)

IssueDetailPane replaces ActivityLog area. Shows full issue body, metadata, and
action hint footer. Body area is scrollable with j/k.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (streaming ▸▸▸)                                             │
│    ├─ worker-1 (running)     auth-handler        ◆ bd-abc1           │
│    └─ worker-3 (reviewing)   db-migration        ◆ bd-def4           │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─────────────────────────────────────────────────────────┐
│▌ bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│  bd-ghi7   P2 task    open  --         Update API documentation      │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issue: bd-abc123 ───────────────────────────────────────────────────┐
│                                                                       │
│  Fix authentication timeout                                           │
│                                                                       │
│  Status: in_progress    Priority: P1       Type: bug                  │
│  Assignee: worker-1     Due: 2026-04-20                               │
│  Blocked by: bd-def456                                                │
│  Labels: backend, auth                                                │
│ ───────────────────────────────────────────────────────────────────── │
│  The auth service times out after 30 seconds when the token           │
│  refresh endpoint is unreachable. This causes the entire login        │
│  flow to hang until the TCP socket timeout fires.                     │
│                                                                       │
│  Acceptance criteria:                                                 │
│  - Timeout reduced to 5s with graceful fallback                       │
│  - Retry with exponential backoff (max 3 attempts)                    │
│  - Unit tests for timeout + retry paths                               │
│                                                                       │
│ ───────────────────────────────────────────────────────────────────── │
│  [o]pen [w]ip [b]locked [d]one     [W]ork on this     [Esc] back     │
└──────────────────────────────────────────────────────────────────────┘

 2 running · 1 review · $0.42 · 3m 21s · 3 issues ·               ? help
```

### 15.4 Issue Loading State (Enter → Waiting for Fetch)

Transient state while orchestrator fetches full Issue from PmService.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (streaming ▸▸▸)                                             │
│    ├─ worker-1 (running)     auth-handler        ◆ bd-abc1           │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─────────────────────────────────────────────────────────┐
│▌ bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│  bd-ghi7   P2 task    open  --         Update API documentation      │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issue: bd-abc123 ───────────────────────────────────────────────────┐
│                                                                       │
│                                                                       │
│                                                                       │
│                  Loading issue bd-abc123...                            │
│                                                                       │
│                                                                       │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘

 2 running · 1 review · $0.42 · 3m 21s · 3 issues ·               ? help
```

### 15.5 Executor Focused — Issue Badge in DetailPane Header (Phase 2)

When a focused executor has `issue_id`, the badge appears right-aligned in the
DetailPane border. No tab switch needed — always visible alongside Stream content.
Press `I` to open full issue detail.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (streaming ▸▸▸)                                             │
│    ├─►worker-1 (running)     auth-handler        ◆ bd-abc1  ◄─ focused
│    ├─ worker-2 (done ✓)      api-tests                               │
│    └─ worker-3 (reviewing)   db-migration        ◆ bd-def4           │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─────────────────────────────────────────────────────────┐
│  bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│  bd-ghi7   P2 task    open  --         Update API documentation      │
└──────────────────────────────────────────────────────────────────────┘
┌─[Stream] Artifacts  Task  Review ────── ◆ bd-abc1 P1 Fix auth...─┐
│                                                                    │
│  > Thinking...                                                     │
│    Looking at the auth handler in src/auth/handler.rs. The         │
│    timeout is hardcoded at 30s on line 142. I'll change it         │
│    to use the configurable timeout from AuthConfig.                │
│                                                                    │
│  ! Act: Edit src/auth/handler.rs                                   │
│    - Line 142: timeout: Duration::from_secs(30)                    │
│    + Line 142: timeout: config.auth_timeout                        │
│                                                                    │
│  ! Act: Edit src/auth/config.rs                                    │
│    + pub auth_timeout: Duration,  // default: 5s                   │
│                                                                    │
│  > Thinking...                                                     │
│    Now I need to add the retry logic with exponential backoff...   │
│                                                                    │
│                                                     [I]ssue detail │
└────────────────────────────────────────────────────────────────────┘

 2 running · 1 review · $0.42 · 3m 21s · 3 issues ·             ? help
```

### 15.6 Executor Focused — No Linked Issue (Badge Absent)

When executor has no `issue_id`, DetailPane renders exactly as today.
No badge, no hint — zero visual noise.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (streaming ▸▸▸)                                             │
│    ├─ worker-1 (running)     auth-handler        ◆ bd-abc1           │
│    ├─►worker-2 (done ✓)      api-tests                    ◄─ focused │
│    └─ worker-3 (reviewing)   db-migration        ◆ bd-def4           │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─────────────────────────────────────────────────────────┐
│  bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│  bd-ghi7   P2 task    open  --         Update API documentation      │
└──────────────────────────────────────────────────────────────────────┘
┌─[Stream] Artifacts  Task  Review ────────────────────────────────────┐
│                                                                       │
│  > Thinking...                                                        │
│    Running the test suite for the new API endpoints.                  │
│                                                                       │
│  ! Act: Bash cargo test --package spur-api                            │
│    running 12 tests                                                   │
│    test api::auth::test_login ... ok                                  │
│    test api::auth::test_refresh ... ok                                │
│    test api::users::test_create ... ok                                │
│    ...                                                                │
│    test result: ok. 12 passed; 0 failed                               │
│                                                                       │
│  > Session completed successfully                                     │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘

 1 running · 1 review · $0.42 · 3m 21s · 3 issues ·             ? help
```

### 15.7 "W" Key — Work On Issue (Prompt Sent to Brain)

User presses `W` on selected issue. TUI constructs a prompt from the IssueSummary
and sends it to the brain. The prompt appears in the activity log.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (thinking ···)                                              │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (3) ─────────────────────────────────────────────────────────┐
│  bd-abc1   P1 bug     wip   worker-1   Fix authentication timeout  ◆ │
│  bd-def4   P0 bug     wip   worker-3   DB migration deadlock       ◆ │
│▌ bd-ghi7   P2 task    open  --         Update API documentation      │ ◄── user pressed W
└──────────────────────────────────────────────────────────────────────┘
┌─ Activity ───────────────────────────────────────────────────────────┐
│  14:35:12 [you]       Work on this issue:                            │
│                       Issue: bd-ghi7 -- Update API documentation     │
│                       Priority: P2 | Type: task | Status: open       │
│                                                                       │
│                       Use `get_issue` tool to read full details.      │
│                       Use `delegate_to_worker` with                   │
│                       issue_id="bd-ghi7" for delegations.            │
│                                                                       │
│  14:35:13 [brain]     ···  (thinking)                                │
│                                                                       │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘
 [brain: ···]
 1 running · 0 review · $0.42 · 5m 12s · 3 issues ·               ? help
```

### 15.8 Empty State — No Issues Loaded

When `.beads/` has no issues or PM is not configured. IssuesPanel is hidden entirely
(zero height). Tab cycle skips Panel::Issues. Dashboard looks identical to pre-beads TUI.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (ready)                                                     │
└──────────────────────────────────────────────────────────────────────┘
┌─ Activity ───────────────────────────────────────────────────────────┐
│  14:30:00 [brain]     Brain agent spawned                            │
│  14:30:01 [pm]        0 issues loaded                                │
│                                                                       │
│                                                                       │
│                                                                       │
│                                                                       │
│                                                                       │
│                                                                       │
│                                                                       │
│                                                                       │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘
 [/] command · [@] mention · [!] interrupt · [Alt+I] vim · ? for help
 0 running · 0 review · $0.00 · 0m 01s ·                          ? help
```

### 15.9 Phase 4 Vision — Sub-Issue Hierarchy + Issues Tab

When sub-issues exist (Phase 4), the IssuesPanel shows indented children and progress
counts. A dedicated Issues tab appears in DetailPane showing the full hierarchy tree
with executor linkage and progress bar.

```
┌─ Agents ─────────────────────────────────────────────────────────────┐
│  ▾ brain (idle)                                                      │
│    ├─►worker-1 (running)     auth-handler        ◆ bd-001            │
│    ├─ worker-2 (done ✓)      auth-tests          ◆ bd-002            │
│    └─ worker-3 (running)     auth-docs           ◆ bd-003            │
└──────────────────────────────────────────────────────────────────────┘
┌─ Issues (6) ─────────────────────────────────────────────────────────┐
│  bd-000   P1 task    wip   --         Refactor auth system     2/5 ◆ │
│  bd-abc1  P1 bug     done  --         Fix authentication timeout     │
│  bd-def4  P0 bug     done  --         DB migration deadlock          │
└──────────────────────────────────────────────────────────────────────┘
┌─ Stream  Artifacts  Task  Review  [Issues] ── ◆ bd-001 P2 handler─┐
│                                                                     │
│  Parent: bd-000 "Refactor auth system" (wip, 2/5 done)             │
│  ┌──────────────────────────────────────────────────────────┐      │
│  │  ok bd-001  P2  Fix handler        done    > worker-1    │      │
│  │  ok bd-002  P2  Add tests          done    > worker-2    │      │
│  │  >> bd-003  P3  Update docs        wip     > worker-3    │      │
│  │     bd-004  P2  Migration script   pending               │      │
│  │     bd-005  P3  Cleanup old code   pending               │      │
│  └──────────────────────────────────────────────────────────┘      │
│                                                                     │
│  Progress: ================------------- 40% (2/5)                  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘

 2 running · 0 review · $1.23 · 12m 45s · 6 issues ·             ? help
```

### 15.10 Phase Progression Summary

Visual evolution across phases. Each phase adds capability without removing
existing UI. Phase 1 is the foundation; each subsequent phase layers on top.

```
Phase 1 (MVP)                    Phase 2 (Badge+Linkage)
-------------------------------  ---------------------------------
+-- Agents ---------------+      +-- Agents ---------------+
| workers                 |      | workers            * id |    * = linked indicator
+-------------------------+      +-------------------------+
+-- Issues ---------------+      +-- Issues ---------------+
| j/k/Enter/W             |      | j/k + o/w/b/d          |
| |selected row           |      | |selected          *   |
+-------------------------+      +-------------------------+
+-- IssueDetail ----------+      +--[Stream]-----* badge--+     badge = right-aligned
| body/meta               |      | stream content         |     in border title
| [o][w][b][d]            | (or) |                        |
| [W]ork [Esc]            |      |           [I] detail   |
+-------------------------+      +-------------------------+

Phase 3 (Create)                 Phase 4 (Hierarchy)
-------------------------------  ---------------------------------
+-- Agents ---------------+      +-- Agents ---------------+
| workers            * id |      | workers            * id |
+-------------------------+      +-------------------------+
+-- Issues ---------------+      +-- Issues ---------------+
| /issue create <title>    |      | + indent tree           |
| |new issue appears       |      | |parent         2/5 *  |
+-------------------------+      +-------------------------+
+--[Stream]-----* badge--+      +-- Stream [Issues] * ---+     Issues tab = tree
| stream content         |      | sub-issue tree         |     with progress bar
|                        |      | progress bar           |
|           [I] detail   |      |                        |
+-------------------------+      +-------------------------+
```
