# SPUR TUI: Dashboard Separation & View Architecture Redesign

**Status:** Proposal
**Scope:** `spur-tui` crate
**Date:** 2026-04-22
**Stakeholders:** TUI maintainers, UX reviewers

---

## 1. Executive Summary

The current `DashboardView` is a monolithic composing view that crams **Agent Monitor**, **Issue Browser**, **Activity Log**, **Agent Detail Pane**, and **Issue Detail Pane** into a single screen with a single key-handler. This creates:

1. **Keybinding namespace collisions** — `j`/`k`/`g`/`G`/`w`/`o`/`b`/`d` mean different things depending on invisible state (which panel is "focused", whether `issue_focus` is `Loaded`, whether the input bar is in Vim Normal or Emacs mode).
2. **Cognitive overload** — Users must track sub-mode state to predict what a key will do.
3. **Maintenance fragility** — The `handle_view_key` function is ~500 lines of nested conditionals. The recent modal revamp introduced a bug where `w` (and `o`/`b`/`d`/`W`) only work in Vim Normal mode, not Emacs mode, because the arms were duplicated in the wrong branch.

**Proposal:** Extract the issue browser into a standalone `IssueBrowserView`, slim the Dashboard into an `AgentMonitorView`, and establish a clean view-navigation model across the four primary surfaces: **Agent Monitor**, **Issue Browser**, **Plan Inspector**, and **Session Detail**.

---

## 2. Immediate Bug: The `w` Keybinding (Root Cause & Fix)

### 2.1 Symptom
In the Dashboard, pressing `w` to mark an issue as "in_progress" does nothing when the `InputBar` is in `EditMode::Emacs` (the default).

### 2.2 Root Cause Analysis

Tracing the key path for `w` when `issue_focus == IssueFocus::Loaded { .. }`:

1. `handle_key_inner` → `key_owner(key)` is called.
2. `DashboardMode::Navigate` branch executes.
3. `key.code == KeyCode::Char('w')` and `!self.input_bar.is_vim_normal()` → falls through to the Emacs branch (line 752).
4. `is_view_action_char('w')` returns `true` because `issue_focus != None` and `w` is in the `matches!(ch, 'j' | 'k' | 'o' | 'w' | 'b' | 'd' | 'W')` list.
5. `key_owner` returns `KeyOwner::View`.
6. `handle_view_key` is called.
7. `KeyCode::Char(ch)` matches.
8. `if self.input_bar.is_vim_normal()` is `false` → the entire `o`/`w`/`b`/`d`/`W` block is **skipped**.
9. The insert/emacs `match ch` at line 1144 has **no arm for `w`**.
10. Falls to `_ => None`. Key swallowed.

### 2.3 Fix (Minimal — Pre-Separation)

Duplicate the issue-status arms from the Vim Normal block into the Emacs/Insert block:

```rust
// Inside the insert/emacs match ch block (line ~1144)
'o' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
    if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
        return Some(Action::Issue(
            crate::action::IssueAction::UpdateStatus {
                id: id.clone(),
                status: "open".into(),
            },
        ));
    }
    None
}
'w' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
    // ... same pattern, "in_progress"
}
'b' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
    // ... same pattern, "blocked"
}
'd' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
    // ... same pattern, "closed"
}
'W' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
    // ... same pattern, work-on issue
}
```

**This duplication is exactly why separation is the right long-term fix.** The same logic is being copy-pasted across two mode branches because the key-space is overcrowded.

---

## 3. MCTS Round 1: First Principles Decomposition

### 3.1 What Is a "View" in SPUR?

From the codebase, a view is a tuple of four primitives:

| Primitive | Trait Method | Purpose |
|-----------|--------------|---------|
| **Render target** | `render(&mut self, frame, area, ctx)` | Draws to a `Rect` |
| **Key surface** | `handle_key(&mut self, key, ctx) -> Option<Action>` | Consumes input, emits actions |
| **Event sink** | `handle_spur_event(&mut self, event, ctx)` | Reacts to orchestrator events |
| **State bag** | `tick(&mut self)` | Holds UI state, animates |

The `App` is a **router**: it holds optional view instances and dispatches `render`/`handle_key` to `current_view`. `NavigateTo(ViewId)` and `NavigateBack` are the routing actions.

### 3.2 First Principles: Why Does the Dashboard Feel Wrong?

**Principle 1: A key should have one meaning per view.**

In the Dashboard, `j` means:
- "Select next agent" (if `focused_panel == Agents && focused_node.is_none()`)
- "Select next issue" (if `focused_panel == Issues && focused_node.is_none()`)
- "Scroll detail pane" (if `focused_node.is_some()`)
- "Scroll activity log" (otherwise)
- "Scroll issue detail" (if `issue_focus` is `Loaded` — and this even intercepts `Up`/`Down`!)

The user must hold 5+ invisible predicates in working memory. This violates the **principle of least surprise**.

**Principle 2: Views should own their state; components should be reusable.**

`DashboardView` currently owns:
- `agents_tree: AgentsTree`
- `issues_panel: IssuesPanel`
- `issue_detail_pane: IssueDetailPane`
- `detail_pane: DetailPane`
- `activity_log: ActivityLog`
- `input_bar: InputBar`

If we want an Issue Browser view, we can't reuse the Dashboard's issue logic — it's entangled with `focused_panel`, `focused_node`, and agent-tree state.

**Principle 3: Modal systems should be shallow, not deep.**

Current modal stack:
```
Dashboard
├── DashboardMode { Navigate, Compose }
├── focused_panel { Agents, Issues, Log }
├── focused_node: Option<ExecutorId>
├── detail_pane.tab { Stream, Artifacts, Attempts, Task, Review }
└── issue_focus { None, Loading, Loaded }
```

That's 5 layers of modality. The recent `DashboardMode` revamp helped, but the other 4 layers remain. Each layer increases the chance of bugs like the `w` key issue.

### 3.3 Problem Statement (Formal)

> The DashboardView violates the Single Responsibility Principle by conflating four distinct user workflows (monitor agents, browse issues, inspect agent details, read activity log) into one view with one key-handler. This causes keybinding namespace exhaustion, state entanglement, and maintenance fragility.

---

## 4. MCTS Round 2: Strategy Evaluation

We evaluate four candidate architectures against three criteria:
- **K1: Keybinding clarity** — Can a user learn the keys in <2 minutes?
- **K2: Information visibility** — Can the user see everything they need without excessive view-switching?
- **K3: Implementation cost** — How much code churn? Does it reuse existing patterns?

### 4.1 Option A: Full Separation (The "Four Views" Model)

Extract everything into dedicated views:
- `AgentMonitorView` — AgentsTree + ActivityLog + DetailPane + InputBar
- `IssueBrowserView` — IssuesPanel + IssueDetailPane + filters + InputBar
- `PlanInspectorView` — Already exists ✅
- `SessionDetailView` — Already exists ✅

| Criterion | Score | Rationale |
|-----------|-------|-----------|
| K1 | ⭐⭐⭐⭐⭐ | Each view has a clean key namespace. `j`/`k` always mean the same thing within a view. |
| K2 | ⭐⭐⭐ | Lose "at a glance" issue visibility while monitoring agents. User must switch views to see issues. |
| K3 | ⭐⭐⭐⭐ | Follows existing `PlanInspectorView` pattern. Requires new `IssueBrowserView`, refactoring `DashboardView`. Moderate churn. |

### 4.2 Option B: Tabbed Dashboard

Keep one `DashboardView` but add explicit tabs: `[Agents] [Issues] [Log]`.

| Criterion | Score | Rationale |
|-----------|-------|-----------|
| K1 | ⭐⭐⭐ | Better than current, but `j`/`k` still change meaning per tab. Need tab-switch keys too. |
| K2 | ⭐⭐⭐⭐ | Single view, but you still can't see agents and issues simultaneously. |
| K3 | ⭐⭐⭐⭐⭐ | Lowest churn — just add tab state to existing Dashboard. |

**Verdict:** Rejects K1. Still conflates key namespaces across tabs.

### 4.3 Option C: Enhanced Focus Model (tmux-style)

Keep the monolithic view but make panel focus extremely explicit: colored borders, `[FOCUSED]` labels, and a dedicated "focus cycle" key (`Tab` or `Ctrl+w hjkl`).

| Criterion | Score | Rationale |
|-----------|-------|-----------|
| K1 | ⭐⭐ | Users must learn a meta-layer of focus commands. The `w` bug would still be possible because `issue_focus` adds a 6th layer. |
| K2 | ⭐⭐⭐⭐⭐ | Everything visible at once. |
| K3 | ⭐⭐ | Makes the 500-line `handle_view_key` even larger. |

**Verdict:** Rejects K1 and K3. This is the path that created the current bug.

### 4.4 Option D: Hybrid Separation (Recommended)

Full separation **plus** a compact "Issue Ticker" in the Agent Monitor:
- `AgentMonitorView` — AgentsTree + ActivityLog + DetailPane + **Issue Ticker strip** (count + latest 3 issues) + InputBar
- `IssueBrowserView` — Full standalone issue browser with detail pane
- `PlanInspectorView` — Existing
- `SessionDetailView` — Existing

| Criterion | Score | Rationale |
|-----------|-------|-----------|
| K1 | ⭐⭐⭐⭐⭐ | Clean namespaces in all views. |
| K2 | ⭐⭐⭐⭐⭐ | Agent Monitor keeps glanceable issue awareness. Full browser available on demand. |
| K3 | ⭐⭐⭐⭐ | Reuses `IssuesPanel` / `IssueDetailPane` components. New `IssueBrowserView` follows `PlanInspectorView` pattern. Slightly more churn than Option A. |

### 4.5 Round 2 Winner: Option D

Option D wins on K1 and K2 with only a marginal K3 cost. It is the **Pareto optimal** choice.

---

## 5. MCTS Round 3: Consolidated Design

### 5.1 View Inventory

| ViewId | Purpose | Keybindings |
|--------|---------|-------------|
| `AgentMonitor` | Watch agents, read activity, inspect details | `j`/`k` tree nav, `h`/`l` detail tabs, `Enter` focus agent, `c` collapse, `r` jump review, `z` zoom, `]` next view |
| `IssueBrowser` | Browse, filter, triage issues | `j`/`k` list nav, `Enter` open detail, `o`/`w`/`b`/`d` status, `f` filter, `/` search, `]` next view |
| `PlanInspector` | Inspect plan stages & tasks | `h`/`l` stage, `j`/`k` task, `Enter` detail, `Esc` back |
| `SessionDetail` | Chat with brain session | `j`/`k` scroll chat, `i` compose, `Esc` back/stop |

### 5.2 Global Navigation Model

Replace the ad-hoc `NavigateBack` heuristic with explicit **view ring navigation**:

```
AgentMonitor <-> IssueBrowser <-> [SessionDetail] <-> [PlanInspector]
     ^                                                            |
     └────────────────────────────────────────────────────────────┘
```

- `]` or `Ctrl+n` — next view in ring
- `[` or `Ctrl+p` — previous view in ring
- `1`/`2`/`3`/`4` — direct jump (context-sensitive: `3` only appears if session active)
- `Esc` from any non-Dashboard view → `AgentMonitor` (the "home" view)

`NavigateBack` is deprecated for user-facing navigation; kept for `Esc` from overlays (Help, QuitConfirm).

### 5.3 Component Reuse Map

```
components/
├── agents_tree.rs        → AgentMonitor
├── activity_log.rs       → AgentMonitor
├── detail_pane.rs        → AgentMonitor
├── input_bar.rs          → ALL views (universal)
├── issues_panel.rs       → AgentMonitor (ticker) + IssueBrowser (full)
├── issue_detail_pane.rs  → IssueBrowser
├── status_bar.rs         → ALL views
├── help_overlay.rs       → ALL views
└── plan_stage_board.rs   → PlanInspector
```

`IssuesPanel` is already a reusable component. We can instantiate it in two places with different `render` area constraints.

---

## 6. ASCII Wireframes

### 6.1 Agent Monitor (`AgentMonitorView`)

```
┌─ Lineage: 5 agents · 2 running · z zoom ──────────────────────────────┐
│  ▼ brain-1          [ running ]  plan:feature-x                        │
│    ├─ ▶ codex-1     [ working ]  react loop                           │
│    │   └─ ○ review-1[ pending ]                                       │
│    └─ ▶ gemini-2    [ idle ]                                          │
│  ▶  brain-2         [ idle ]                                           │
├─ Issues: 3 tracked · latest: #42 "Auth refactor" [blocked] ────────────┤
│  (compact ticker — 1 line, scrollable with `<`/`>`)                   │
├─ Activity Log ─────────────────────────────────────────────────────────┤
│  [14:32] brain-1  > Spawned codex-1 for task-7                        │
│  [14:33] codex-1  > React loop started                                │
│  [14:35] codex-1  > Attempt 1 complete — awaiting review              │
│  ...                                                                   │
├─ Input ────────────────────────────────────────────────────────────────┤
│  > _                                                                  │
├─ [1]Agent  [2]Issues  [3]Chat  [4]Plan  │ 2 running  │ $0.042 │ 5m 12s ┤
└────────────────────────────────────────────────────────────────────────┘
```

**Keybindings:**
- `j`/`k` — navigate agent tree
- `Enter` — focus selected agent (opens DetailPane to the right, replacing Activity Log)
- `h`/`l` — cycle DetailPane tabs (Stream | Artifacts | Attempts | Task | Review)
- `c` — collapse/expand selected node
- `r` — jump to Review tab of selected node
- `z` — zoom mode (collapse tree to header, expand DetailPane)
- `]` — go to IssueBrowser
- `q` — quit

### 6.2 Agent Monitor — Focused Agent Detail

```
┌─ Lineage: 5 agents · 2 running · z restore ────────────────────────────┐
│  ▼ brain-1                                                             │
│    ├─ ▶ codex-1     [ working ]                                        │
│    │   └─ ○ review-1[ pending ]                                        │
│    └─ ▶ gemini-2    [ idle ]                                           │
├─ Detail: codex-1  [Stream|Artifacts|Attempts|Task|Review] ─────────────┤
│  > Planning approach...                                                │
│  > Reading file src/auth.rs                                            │
│  > Modified src/auth.rs (+45,-12)                                      │
│  ...                                                                   │
├─ Input ────────────────────────────────────────────────────────────────┤
│  > _                                                                  │
├─ [1]Agent  [2]Issues  [3]Chat  [4]Plan  │ 2 running  │ $0.042 │ 5m 12s ┤
└────────────────────────────────────────────────────────────────────────┘
```

**Keybindings:**
- `Esc` — unfocus agent, return to Activity Log
- `j`/`k` — scroll DetailPane (or tree if zoomed)
- `1`/`2`/`3`/`4`/`5` — jump to DetailTab directly

### 6.3 Issue Browser (`IssueBrowserView`)

```
┌─ Issue Browser ── 12 issues ── filter: status:open ────────────────────┐
│  ID      P  Type      Status     Assignee    Title                     │
│> #42     P1 bug       open       codex-1     Auth refactor crashes     │
│  #43     P2 feature   in_prog    gemini-2    Add OAuth2 flow           │
│  #44     P0 bug       blocked    —           Memory leak in worker     │
│  #45     P3 chore     closed     codex-1     Update dependencies       │
├─ Issue: #42  Auth refactor crashes on startup ─────────────────────────┤
│  Status: open  ·  Priority: P1  ·  Type: bug  ·  Assignee: codex-1    │
│  Due: 2026-04-25  ·  Blocked by: #44  ·  Labels: backend, auth       │
│  ─────────────────────────────────────────────────────────────────────│
│  When starting the app with `--auth=oauth2`, the refactor branch      │
│  crashes with a null pointer in `src/auth/provider.rs:87`.            │
│  ...                                                                   │
│  ─────────────────────────────────────────────────────────────────────│
│  [o]pen  [w]ork  [b]locked  [d]one  [W]ork-on  [Esc]close            │
├─ Input ────────────────────────────────────────────────────────────────┤
│  > _                                                                  │
├─ [1]Agent  [2]Issues  [3]Chat  [4]Plan  │ filter:status:open  │ 12 total ┤
└────────────────────────────────────────────────────────────────────────┘
```

**Keybindings:**
- `j`/`k` — navigate issue list
- `g`/`G` — first/last issue
- `Enter` — open issue detail (or expand if already open)
- `o`/`w`/`b`/`d` — set status (open/work/blocked/done)
- `W` — "work on" this issue (dispatch to brain)
- `f` — open filter prompt in InputBar
- `/` — search titles/bodies
- `]` — go to AgentMonitor
- `Esc` — close detail pane (if open) or go to AgentMonitor

### 6.4 Plan Inspector (Existing — For Reference)

```
┌─ Plan: feature-x  ACTIVE  3 / 7 done ──────────────────────────────────┐
│  [████████████░░░░░░░░░░░░░░░░░░░░░░░░░░]                             │
├─ Stage 0        Stage 1        Stage 2        Stage 3 ─────────────────┤
│  ▶ task-0      task-3         task-5        task-6                    │
│    task-1      ▶ task-4       task-7                                  │
│    task-2                                                             │
├─ Task: task-4  "Implement OAuth2 callback" ────────────────────────────┤
│  Status: dispatched  ·  Worker: codex-1  ·  Issue: #43               │
│  ...                                                                   │
├─ [1]Agent  [2]Issues  [3]Chat  [4]Plan  │ feature-x  │ 3/7 done      ┤
└────────────────────────────────────────────────────────────────────────┘
```

### 6.5 Session Detail (Existing — For Reference)

```
┌─ Session: feature-x  (codex) ──────────────────────────────────────────┐
│  User: Implement OAuth2 callback                                       │
│  ─────────────────────────────────────────────────────────────────────│
│  > I'll help you implement the OAuth2 callback. Let me start by       │
│  > reading the current auth code...                                   │
│  > [Reading src/auth.rs]                                              │
│  > ...                                                                │
├─ Input ────────────────────────────────────────────────────────────────┤
│  > _                                                                  │
├─ [1]Agent  [2]Issues  [3]Chat  [4]Plan  │ streaming  │ $0.042 │ 5m 12s ┤
└────────────────────────────────────────────────────────────────────────┘
```

---

## 7. User Journey

### 7.1 Journey A: Monitor → Triage → Dispatch

**Goal:** See that an agent finished, review its work, open a related issue, mark it in-progress, and dispatch the next task.

```
[1] User opens SPUR. Lands on AgentMonitor.
      └─ Sees codex-1 completed attempt 1, awaiting review.

[2] Presses `j` to select codex-1, `Enter` to focus.
      └─ DetailPane opens on Review tab.

[3] Reads diff in Review tab. Presses `a` to approve.
      └─ Agent continues to next task.

[4] Presses `]` to switch to IssueBrowser.
      └─ Sees issue #42 related to the auth work.

[5] Presses `j` to select #42, `Enter` to open detail.
      └─ Reads description, confirms it's the right issue.

[6] Presses `w` to mark "in_progress".
      └─ Issue status updates, reflected in ticker when returning.

[7] Presses `W` to "work on" this issue.
      └─ Dispatches to brain with issue context.

[8] Presses `]` to cycle back to AgentMonitor.
      └─ Issue ticker now shows #42 as in_progress.
```

**Key insight:** Each step uses a **different view**, but the same familiar `j`/`k`/Enter keys. The user never has to remember "am I in the agent tree or the issue list?" — the view tells them.

### 7.2 Journey B: Plan Inspection → Deep Dive

**Goal:** Check plan progress, see a stuck task, open its session to chat.

```
[1] User on AgentMonitor. Presses `4` to jump to PlanInspector.
      └─ Sees feature-x plan, Stage 1, task-4 is stuck.

[2] Presses `l` to move to Stage 2, `j`/`k` to inspect tasks.
      └─ task-5 looks blocked.

[3] Presses `3` to jump to SessionDetail.
      └─ Chat view with the brain.

[4] Types "Why is task-5 blocked?" and submits.
      └─ Brain explains dependency on task-4.

[5] Presses `Esc` to return to AgentMonitor.
```

### 7.3 Journey C: At-a-Glance Monitoring

**Goal:** Keep SPUR open in a tmux pane and watch for alerts.

```
[1] User stays on AgentMonitor.

[2] Issue ticker strip at top shows:
      "Issues: 1 blocked · #44 Memory leak"

[3] User notices without switching views. Presses `2` to investigate.
      └─ IssueBrowser opens, #44 already selected.

[4] Presses `b` to confirm blocked status (already blocked).
      └─ No-op, but user verified.

[5] Presses `1` to return to monitoring.
```

---

## 8. Implementation Roadmap

### Phase 0: Hotfix (Immediate)
- [ ] Fix `w`/`o`/`b`/`d`/`W` in Emacs mode by duplicating arms into insert block.
- [ ] Add test in `dashboard_composer_contract.rs` for issue status keys in both Vim Normal and Emacs.

### Phase 1: Extract IssueBrowserView
- [ ] Create `views/issue_browser.rs` with `IssueBrowserView` struct.
- [ ] Move `tracked_issues`, `issues_panel`, `issue_focus`, `issue_detail_pane` from Dashboard.
- [ ] Add `ViewId::IssueBrowser` to `action.rs`.
- [ ] Implement `View` trait for `IssueBrowserView`.
- [ ] Wire `App` to hold `issue_browser: Option<IssueBrowserView>`.
- [ ] Route `Action::NavigateTo(ViewId::IssueBrowser)` in `App::process_action`.
- [ ] Update `help_overlay.rs` with IssueBrowser keybindings.

### Phase 2: Slim Dashboard → AgentMonitorView
- [ ] Rename `DashboardView` → `AgentMonitorView` (or keep alias).
- [ ] Remove `IssuesPanel`, `IssueDetailPane`, `issue_focus`, `tracked_issues` state.
- [ ] Add compact `IssuesTicker` component (1-line strip showing count + latest 3).
- [ ] Simplify `handle_view_key`: remove all `issue_focus` branches, `focused_panel` can be simplified to `Agents` vs `Log` (or removed entirely).
- [ ] Update `panel_context_hint` and status bar.

### Phase 3: Unified View Navigation
- [ ] Add `]` / `[` global shortcuts for view ring navigation.
- [ ] Add `1`/`2`/`3`/`4` direct jumps in all views.
- [ ] Update `NavigateBack` semantics: `Esc` from non-Monitor views → Monitor.
- [ ] Update `help_overlay.rs` with global navigation section.

### Phase 4: Polish & Test
- [ ] Add `IssueBrowserView` tests (key handling, render smoke).
- [ ] Update `dashboard_composer_contract.rs` to reflect simplified AgentMonitor.
- [ ] Run `cargo test -p spur-tui`, `cargo clippy -p spur-tui -- -D warnings`.
- [ ] Update `docs/onboarding/` screenshots if any.

---

## 9. Keybinding Quick Reference

### Global (All Views)
| Key | Action |
|-----|--------|
| `]` / `Ctrl+n` | Next view |
| `[` / `Ctrl+p` | Previous view |
| `1` | Agent Monitor |
| `2` | Issue Browser |
| `3` | Session Detail (if active) |
| `4` | Plan Inspector (if active) |
| `?` | Show help |
| `q` | Quit |
| `Ctrl+p` / `Ctrl+n` | Input history (when composing) |

### Agent Monitor
| Key | Action |
|-----|--------|
| `j` / `k` | Next/prev agent |
| `Enter` | Focus agent / unfocus |
| `h` / `l` | Prev/next detail tab |
| `g` / `G` | First/last agent |
| `c` | Collapse/expand node |
| `r` | Jump to Review tab |
| `z` | Zoom layout |
| `v` | Toggle verbose |
| `s` | Request sessions |

### Issue Browser
| Key | Action |
|-----|--------|
| `j` / `k` | Next/prev issue |
| `Enter` | Open/close detail |
| `g` / `G` | First/last issue |
| `o` | Status: open |
| `w` | Status: in_progress |
| `b` | Status: blocked |
| `d` | Status: closed |
| `W` | Work on issue |
| `f` | Filter prompt |
| `/` | Search prompt |
| `Esc` | Close detail or back to Monitor |

### Plan Inspector
| Key | Action |
|-----|--------|
| `h` / `l` | Prev/next stage |
| `j` / `k` | Next/prev task |
| `g` / `G` | First/last task in stage |
| `Enter` | Open task detail |
| `Esc` | Back to Monitor |

### Session Detail
| Key | Action |
|-----|--------|
| `j` / `k` | Scroll chat |
| `i` / `a` | Enter compose mode |
| `Esc` | Stop stream / back to Monitor |
| `Ctrl+o` | Toggle observe collapsed |

---

## 10. Appendix: State Simplification Math

| Metric | Current Dashboard | Proposed AgentMonitor | Proposed IssueBrowser |
|--------|-------------------|----------------------|----------------------|
| `focused_panel` variants | 3 (Agents, Issues, Log) | 2 (Agents, Log) or 0 | 1 (Issues) |
| `focused_node` usage | Agent detail + issue intercept | Agent detail only | None |
| `issue_focus` layers | 3 (None, Loading, Loaded) | 0 | 2 (None, Loaded) |
| `handle_view_key` branches | ~45 match arms | ~25 match arms | ~20 match arms |
| Modal depth | 5 | 2 | 2 |

**Result:** ~50% reduction in modal depth and key-handler branching. The `w` bug class becomes impossible because each view has exactly one meaning per key.

---

*End of Proposal*
