# 2026-04-23 Dedicated Plan Inspector / DAG UI Design

Status: Approved
Scope: SPUR TUI UX and state contract
Decision: Keep `SessionDetail` minimal; move DAG inspection into a dedicated full-screen view

## Summary

SPUR needs two different plan surfaces:

1. A dense, low-cost signal in `SessionDetail` that tells the user a plan exists and is progressing.
2. A dedicated read-only plan inspector for the actual DAG, task states, and next-action context.

This spec intentionally rejects a large inline plan panel in `SessionDetail`. That screen is already vertically constrained and should remain optimized for chat, trace, and worker visibility.

The resulting split is:

- `SessionDetail`: one-line `PlanPulse`
- `PlanInspectorView`: full-screen dedicated plan/DAG view
- `Alt+P`: toggle between `SessionDetail` and `PlanInspectorView`
- `Esc`: always closes `PlanInspectorView` back to `SessionDetail`

Important refinement:

- v1 does not render an arbitrary node-edge graph by default
- v1 renders a stage-lane execution board derived from the DAG
- explicit graph-style rendering is deferred to a later overlay or alternate mode

## Design Intent

### User Jobs

1. While chatting with the brain, the user should immediately know whether a plan is active without losing trace space.
2. When the user wants detail, they should be able to jump to a purpose-built plan screen in one keystroke.
3. The detailed screen should answer:
   - What is running now?
   - What is blocked and by what?
   - What is awaiting review?
   - What failed, rejected, or completed?
   - What is the next operator action?
4. The design must respect the current TUI mental model: one active full-screen view at a time, with overlays or view switches instead of persistent side panels.

### Non-Goals

- No plan approvals or rejections inside the inspector in v1
- No editing, mutation, or merge actions from the inspector in v1
- No attempt to reconstruct plan truth from `SessionUpdate::Plan` checklist text
- No reuse of executor lineage as the primary plan model

## Interaction Model

### Core Split

- `SessionDetail` stays focused on conversation and live execution.
- `PlanPulse` is a compact signal, not a mini dashboard.
- `PlanInspectorView` is the place for detailed DAG inspection.

### Opening and Closing

- From `SessionDetail`, `Alt+P` opens `PlanInspectorView`
- From `PlanInspectorView`, `Alt+P` closes back to `SessionDetail`
- From `PlanInspectorView`, `Esc` also closes back to `SessionDetail`

This is a view toggle, not a session mode. `Alt+M` already owns mode semantics; `Alt+P` is navigation.

### Visibility Rules

- If there is no tracked plan for the current session, `PlanPulse` is hidden.
- If there is no tracked plan, `Alt+P` no-ops and emits a short hint such as `No active plan to inspect`.
- If a plan is active, `PlanPulse` appears automatically.
- If a plan is terminal, `PlanPulse` remains visible in compressed terminal form until the session context changes or a newer tracked plan supersedes it.

## Source of Truth and Join Model

This design is beads-first.

For persisted plans:

- beads-backed plan/task state is the canonical source of truth
- executor lineage is a temporary live overlay only

The inspector must not treat `ExecutorNode` as the durable task record.

### Canonical Task Identity

For persisted plans, every displayed task should anchor on the durable plan task identity, which in practice must map to a beads issue id.

This means:

- `PlanProjection` is the primary model for task existence and status
- `ExecutorNode` is joined onto that model by `issue_id`
- executor ids are not the primary identity for plan tasks

### What Comes From Beads / PlanProjection

The following must come from the durable plan projection:

- task existence
- task ordering / dependency membership
- canonical task status
- blocked-by state
- review state
- terminal state
- merge readiness / plan-level next action

### What Comes From Lineage

The following may be layered in from executor lineage when available:

- whether a worker is actively running now
- elapsed live runtime
- latest worker stream / telemetry
- latest diff counters
- transient error display
- agent / executor liveness

### UI Rule

The inspector should render:

- durable task status from `PlanProjection`
- live activity chips from matching `ExecutorNode`

Example:

```text
[REV] Build PlanProjection   worker: claude-code   live: running 00:43   diff +12/-3
```

If there is no matching executor node, the task must still render correctly from durable state alone.

### Ephemeral Plan Caveat

Ephemeral plans do not satisfy strict beads-first source-of-truth semantics.

For v1:

- persisted / beads-backed plans are the primary supported inspector target
- if ephemeral plans are shown at all, they must be clearly marked as session-local and non-durable
- no inspector design decision should depend on executor lineage pretending to be the durable task model

## SessionDetail `PlanPulse` Spec

### Placement

`PlanPulse` should not consume a new full-width content panel.

Preferred placement:

- inline on the header row, right-aligned when space allows

Fallback placement:

- a single compact row directly below the header only when width is too narrow to fit the pulse inline

Rejected placement:

- a persistent multi-row inline plan panel between trace and input

### Purpose

`PlanPulse` only answers:

- Is a plan active?
- What high-level state is it in?
- How far along is it?
- What should the user do next?
- How do I inspect it?

### Content

Canonical fields, highest to lowest priority:

1. `plan_id`
2. overall state
3. compact counts
4. next action
5. `Alt+P` affordance

Preferred full format:

```text
plan p-123 running | 3/7 done | rv1 fl0 | next: review | Alt+P
```

Compressed formats:

```text
plan p-123 running | 3/7 | review | Alt+P
plan p-123 running | 3/7 | Alt+P
plan running | Alt+P
```

### State Labels

Use short, operator-facing labels:

- `running`
- `awaiting_review`
- `ready_to_merge`
- `has_failures`
- `has_rejections`
- `approved`
- `failed`

Avoid verbose prose in the pulse itself.

### Color / Tone

- `running`: cyan
- `awaiting_review`: yellow bold
- `ready_to_merge`: green bold
- `has_failures` / `failed`: red bold
- `has_rejections`: red
- `approved`: green

### Copy Rules

- Prefer machine-like clarity over conversational copy.
- Use counts instead of sentences.
- Preserve the operator verb in `next:`:
  - `review`
  - `merge`
  - `inspect failure`
  - `inspect rejection`

### Display Label Mapping

The durable task status remains canonical in the data model, but the UI may map it to shorter operator-friendly display labels.

- `pending` -> `queued`
- `ready` -> `ready`
- `awaiting_review` -> `review`
- `approved` -> `passed`
- `failed` -> `failed`
- `cancelled` -> `skipped`
- `rejected` -> `rejected`

The UI should prefer concise CI-style wording over internal jargon where clarity improves.

### Glyph Policy

The UI should be ASCII-safe by default.

- status tags must render correctly in plain monospaced ASCII terminals
- any future Unicode polish must preserve a stable ASCII fallback
- wireframes in this spec intentionally use ASCII-first labels and borders

## Dedicated `PlanInspectorView` Spec

### Position in the UI Model

`PlanInspectorView` is a new full-screen top-level view:

- `ViewId::PlanInspector(SessionId)`

It follows the existing full-screen view pattern rather than introducing a true split-screen sidecar architecture.

### View Layout

The inspector uses a stable three-part layout:

1. Header
2. Main body
3. Footer / hint bar

Main body split:

- left: stage-lane execution board, about 60%
- right: selected task detail, about 40%

### Industry Pattern Baseline

The default inspector should feel closer to a CI pipeline or job-run board than a free-form graph canvas.

Primary analogs:

- GitHub Actions job view
- GitLab pipeline graph
- Buildkite pipeline board
- workflow run inspectors with a selected job detail pane

Why:

- users already understand lane-by-stage layouts
- terminal UIs handle aligned boards and lists far better than arbitrary edge routing
- read-only operator workflows map well to pipeline-inspector patterns

### Default Rendering Strategy

`PlanInspectorView` should default to a stage-lane board, not a literal DAG drawing.

Definition:

- a lane is a derived execution stage
- by default, stages map to topological depth or another deterministic layering rule
- dependency edges are implicit in lane order and task detail, not continuously drawn on screen

This keeps the inspector friendly, stable under resize, and realistic to implement in `ratatui`.

### Left Pane: Stage-Lane Execution Board

The left pane is dependency-first.

Use lane columns by execution stage or dependency depth, not free-form drawn edges. This keeps the UI readable in a terminal and avoids brittle ASCII wiring.

Each column represents a derived stage:

- `Stage 0`
- `Stage 1`
- `Stage 2`
- etc.

Each task is a compact card:

- status tag
- task name
- agent
- attempt summary if relevant

Example card content:

```text
[RUN] Build PlanProjection
agent: claude-code
try 1/3
```

Status tags:

- `[PND]`
- `[RDY]`
- `[RUN]`
- `[REV]`
- `[OK ]`
- `[REJ]`
- `[ERR]`
- `[CAN]`

The selected task is highlighted. Navigation is by task and lane, not by drawn edge.

#### Why Not a Literal Graph

The terminal is hostile to:

- diagonal and curved edge routing
- dense edge crossings
- arbitrary node movement on resize
- readable graph layouts below wide-terminal sizes

So the default board should emphasize:

- stable columns
- stable ordering
- clear status
- fast keyboard traversal
- detail-on-selection

### Right Pane: Task Detail

The right pane expands the selected task.

Fields:

- task id
- task name
- current status
- agent
- attempt `n/max`
- depends_on
- unblocks
- blocked_by
- summary
- feedback or error
- worker branch
- diff summary
- next operator action for this task

This pane is authoritative for detail. The left pane stays compact.

Recommended tabs are deferred from v1, but the pane should be designed so that future tabbing remains possible without layout replacement.

### Header

Header fields:

- plan id
- overall status
- progress string
- counts summary
- merge readiness indicator when applicable

Example:

```text
Plan p-123  status: awaiting_review  progress: 4/7 reviewed  running:1  review:1  failed:0
```

### Footer

Footer hints:

- `Alt+P close`
- `Esc back`
- `h/l lanes`
- `j/k tasks`
- `g/G top/bottom`
- `? help`

Optional future hints are excluded from v1.

### Responsive Behavior

The inspector must degrade gracefully on narrow terminals.

- At 100+ columns: full two-pane stage board + detail pane
- At 80-99 columns: narrower lanes, abbreviated detail labels
- Below 80 columns: collapse into a single vertical task list grouped by stage headers
- Below 40 columns: show only status glyphs, task names, and a minimal summary line

The responsive fallback is a grouped list, not a squeezed pseudo-graph.

Implementation rule:

- at widths below 90 columns, the stage-lane board must degrade to the stacked grouped-list layout

## Key States and Transitions

### State Model

Per session, the TUI tracks one current plan context for v1:

- the most recent active plan for that session
- if none are active, the most recent terminal tracked plan may remain visible in compressed form

### Transitions

1. No tracked plan
   - `PlanPulse` hidden
   - `Alt+P` no-op with status hint
2. Plan seeded
   - `PlanPulse` appears
   - `Alt+P` opens inspector
3. Plan running
   - pulse shows `running`
   - inspector shows live statuses
4. Plan awaiting review
   - pulse shows `awaiting_review`
   - inspector highlights reviewable tasks
5. Plan terminal
   - pulse compresses to terminal summary
   - inspector remains available for review of final state
6. Newer plan supersedes current tracked plan
   - pulse switches to the newer plan
   - inspector opens the currently tracked plan

## Keybindings and Navigation

### SessionDetail

- `Alt+P`: open inspector if tracked plan exists
- `Alt+P`: no-op with brief hint if no tracked plan exists

### PlanInspectorView

- `Alt+P`: close and return to `SessionDetail`
- `Esc`: close and return to `SessionDetail`
- `j/k` or `Up/Down`: move selected task within the current lane
- `h/l` or `Left/Right`: move across dependency-depth lanes
- `g/G`: jump to first/last visible task in the active lane
- `?`: open help overlay

Selection model:

- the selected task is highlighted with reverse video or equivalent strong focus treatment
- in ASCII examples, selection is represented by a leading `>`
- in stacked responsive mode, `j/k` moves through the flattened visible task order

### Navigation Semantics

- `PlanInspectorView` always returns to the originating `SessionDetail`
- it should not return to `Dashboard` directly
- it should not trap the user in a separate stack with ambiguous back behavior
- if a future nested overlay is opened from the inspector, first `Esc` closes the nested overlay, second `Esc` closes the inspector

## ASCII Wireframes

### 1. SessionDetail with `PlanPulse`

```text
+------------------------------------------------------------------------------+
| Dashboard > claude-code (brain)      04:12      $1.28                        |
| Plan p-123  running  3/7  review:1  fail:0  next: review             Alt+P   |
+------------------------------------------------------------------------------+
|                                                                              |
|  React trace                                                                 |
|  - agent thought                                                             |
|  - tool call                                                                 |
|  - worker completion                                                         |
|                                                                              |
|  ...                                                                         |
|                                                                              |
+------------------------------------------------------------------------------+
| Workers (2)                                                                  |
|  codex    running      00:43    +12/-3                                       |
|  kimi     review       01:10    +0/-0                                        |
+------------------------------------------------------------------------------+
| > message input                                                              |
+------------------------------------------------------------------------------+
| [Enter] send  [Esc] back  [Alt-M] mode  [Alt-D] workers  [Alt-P] inspect    |
+------------------------------------------------------------------------------+
```

### 2. `PlanInspectorView` Main View

```text
+------------------------------------------------------------------------------+
| Plan Inspector   p-123   review   done 4/7   run 1   rev 1   fail 0   Alt+P |
| Stages: 3        Selected: t-review-projection                         Esc    |
+--------------------------------------+---------------------------------------+
| Stage 0                              | Task detail                           |
|--------------------------------------|---------------------------------------|
|   [PAS] Seed plan event contract     | Build PlanProjection                  |
|   [PAS] Add PlanProjection model     | id: t-review-projection               |
|                                      | state: review        try: 1/3         |
| Stage 1                              | agent: claude-code                    |
|--------------------------------------| depends on: t-seed-contract           |
| > [REV] Build PlanProjection         | unblocks: t-plan-inspector-view       |
|   [RUN] Wire App/ViewContext  00:43  | live: running 00:43   diff: +184/-22  |
|                                      | summary: projects plan status into UI |
| Stage 2                              | next: review task output              |
|--------------------------------------| worker: spur/worker-...               |
|   [QUE] SessionDetail PlanPulse      |                                       |
|   [QUE] PlanInspectorView            |                                       |
+--------------------------------------+---------------------------------------+
| [H/L] lane  [J/K] task  [G] end  [Alt-P] close  [Esc] back  [?] help        |
+------------------------------------------------------------------------------+
```

### 3. `PlanInspectorView` Narrow Responsive Fallback

Used when the terminal is too narrow for a reliable two-pane lane board.

```text
+--------------------------------------------------------------------+
| Plan Inspector   p-123   review   done 4/7   run 1   rev 1   Alt+P|
+--------------------------------------------------------------------+
| Stage 0                                                            |
|   [PAS] Seed plan event contract                                   |
|   [PAS] Add PlanProjection model                                   |
|                                                                    |
| Stage 1                                                            |
| > [REV] Build PlanProjection                try 1/3                |
|   [RUN] Wire App/ViewContext                00:43                  |
|                                                                    |
| Stage 2                                                            |
|   [QUE] SessionDetail PlanPulse                                   |
|   [QUE] PlanInspectorView                                         |
+--------------------------------------------------------------------+
| Selected: Build PlanProjection                                    |
| agent: claude-code   depends on: t-seed-contract                  |
| next: review task output                                          |
+--------------------------------------------------------------------+
| [J/K] task  [G] end  [Alt-P] close  [Esc] back                    |
+--------------------------------------------------------------------+
```

### 4. Empty / No-Plan State

This state is primarily handled by gating `Alt+P`, but the inspector may still need a graceful empty render if opened during a race.

```text
+------------------------------------------------------------------------------+
| Plan Inspector                                                              |
+------------------------------------------------------------------------------+
|                                                                              |
|  No tracked plan for this session.                                           |
|                                                                              |
|  Submit a plan, wait for the plan line to appear, then press Alt+P.          |
|                                                                              |
+------------------------------------------------------------------------------+
| [Alt-P] close  [Esc] back                                                    |
+------------------------------------------------------------------------------+
```

### 5. Terminal / Ready-to-Merge State

```text
+------------------------------------------------------------------------------+
| Dashboard > claude-code (brain)      09:41      $3.02                        |
| Plan p-123  merge-ready  passed 7/7  review:0  fail:0                 Alt+P  |
+------------------------------------------------------------------------------+
|  React trace                                                                 |
|  ...                                                                         |
+------------------------------------------------------------------------------+
| [Enter] send  [Esc] back  [Alt-M] mode  [Alt-D] workers  [Alt-P] inspect    |
+------------------------------------------------------------------------------+
```

And inside the inspector:

```text
+------------------------------------------------------------------------------+
| Plan Inspector   p-123   merge-ready   passed 7/7   run 0   rev 0   Alt+P   |
| Total time: 8m 08s   last finished: 07:39:10                          Esc    |
+--------------------------------------+---------------------------------------+
| Stage 0                              | Summary                               |
|--------------------------------------|---------------------------------------|
|   [PAS] Seed plan event contract     | All tasks completed successfully.     |
|   [PAS] Add PlanProjection model     | next: merge_plan                      |
|                                      | merge state: not started              |
| Stage 1                              | review: 0   failed: 0   skipped: 0    |
|--------------------------------------|                                       |
| > [PAS] Build PlanProjection         |                                       |
|   [PAS] Wire App/ViewContext         |                                       |
|                                      |                                       |
| Stage 2                              |                                       |
|--------------------------------------|                                       |
|   [PAS] SessionDetail PlanPulse      |                                       |
|   [PAS] PlanInspectorView            |                                       |
+--------------------------------------+---------------------------------------+
| [H/L] lane  [J/K] task  [Alt-P] close  [Esc] back  [?] help                |
+------------------------------------------------------------------------------+
```

## Risks

### 1. Missing Seed / Snapshot Contract

Current ACP plan lifecycle events are deltas. They do not carry enough data to seed a full DAG projection by themselves.

Without a seed or snapshot, the inspector cannot correctly know:

- the initial task list
- dependency edges
- blocked-by relationships
- the canonical `plan_id`

### 2. Trace Duplication

If `SessionUpdate::Plan` continues rendering as checklist text in the trace while `PlanPulse` and the inspector exist, plan status will be duplicated in two incompatible formats.

### 3. Multiple Plans per Session

If a brain session submits multiple plans over time, the TUI needs a deterministic rule for which plan is tracked as current in `SessionDetail`.

### 4. Overloaded Scope

Adding review controls inside the inspector would require task-to-executor correlation and a more complex mutation model. That is explicitly out of scope for v1.

### 5. Literal Graph Rendering as a Premature Trap

For `ratatui`, a true node-edge graph is expensive and brittle:

- edge crossings are hard to read
- resizing invalidates placement
- keyboard navigation becomes less obvious
- wide graphs collapse badly on narrow terminals

That is a dead end for v1 and should not be the default experience.

## Open Questions

1. Should terminal `PlanPulse` remain visible forever, or only until the next user turn?
2. In v1, should the inspector support browsing historical plans for the same session, or only the current tracked plan?
3. Should `PlanCompleted` terminal states with failures auto-focus the first failed task when the inspector opens?
4. Should the left pane show strict dependency-depth lanes only, or also a filtered list mode for `awaiting_review` / `failed` tasks in a future version?

## Required Contract / State Work Before Implementation

### PlanProjection Data Contract

The design needs a concrete projection shape, not just the name `PlanProjection`.

Illustrative model:

```rust
pub struct PlanProjection {
    pub plan_id: String,
    pub tasks: Vec<PlanTaskNode>,
    pub stages: Vec<String>,
}

pub struct PlanTaskNode {
    pub task_id: String,
    pub stage_idx: usize,
    pub status: PlanTaskStatus,
    pub executor_id: Option<ExecutorId>,
    pub agent: String,
    pub title: String,
    pub deps: Vec<String>,
}
```

This is intentionally illustrative rather than normative Rust API, but the spec requires equivalent fields.

### Required

1. Add a plan seed or snapshot event
   - example: `PlanSubmitted` or `PlanSnapshot`
   - must include `session_id`, `plan_id`, tasks, and dependency edges
   - must include stable task identifiers suitable for projection and selection
2. Add an app-level `PlanProjection`
   - sibling to executor lineage
   - updated from ACP plan events
   - passed through `ViewContext`
3. Define tracked-plan selection rules
   - which plan `SessionDetail` treats as current for the session
4. Define trace policy
   - either demote or remove checklist rendering from `SessionUpdate::Plan`

### Deferred

- task review actions in the inspector
- multi-plan browsing
- merge or PR actions from the inspector
- free-form graph edge drawing
- animated graph layout or force-directed positioning

## Component Reuse Inventory

The design should reuse existing TUI patterns rather than inventing a separate visual language.

- `AgentsTree` can inform task selection and collapse behavior
- `DetailPane` can inform the selected-task detail surface
- `WorkersPanel` styling can inform compact task card chrome
- `MermaidViewerView` is the natural place to borrow overlay mechanics for any future graph-style visualization mode

This is a reuse direction, not a requirement to share implementation directly.

## V1 / V2 Boundary

### V1

- `PlanPulse` in `SessionDetail`
- stage-lane execution board
- selected task detail pane
- grouped-list responsive fallback
- keyboard navigation only
- read-only inspector

### V2

- optional alternate graph overlay or Mermaid-derived visualization
- optional tree/list toggle modes
- optional richer task tabs
- optional mouse support
- optional animated status polish

## Out of Scope

- drag-and-drop task rearrangement
- in-place task editing
- force-directed or physics-based graph layout
- mandatory new external graph-layout crate dependencies
- making the plan inspector the place where review decisions are submitted

## Recommended Next Step

Write the implementation plan only after the plan event contract is frozen. The UI design is stable enough to plan, but the projection input contract is still a blocker for correct implementation.
