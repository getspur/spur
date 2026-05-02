# TUI Backlog / Sprints / Sprint Navigation Design

Date: 2026-05-02

## Summary

SPUR should split issue browsing, plan ownership browsing, and active plan execution into three separate TUI surfaces:

- **Backlog**: repository issue inventory, implemented by `IssueBrowserView`.
- **Sprints**: persisted plan ownership/control plane, implemented by new `PlanBrowserView`.
- **Sprint**: current brain-owned plan execution cockpit, implemented by `PlanInspectorView`.

The first-principles boundary is write authority. Backlog can browse broad repository state. Sprints can browse durable plans and decide ownership actions. Sprint may only inspect and operate on the plan already assigned to the current brain session.

## Core Invariants

1. `PlanInspectorView` only shows the current brain session's active/owned plan.
2. `IssueBrowserView` remains issue-first and must not become a plan ownership browser.
3. `PlanBrowserView` is the only general persisted-plan browser.
4. A plan opens in `PlanInspectorView` only when ownership is confirmed as current brain.
5. `owned_by_other` and `ambiguous_owner` plans cannot open in `PlanInspectorView`.
6. MVP `resume_plan` may claim only unowned plans. Active handoff is explicitly deferred.
7. Active execution is one-to-one: one active plan has exactly one owner brain, and one brain session may own at most one active nonterminal plan.

## Active Plan Cardinality

A brain session is the reasoning, context, and worker-orchestration owner for a plan. Allowing one brain session to own multiple active plans would make the Sprint view ambiguous and would risk mixing worker dispatch, review, and repair state across unrelated plans.

Therefore SPUR enforces:

```text
count(active nonterminal plans owned by current brain session) <= 1
```

Definitions:

- **Owned by current brain**: epic has `spur:plan-owner:<current_brain_session>`.
- **Active**: lifecycle is not terminal.
- **Terminal**: complete, failed, cancelled, or any future terminal state that no longer accepts worker dispatch/review writes.

Allowed:

```text
One brain session owns one running plan.
Many brain sessions each own one running plan.
One brain session has multiple historical/terminal plans.
```

Blocked:

```text
One brain session owns two running plans.
One brain session resumes an unowned plan while already owning another active plan.
One brain session executes a new epic while already owning another active plan.
```

This is a server-side invariant. TUI checks improve UX, but MCP must enforce it in `execute_epic` and `resume_plan`.

## Mental Model

```text
Backlog  = IssueBrowserView   = repository work inventory
Sprints  = PlanBrowserView    = persisted plan ownership/control
Sprint   = PlanInspectorView  = current brain-owned execution view
```

## Component Architecture

```mermaid
flowchart TB
    User[User]
    App[TUI App]
    Backlog[IssueBrowserView\nBacklog]
    Sprints[PlanBrowserView\nSprints]
    Sprint[PlanInspectorView\nSprint]
    PM[PM / Beads]
    Brain[Current Brain Session]
    MCP[MCP Callback Server]
    Projection[PlanProjectionStore]

    User --> App
    App --> Backlog
    App --> Sprints
    App --> Sprint

    Backlog -- RefreshIssues / ViewDetail / ExecuteEpic --> App
    Sprints -- RefreshPlans / ResumePlan / OpenSprint --> App
    Sprint -- View task issue detail --> App

    App -- UserInput --> Brain
    Brain --> MCP
    MCP --> PM
    MCP -- Plan ownership labels --> PM
    Brain -- PlanSnapshotUpdated --> Projection
    Projection --> Sprint

    PM -- IssuesLoaded / IssueDetailFetched --> Backlog
    PM -- PlansLoaded --> Sprints
```

## View Responsibilities

### Backlog: `IssueBrowserView`

Backlog is repository-scoped. It answers "what work exists?"

Responsibilities:

- List PM issues and epics.
- Open issue detail.
- Open issue dependency graph.
- Start `execute_epic` from an epic.
- Navigate to Sprints after plan creation if appropriate.

Non-responsibilities:

- It does not decide plan ownership.
- It does not list persisted plans as first-class objects.
- It does not open `PlanInspectorView` directly from a raw epic.

### Sprints: `PlanBrowserView`

Sprints is ownership-scoped. It answers "which persisted plans exist and which can this brain own?"

Responsibilities:

- List persisted plan epics.
- Highlight the current brain's single active Sprint slot.
- Display ownership state.
- Display plan lifecycle/progress summary.
- Resume unowned plans.
- Open current-brain-owned plans into Sprint.
- Block owned-by-other and ambiguous plans.
- Block resume/execute when the current brain already owns another active plan.
- Link back to the source epic in Backlog.

### Sprint: `PlanInspectorView`

Sprint is execution-scoped. It answers "what is happening inside the current brain's assigned plan?"

Responsibilities:

- Show task DAG/stages for the current active plan.
- Show task execution and review state.
- Show linked issue detail for tasks.
- Handle task detail scrolling and issue-detail loading/error states.

Non-responsibilities:

- It does not browse all persisted plans.
- It does not reclaim ownership.
- It does not show plans not assigned to the current brain session.

## Data Contracts

### Backlog Issue Summary

```rust
struct IssueSummaryEvent {
    id: String,
    title: String,
    status: String,
    issue_type: Option<String>,
    priority: Option<i32>,
    assignee: Option<String>,
}
```

### Sprints Plan Summary

```rust
struct PlanSummaryEvent {
    plan_id: String,
    epic_id: String,
    title: String,
    owner_state: PlanOwnerStateEvent,
    lifecycle: PlanLifecycleEvent,
    counts: Option<PlanSummaryCountsEvent>,
    updated_at: Option<DateTime<Utc>>,
}

enum PlanOwnerStateEvent {
    Mine,
    Unowned,
    Other { owner: String },
    Ambiguous { owners: Vec<String> },
}

enum PlanLifecycleEvent {
    Pending,
    Running,
    AwaitingReview,
    Complete,
    Failed,
    Unknown,
}
```

### Sprint Active Plan

`PlanInspectorView` should continue to use:

```rust
ctx.plan_projection.current_for_session(&self.session_id)
```

This is the correct guardrail: no session plan projection means no active Sprint for this brain.

## ASCII Wireframes

### Backlog

```text
┌──────────────────────────── Backlog ────────────────────────────┐
│ Filter: open/all   Search: ____        r Refresh   E Execute     │
├───────┬──────────┬────────────┬───────────────┬─────────────────┤
│ Type  │ ID       │ Status     │ Priority      │ Title           │
├───────┼──────────┼────────────┼───────────────┼─────────────────┤
│ epic  │ bd-120   │ open       │ P1            │ Auth migration  │
│ task  │ bd-121   │ open       │ P2            │ Token cleanup   │
│ epic  │ bd-122   │ blocked    │ P2            │ TUI sprint flow │
└───────┴──────────┴────────────┴───────────────┴─────────────────┘
│ Enter detail   v graph   W work task   E execute epic   Esc back │
└──────────────────────────────────────────────────────────────────┘
```

### Backlog Issue Detail

```text
┌──────────── Backlog ────────────┬──────────── Issue Detail ──────┐
│ > epic bd-120 Auth migration    │ bd-120                         │
│   task bd-121 Token cleanup     │ Status: open   Priority: P1    │
│   epic bd-122 TUI sprint flow   │ Labels: spur:source-issue      │
│                                 │                                │
│                                 │ Description                    │
│                                 │ Implement auth migration...    │
└─────────────────────────────────┴────────────────────────────────┘
│ PgUp/PgDn scroll   Enter close detail   E execute epic            │
└───────────────────────────────────────────────────────────────────┘
```

### Sprints

```text
┌──────────────────────────── Sprints ─────────────────────────────┐
│ Current Sprint: plan-a1 running 2/7        Enter Open Sprint      │
│ r Refresh   Enter Open   R Resume   b Backlog   Esc Dashboard     │
├──────────────┬──────────┬──────────────┬──────────┬──────────────┤
│ Plan         │ Epic     │ Owner        │ State    │ Progress     │
├──────────────┼──────────┼──────────────┼──────────┼──────────────┤
│ plan-a1      │ bd-120   │ mine         │ running  │ 2/7 done     │
│ plan-b2      │ bd-122   │ unowned      │ paused   │ blocked      │
│ plan-c3      │ bd-130   │ other-brain  │ running  │ 1/5 done     │
│ plan-d4      │ bd-140   │ ambiguous    │ invalid  │ --           │
└──────────────┴──────────┴──────────────┴──────────┴──────────────┘
│ mine: Enter opens Sprint   unowned: R resumes only if no current   │
└───────────────────────────────────────────────────────────────────┘
```

### Sprints Detail

```text
┌────────────── Sprints ──────────────┬──────────── Plan Detail ────┐
│ > plan-a1  mine        running      │ Plan: plan-a1               │
│   plan-b2  unowned     paused       │ Epic: bd-120                │
│   plan-c3  other       running      │ Owner: current brain        │
│   plan-d4  ambiguous   invalid      │ Tasks: ready 2 running 1    │
│                                     │ Next: wait for review       │
└─────────────────────────────────────┴─────────────────────────────┘
│ Enter Open Sprint   R Resume   e View Epic   r Refresh            │
└───────────────────────────────────────────────────────────────────┘
```

### Sprint

```text
┌──────────────────────────── Sprint: plan-a1 ─────────────────────┐
│ Status running       Progress 2/7 reviewed       Next review task │
├──────── Ready ───────┬────── Running ──────┬──── Awaiting Review ─┤
│ > bd-121 Token clean │ bd-124 API update   │ bd-126 Tests         │
│   bd-122 Model sync  │                     │                      │
├──────────────────────┴─────────────────────┴──────────────────────┤
│ Task detail                                                        │
│ task: bd-121                                                       │
│ issue: bd-121                                                      │
│ status: ready                                                      │
│ agent: codex                                                       │
│                                                                    │
│ Issue                                                              │
│ title: Token cleanup                                               │
│ description: ...                                                   │
└────────────────────────────────────────────────────────────────────┘
│ h/l lane   j/k task   Enter issue detail   PgUp/PgDn scroll        │
│ Esc back to Sprints                                                │
└────────────────────────────────────────────────────────────────────┘
```

## User Journeys

### Journey 1: Create Sprint From Backlog

```mermaid
sequenceDiagram
    actor U as User
    participant B as Backlog / IssueBrowserView
    participant App as TUI App
    participant Brain as Current Brain
    participant MCP as MCP Server
    participant PM as Beads / PM
    participant S as Sprints / PlanBrowserView

    U->>B: Select epic
    U->>B: Press E Execute
    B->>App: Action::Issue(ExecuteEpic { epic_id })
    App->>Brain: UserInput::ExecuteEpic
    Brain->>MCP: execute_epic(epic_id)
    MCP->>PM: Check current brain active plan count
    alt current brain already owns active plan
        MCP-->>Brain: error: ActivePlanAlreadyOwned
        Brain-->>App: show blocked hint
    else no active plan owned by current brain
    MCP->>PM: Persist plan labels + owner label
    MCP-->>Brain: Plan status
    Brain-->>App: PlanSnapshotUpdated
    App->>S: Navigate or refresh Sprints
    S->>PM: Refresh plans
    PM-->>S: PlansLoaded(owner_state = Mine)
    end
```

### Journey 2: Resume Unowned Sprint

```mermaid
sequenceDiagram
    actor U as User
    participant S as Sprints / PlanBrowserView
    participant App as TUI App
    participant Brain as Current Brain
    participant MCP as MCP Server
    participant PM as Beads / PM
    participant I as Sprint / PlanInspectorView

    U->>S: Select unowned plan
    U->>S: Press R Resume
    S->>App: Action::ResumePlan { plan_id }
    App->>Brain: UserInput::ResumePlan { plan_id }
    Brain->>MCP: resume_plan(plan_id)
    MCP->>PM: Check current brain active plan count
    alt current brain already owns another active plan
        MCP-->>Brain: error: ActivePlanAlreadyOwned
        Brain-->>App: show blocked hint
    else no active plan owned by current brain
    MCP->>PM: Add current plan-owner label
    MCP->>PM: Reload epic and classify owner
    PM-->>MCP: owner_state = Mine
    MCP-->>Brain: claimed
    Brain-->>App: PlansLoaded / PlanSnapshotUpdated
    App->>S: Refresh selected row as Mine
    U->>S: Press Enter Open
    S->>App: NavigateTo(PlanInspector(current_session))
    App->>I: Render current session plan
    end
```

### Journey 3: Block Active Handoff in MVP

```mermaid
sequenceDiagram
    actor U as User
    participant S as Sprints / PlanBrowserView
    participant App as TUI App
    participant Brain as Current Brain
    participant MCP as MCP Server
    participant PM as Beads / PM

    U->>S: Select owned-by-other plan
    U->>S: Press R Resume
    S->>App: Action::ResumePlan { plan_id }
    App->>Brain: UserInput::ResumePlan { plan_id }
    Brain->>MCP: resume_plan(plan_id)
    MCP->>PM: Read owner labels
    PM-->>MCP: owner_state = Other
    MCP-->>Brain: error: active handoff not implemented
    Brain-->>App: IssueCommandError / PlanCommandError
    App-->>S: Show blocked hint
```

### Journey 4: Inspect Current Sprint

```mermaid
sequenceDiagram
    actor U as User
    participant S as Sprints / PlanBrowserView
    participant App as TUI App
    participant P as PlanProjectionStore
    participant I as Sprint / PlanInspectorView

    U->>S: Select owned-by-me plan
    U->>S: Press Enter
    S->>App: NavigateTo(PlanInspector(current_session))
    App->>P: current_for_session(current_session)
    P-->>App: TrackedPlan
    App->>I: Render Sprint
    U->>I: Navigate lanes/tasks
    U->>I: Press Enter on linked issue
    I->>App: Action::Issue(ViewDetail { issue_id })
```

## View State Transitions

```mermaid
stateDiagram-v2
    [*] --> Dashboard

    Dashboard --> Backlog: Open Backlog
    Dashboard --> Sprints: Open Sprints
    Dashboard --> Sprint: Open active Sprint

    Backlog --> BacklogDetail: Enter issue
    BacklogDetail --> Backlog: Enter / Esc
    Backlog --> BacklogGraph: v
    BacklogGraph --> Backlog: v / Esc
    Backlog --> Sprints: Execute epic succeeds
    Backlog --> Backlog: Execute blocked, active sprint exists
    Backlog --> Dashboard: Esc

    Sprints --> Sprint: Enter on Mine
    Sprints --> SprintsRefreshing: r Refresh
    SprintsRefreshing --> Sprints: PlansLoaded
    Sprints --> SprintsRefreshing: R Resume Unowned, no active Mine
    Sprints --> Sprints: R Resume blocked, active Mine exists
    Sprints --> BacklogDetail: e View Epic
    Sprints --> Dashboard: Esc

    Sprint --> SprintIssueLoading: Enter on task issue
    SprintIssueLoading --> SprintIssueDetail: IssueDetailFetched
    SprintIssueLoading --> SprintIssueError: IssueCommandError
    SprintIssueDetail --> Sprint: Esc
    SprintIssueError --> SprintIssueLoading: Enter retry
    SprintIssueError --> Sprint: Esc
    Sprint --> Sprints: Esc
```

## Ownership State Transitions

```mermaid
stateDiagram-v2
    [*] --> Unowned

    Unowned --> Mine: resume_plan success, brain has no active plan
    Unowned --> Unowned: resume_plan blocked, brain already owns active plan
    Unowned --> Other: race, other claimed first
    Unowned --> Ambiguous: race, conflicting labels detected

    Mine --> SprintOpen: Open Sprint
    SprintOpen --> Mine: Back to Sprints
    Mine --> Complete: plan terminal
    Mine --> Ambiguous: invariant violation

    Other --> Other: MVP resume attempt blocked
    Other --> Mine: future explicit handoff

    Ambiguous --> Ambiguous: all writes/open blocked
    Ambiguous --> Unowned: future manual repair
    Complete --> [*]
```

## Brain Active-Plan Slot

```mermaid
stateDiagram-v2
    [*] --> Empty

    Empty --> Occupied: execute_epic success
    Empty --> Occupied: resume_plan success

    Occupied --> Occupied: Open Sprint
    Occupied --> Occupied: execute_epic blocked
    Occupied --> Occupied: resume another plan blocked
    Occupied --> Empty: owned plan reaches terminal lifecycle
    Occupied --> Empty: future explicit transfer/abandon
```

## Sprint Issue Detail State

```mermaid
stateDiagram-v2
    [*] --> Closed

    Closed --> Loading: Enter task with issue_id
    Loading --> Loaded: IssueDetailFetched matching id
    Loading --> Error: IssueCommandError matching id
    Loading --> Error: IssueCommandError no id and exactly one loading
    Loading --> Closed: Esc

    Loaded --> Loaded: PgUp / PgDn scroll
    Loaded --> Closed: Switch task
    Loaded --> Closed: Esc

    Error --> Loading: Enter retry
    Error --> Closed: Esc
```

## PlanBrowser MVP Behavior

### Row Actions

| Owner state | Enter | R Resume | e View Epic |
| --- | --- | --- | --- |
| `Mine` | Open Sprint | No-op / already owned hint | Backlog detail |
| `Unowned` | Hint: resume first | `resume_plan` only if no active Mine | Backlog detail |
| `Other` | Blocked hint | Blocked hint | Backlog detail |
| `Ambiguous` | Blocked hint | Blocked hint | Backlog detail |

### Active Slot Actions

| Current brain active plan | Backlog Execute Epic | Sprints Resume Unowned | Sprints Open Mine |
| --- | --- | --- | --- |
| None | Allowed | Allowed | No-op unless row is Mine terminal/history |
| One active Mine | Blocked | Blocked | Allowed only for that active plan |

### Empty States

```text
No plans found.
Press b to open Backlog and execute an epic.
```

```text
No sprint owned by this brain.
Open Sprints to resume or Backlog to execute an epic.
```

## Required TUI Additions

```rust
enum ViewId {
    Dashboard,
    IssueBrowser,
    PlanBrowser,
    PlanInspector(SessionId),
    // ...
}

enum Action {
    RefreshPlans,
    ResumePlan { plan_id: String },
    NavigateTo(ViewId),
    // ...
}

enum UserInput {
    RefreshPlans,
    ResumePlan { plan_id: String },
    // ...
}
```

## Required Event Additions

```rust
enum SpurEventBody {
    PlansLoaded {
        plans: Vec<PlanSummaryEvent>,
    },
    PlanCommandError {
        operation: String,
        plan_id: Option<String>,
        error: String,
    },
    // ...
}
```

`PlanCommandError` can be deferred if the MVP reuses the existing command error surface, but a plan-specific error event is cleaner once PlanBrowser exists.

## Navigation Rules

Never allow:

```text
Backlog raw epic -> Sprint
Sprints owned-by-other -> Sprint
Sprints ambiguous -> Sprint
Sprint selecting arbitrary persisted plan
Brain session opening a second active Sprint
```

Only allow:

```text
Sprints Mine active plan -> Sprint(current_session)
```

`PlanInspectorView` still guards with:

```rust
ctx.plan_projection.current_for_session(&self.session_id)
```

If there is no current projection, render:

```text
No active sprint for this brain session.
Open Sprints to resume or Backlog to execute an epic.
```

## MVP Implementation Order

1. Add `PlanSummaryEvent` and `PlansLoaded`.
2. Add `RefreshPlans` / `ResumePlan` user input bridge.
3. Add MCP/orchestrator plan listing from beads labels.
4. Add `PlanBrowserView` read-only list.
5. Add `ResumePlan` action for unowned rows.
6. Add server-side active-plan cardinality checks to `execute_epic` and `resume_plan`.
7. Add `Open Sprint` action for the single active `Mine` row only.
8. Add route wiring and status-bar hints.
9. Add snapshot tests for all owner-state rows and active-slot blocked states.
10. Add integration tests for resume/open/execute blocked cases.

## Current Implementation Status (feat/tui-backlog-sprints-sprint-navigation)

As of this worktree implementation, the spec is in an MVP-complete state for the first three flows:

1. `Backlog -> execute_epic -> Sprints`
2. `Sprints -> resume unowned -> Sprints`
3. `Sprints (Mine) -> Sprint (PlanInspector)`

Status snapshot:

| Spec Item | Status | Location |
| --- | --- | --- |
| Plan summary contract (`PlansLoaded`, `PlanSummaryEvent`) | done | `crates/spur-acp/src/domain/events.rs`, `crates/spur-acp/src/domain/mod.rs` |
| Plan list refresh and request path | done | `crates/spur-tui/src/action.rs`, `crates/spur-tui/src/app.rs` |
| `PlanBrowserView` and owner-state rendering | done | `crates/spur-tui/src/views/plan_browser.rs` |
| Resume action for `Unowned` only | done | `crates/spur-tui/src/action.rs`, `crates/spur-tui/src/views/plan_browser.rs`, `crates/spur-tui/src/app.rs` |
| Active-plan cardinality guard (single active nonterminal per brain) | done | `crates/spur-mcp/src/server.rs` (`current_brain_active_owned_plan`) |
| Active slot open guard in TUI navigation | done | `crates/spur-tui/src/views/plan_browser.rs`, `crates/spur-tui/src/app.rs` |
| Sprint task issue-detail fetch + rich metadata | done | `crates/spur-tui/src/views/plan_inspector.rs`, `crates/spur-tui/src/components/plan_task_detail.rs` |
| Sprint task detail scroll affordance | done | `crates/spur-tui/src/views/plan_inspector.rs`, `crates/spur-tui/src/components/plan_task_detail.rs` |
| Plan owner label replacement during execute path | done | `crates/spur-mcp/src/server.rs` |

## UAT Scenarios for Acceptance

1. **Create and own one plan from backlog**
   - With no active owned plan: select an epic in `IssueBrowserView` and press `E`.
   - Expectation: execution succeeds, epic gets one owner label for current brain, and `PlanBrowserView` shows it as `mine`.

2. **Block double-active from backlog**
   - With one active nonterminal `mine` plan in progress: attempt `E` on another issue.
   - Expectation: MCP returns `current brain session already owns active plan...`; no new plan labels are applied and UI shows a blocking hint.

3. **Resume blocked when busy**
   - In `PlanBrowserView`, with one active `mine` plan, place cursor on an `unowned` row and press `R`.
   - Expectation: no ownership change; row remains `unowned`; user hint explains resume blocked.

4. **Resume allowed when free**
   - With no active `mine` plan, select an `unowned` row and press `R`.
   - Expectation: MCP adds owner label; row flips to `mine`; `Open` uses current session plan path.

5. **Blocked handoff paths**
   - For `Other` or `Ambiguous` rows, `Enter` and `R` are rejected.
   - Expectation: `PlanBrowserView` remains on the same row and no active session plan is changed.

6. **Open Sprint detail affordances**
   - On `Mine` row, press `Enter` to open `PlanInspectorView`.
   - Select task with issue ID and press `Enter` to open issue detail.
   - Use `j/k`, `PgUp/PgDn`, `g/G`; expect visible scroll clamp and line-range hint updates.

## Expanded View Interaction Matrix

```mermaid
sequenceDiagram
    participant User as User
    participant A as TUI App
    participant I as IssueBrowser
    participant S as PlanBrowser
    participant P as PlanInspector
    participant M as MCP
    participant PM as Beads/PM

    User->>I: E on epic
    I->>A: Action::Issue(IssueAction::ExecuteEpic)
    A->>M: UserInput::ExecuteEpic
    M->>PM: execute_epic(epic_id) + active-plan check
    M-->>A: Plan command success / blocked
    A->>S: RefreshPlans on success
    PM-->>S: PlansLoaded

    User->>S: R on Unowned
    S->>A: Action::ResumePlan
    A->>M: UserInput::ResumePlan
    M->>PM: resume_plan(plan_id) + active-plan check
    M-->>A: claimed / blocked
    A->>S: show updated owner_state or hint

    User->>S: Enter on Mine(active for this session)
    S->>A: NavigateTo(PlanInspector(session))
    A->>P: Render current session projection
    User->>P: Enter on task with issue
    P->>A: Action::Issue(ViewDetail)
    A->>PM: issue detail fetch
    PM-->>P: IssueDetailFetched
```

## Deferred Work

- Active owner handoff handshake.
- Manual ambiguous-owner repair UI.
- Multi-plan dashboard metrics.
- Plan search/filter.
- Plan detail graph visualization.
- Cross-brain live presence indicators.
