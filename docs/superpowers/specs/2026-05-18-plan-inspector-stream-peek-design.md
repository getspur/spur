# Plan Inspector — Stream Peek & Dashboard Jump

**Date:** 2026-05-18
**Status:** Design approved, awaiting implementation plan
**Scope:** `crates/spur-tui/src/views/plan_inspector.rs`, `crates/spur-tui/src/components/detail_pane.rs`, view-switch routing

## Problem

In `plan_inspector`, a user can see that task `T-12` is `running` on worker `codex`, but cannot watch what codex is actually doing without leaving the view, returning to Dashboard, locating codex in `AgentsTree`, and switching `DetailPane` to its Stream tab. Too many hops for what should be a one-keystroke observation.

## Goals

1. Let the user peek at the live worker stream for the selected task without leaving `plan_inspector`.
2. Let the user jump to the full Dashboard view of that worker when they want more than a peek.
3. Reuse the existing Stream-tab renderer — no parallel implementation of the streaming pipeline.
4. Do not pollute Dashboard's `DetailPane` scroll/follow state with peek-time interactions.

## Non-Goals

- Showing historical attempts in the popover (defer to Dashboard's Attempts tab).
- Mouse support.
- Editing/sending input from the popover.
- Watching multiple workers simultaneously from `plan_inspector`.

## Design

### View state

Add an explicit mode enum to `PlanInspectorView`:

```rust
enum Mode {
    Browse,
    StreamPeek { agent: String, task_id: String },
}
```

`Mode::Browse` is the existing behavior. `Mode::StreamPeek` activates the overlay and intercepts keystrokes.

### Stream renderer reuse

Extract the Stream-tab rendering logic from `DetailPane` into a reusable function that takes a render target (`Rect`, `Frame`), an event source (the same source `DetailPane` uses), and a **transient `StreamViewState`** carrying `scroll_offset` and `is_following`.

`DetailPane` keeps its own `StreamViewState`. `PlanInspectorView` owns a **separate** `StreamViewState`, allocated when entering `StreamPeek` and dropped when leaving it. The two states never share storage — opening the peek does not move Dashboard's scroll, and vice versa.

### Layout

Centered overlay with `Clear` underneath, minimum width 60 columns. If terminal width < 60, the overlay falls back to full-screen. Height ~60% of terminal, capped so it never covers the input bar.

Title bar shows: `stream: <agent> (<task_id>)` and, when the worker has finished, appends `[completed]`. Help footer inside the overlay shows the active keybinds.

### Keybindings

In `Mode::Browse`:

- `s` on a selected task with an assigned worker → enter `StreamPeek`. If the task has no worker yet, flash a status-line message (~2 s) and stay in Browse.
- `S` (shift-s) → emit a view-switch `Action` that routes to Dashboard, focuses that worker in `AgentsTree`, and calls `DetailPane::jump_to_tab(Stream)`. No-op if no worker.

In `Mode::StreamPeek`:

- `Esc` or `q` → leave peek, return to `Browse`.
- `j` / `↓`, `k` / `↑` → scroll the peek's `StreamViewState` only.
- `g` / `G` → top / bottom of peek stream.
- `f` → toggle follow-mode on the peek's `StreamViewState` only.
- All other keys are swallowed (no leakage to the task list).

### Lifecycle rules

| Event | Behavior |
|---|---|
| Selected task changes (any cause) while peek is open | Auto-close peek, drop its `StreamViewState`. User must press `s` again on the new task. |
| Worker finishes while peek is open | Stream renderer naturally quiesces. Title gains `[completed]`. Auto-follow disables. Popover stays open until user dismisses. |
| Task has multiple attempts | Peek shows the **latest active attempt** only. To inspect history, user presses `S` to jump to Dashboard's Attempts tab. |
| User presses `S` while peek is open | Treat as "open it for real": close peek and execute the jump. |
| Terminal resize below 60 cols | Overlay switches to full-screen fallback at next render. |
| Plan/session changes underneath the view | Auto-close peek (same as task change). |

## Implementation notes

- The view-switch on `S` should be expressed as an `Action` variant (e.g. `Action::FocusWorkerInDashboard { agent, tab: DetailTab::Stream }`) so routing stays in `app/action_routing.rs` and `plan_inspector` does not reach into Dashboard internals.
- The extracted Stream renderer must be pure render-time over the passed-in `StreamViewState`. Any mutable scroll/follow updates land on the caller's `StreamViewState`, never on a globally shared one.
- The "no worker" status-line flash is purely visual — no popover, no overlay; reuse whatever status mechanism `plan_inspector` already has.

## Risks & mitigations

1. **Stream renderer is not currently a standalone function.** Mitigation: the extraction is part of this work. The plan must order it ahead of any plan_inspector edits so both call sites (`DetailPane` and the peek) move together.
2. **Event-source contention.** Two consumers rendering the same agent's stream simultaneously must both read from a shared subscription (not duplicate it). Mitigation: confirm the existing source already supports multiple readers; if not, scope a small adapter as part of the plan.
3. **Focus model regressions.** Adding `Mode` to `plan_inspector` changes every key-handling path. Mitigation: route all key events through a single dispatcher that matches on `Mode` first, then delegates.

## Open questions resolved

- **Direction (A/B/C):** C — both popover and jump-nav.
- **Hotkeys:** `s` peek, `S` jump.
- **Sizing:** 60-col minimum, full-screen fallback.
- **Focus model:** local `Mode` enum, not a global Focusable.
- **Scroll/follow state sharing with Dashboard:** **NOT shared.** Peek owns a transient `StreamViewState`, scrollable independently.

## Out of scope (future work)

- Pinning a peek so it survives task-selection changes.
- Side-by-side peeks for two workers.
- Sending input to a worker from the peek.
