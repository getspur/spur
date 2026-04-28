# TUI Destructive-Action Undo + Confirm — design

**Status:** design draft, pending dual review (codex + gemini)
**Date:** 2026-04-28
**Owner:** Kevin Truong (kevin.truong.ds@gmail.com)
**Predecessors:**
- `docs/rca/2026-04-28-spur-tui-keybindings-mapping.md` — full reference map
- `docs/rca/2026-04-28-spur-tui-keybindings-ergonomic-review.md` — §12.2 (gemini split text-undo vs destructive-state-undo; codex elevated this above counted motions)
- `2026-04-28-tui-keybinding-quick-fixes-design.md` — §4.3 deprecates `d` for archive

**Background:** SPUR's TUI has multiple keys that mutate durable state without confirmation or undo:

| Key | Effect | Persistence |
|---|---|---|
| SessionPicker `d`/`x` | Archive session | written to filesystem |
| SessionPicker `p` | Toggle pin | written to filesystem |
| IssueBrowser `d`/`x` | Issue status → closed | written to beads |
| IssueBrowser `o` | Issue status → open | written to beads |
| IssueBrowser `w` | Issue status → in_progress | written to beads |
| IssueBrowser `b` | Issue status → blocked | written to beads |
| IssueBrowser `W` | Open `WorkOn` session | spawns subprocess |
| Dashboard Review `A`/`D`/`M`/`R` | Submit review decision | sent to ACP server |

Pressing any of these is **single-keystroke and irrevocable**. Codex's review elevated this above counted vim motions ("trust-breaking if vim mode is advertised"). Gemini split: text-input undo can stay deferred, but destructive-state-mutation undo "is critical".

---

## 1. Goal

Provide a single, uniform safety net for every key that durably mutates external state — locally OR remotely — using a Gmail-toast-style tombstone. After this work:

- Every destructive action commits (or queues, for remote) immediately and shows a toast: `"Archived 'foo'. Press u to undo (60s)"` or `"Approving… Press u to revert (3s)"`.
- One tombstone slot per view; the most-recent destructive action is always reversible.
- Pressing `u` (vim) / `Ctrl+Z` (emacs) reverses the action OR cancels the queued network dispatch.
- No config toggles. No pre-action confirm prompts. The toast IS the confirmation.

This design replaces the original spec's per-view UndoStack + ConfirmState dual system. The L9 UX synthesis (round 1–7 sequential thinking, 2026-04-28) determined that the dual system was over-engineered for SPUR's actual destructive surface (only 2 reversible action families and 4 review-submit actions today) and that the Gmail toast pattern is a better mental-model fit for low-frequency destructive ops in productivity tools.

## 2. Non-goals

- **No general text-input undo.** tui-textarea may have undo support; if so, expose via input_bar — that's a separate small task. This spec is about state-mutation undo.
- **No undo across sessions / restarts.** Tombstone is in-memory only.
- **No multi-step undo replay.** Single tombstone slot per view; the previous tombstone is gone the moment a new destructive action displaces it.
- **No redo.** Single-direction undo. Future ADR if needed.
- **No transactional batching.** Each action is its own tombstone entry.
- **No config toggle.** No `[tui.destructive]`. The behavior is the behavior. (Gemini's spec review BLOCK on the config toggle was correct.)
- **No pre-action confirm prompts.** No `"Press X again to confirm"`. The post-action toast covers the same UX surface without friction tax in the happy path.

## 3. Background — taxonomy of mutations

| Mutation class | Examples | Local-reversible? | Tombstone behavior |
|---|---|---|---|
| Local toggle | session pin, session archive | YES — write opposite | 60s window; commit immediately, `u` reverses |
| Local status set | issue status → open/in_progress/blocked/closed | YES — restore previous | 60s window; commit immediately, `u` reverses |
| Remote-trigger irrevocable | submit review Approve/Reject/Modify/Retry | NO once dispatched | 3s client-side queue; `u` cancels queue; after 3s dispatches |
| External resource | spawn WorkOn session (subprocess) | NO once spawned | 3s client-side queue; `u` cancels queue |
| Stream-cancel | SessionDetail first-Esc cancels in-flight stream | already-irrevocable | covered by quick-fixes spec §4.9 (one-shot hint), NOT in this spec |
| Send message | composer Submit | irrevocable to agent once on the wire | NOT covered by this spec — message-send is a primary user intent and adding tombstone friction breaks chat flow. Future ADR if needed. |

All five rows in this spec's scope (rows 1–4) get the same tombstone UX. Local-reversible has a 60s window; remote-irrevocable uses a 3s client-queue window. The user sees the same toast pattern either way.

## 4. Design

### 4.1 Tombstone data model

One tombstone slot per view. Hosted on the App.

**Prerequisite** (claude-code feasibility review): `ViewId` at `crates/spur-tui/src/action.rs:188` derives `Debug, Clone, PartialEq, Eq` only. To use it as a `HashMap` key, **add `Hash` to its derive list** (one-line patch). All inner fields (`SessionId` is `String`-backed) Hash trivially.

```rust
// In action.rs:
#[derive(Debug, Clone, PartialEq, Eq, Hash)]  // ← Hash added
pub enum ViewId { Dashboard, SessionDetail(SessionId), ... }

// In crates/spur-tui/src/components/tombstone.rs (new module):
pub struct Tombstone {
    pub view: ViewId,
    pub kind: TombstoneKind,
    pub label: String,        // shown in toast: "Archived 'foo'"
    pub created_at: Instant,
    pub expires_at: Instant,  // created_at + window
}

pub enum TombstoneKind {
    /// Action already committed locally. `u` dispatches the inverse action
    /// through the normal process_action path so backend writes are routed
    /// the same as the original mutation (codex's spec review:
    /// closure-based revert breaks for beads-backed status changes).
    Reversible { inverse: Action },

    /// Action is queued; will dispatch at expires_at unless `u` cancels.
    /// `u` cancels: drop the action, never dispatch.
    /// Expiry: dispatch the action via process_action.
    QueuedRemote { pending: Action },
}

pub struct TombstoneSlots {
    /// One slot per view. Keys ARE limited — Dashboard, SessionDetail,
    /// SessionPicker, IssueBrowser, PlanInspector. MermaidViewer has no
    /// destructive ops.
    by_view: HashMap<ViewId, Tombstone>,
}

impl TombstoneSlots {
    /// Quick-fixes spec §4.10 panic-Esc handler calls this. Drops all
    /// tombstones WITHOUT dispatching queued-remote actions.
    pub fn cancel_all_without_dispatch(&mut self) {
        self.by_view.clear();
    }
}
```

### 4.2 Behavior — reversible actions (60s window)

When a reversible-class key fires (e.g. SessionPicker `x` archive):

1. **Commit immediately** through the normal action path. Backend write happens. UI updates.
2. **Capture the inverse `Action`** (e.g. for "archived 'foo'" the inverse is `Action::ToggleSessionArchive { session_id }` — same action variant since it toggles).
3. **Install tombstone** on `TombstoneSlots[ViewId::SessionPicker]` with `expires_at = now + 60s`. Replaces any prior tombstone for that view.
4. **Render toast**: `"Archived 'foo'. Press u to undo (60s)"` in the hint slot. Toast updates the countdown each render.
5. **`u` keystroke** (vim Normal or Emacs `Ctrl+Z`) within window: pop tombstone, dispatch `kind.inverse` through `process_action`, render `"Undid: archived 'foo'"` for 1s, clear hint.
6. **Window expires**: tombstone evicted, toast clears. Action is finalized.
7. **View change**: tombstone evicted (action committed). Per-view persistence — switching views doesn't carry the slot.
8. **Triple-Esc panic** (quick-fixes T2.9): all tombstones cleared without dispatching anything. (Reversible has already committed; panic just stops the user undoing it. Acceptable — they pressed panic, they wanted out.)

### 4.3 Behavior — irrevocable network actions (3s queue window)

When an irrevocable-class key fires (e.g. Dashboard Review `A` for Approve):

1. **DO NOT dispatch yet.** Capture the `Action` value into a `TombstoneKind::QueuedRemote { pending }`.
2. **Install tombstone** on `TombstoneSlots[ViewId::Dashboard]` with `expires_at = now + 3s`. Replaces any prior tombstone for the view.
3. **Render toast**: `"Approving… Press u to revert (3s)"` in the hint slot.
4. **`u` keystroke within 3s**: drop the tombstone WITHOUT dispatching. Render `"Cancelled: Approve"` for 1s. Action never went anywhere.
5. **Window expires (3s elapsed without `u`)**: dispatch `pending` through `process_action`. Toast updates to `"Sent."` for 1s, then clears.
6. **User presses ANOTHER destructive key (e.g. another `A` on the next review)**: the new action displaces the current tombstone — Q1 dispatches IMMEDIATELY (before its 3s expires) so Q2 can take the slot. Toast for Q2 shows. **Zero friction tax for power-user rapid-approve flows** — the user just keeps pressing keys, the queue auto-flushes.
7. **View change**: same as (6) — current tombstone dispatches immediately so it doesn't get stranded.
8. **Triple-Esc panic**: tombstone CANCELLED without dispatching. This is the user's escape hatch from a queued action they regret AND can't reach `u` for.

### 4.4 Coverage map

| Action variant | View | Class | Window | Inverse |
|---|---|---|---|---|
| `Action::ToggleSessionArchive { session_id }` (`x`, deprecated `d`) | SessionPicker | reversible | 60s | same variant (toggling twice returns) |
| `Action::ToggleSessionPin { session_id }` (`p`) | SessionPicker | reversible | 60s | same variant |
| `Action::RenameSession { session_id, new_title }` | SessionPicker | reversible | 60s | same variant with `new_title = original_title` (captured from `RenameState.original_title` — new field, see §7) |
| `Action::Issue(IssueAction::UpdateStatus { id, status })` (`o`/`w`/`b`/`d`/`x`/`W`) | IssueBrowser | reversible | 60s | same variant with `status = previous_status` (`String` snapshot read from `IssueRow::status` before dispatch) |
| `Action::SubmitReview { decision: Approve, … }` (`A`) | Dashboard | irrevocable | 3s queue | (queue cancel; never dispatched) |
| `Action::SubmitReview { decision: Reject, … }` (`D`) | Dashboard | irrevocable | 3s queue | (queue cancel) |
| `Action::SubmitReview { decision: Modify, … }` (`M`) | Dashboard | irrevocable | 3s queue | (queue cancel) |
| `Action::SubmitReview { decision: Retry, … }` (`R`) | Dashboard | irrevocable | 3s queue | (queue cancel) |

Out of scope: composer Submit (chat send), SessionDetail Esc-cancel-stream (quick-fixes §4.9 covers), `WorkOn` session spawn (separate decision — acceptable to confirm via leader sequence later).

**Action variant verification** (claude-code feasibility review): all variants above are confirmed against `crates/spur-tui/src/action.rs` as of HEAD. The original spec invented `Action::SetIssueStatus { issue_id, status }` which does NOT exist; the real path goes through `Action::Issue(IssueAction::UpdateStatus { id, status })` where `status` is a raw `String`.

### 4.5 Inverse-action dispatch (codex's + claude-code's amendments)

The inverse goes through `App::process_action`, NOT a captured closure. Codex's spec review caught that issue-status undo isn't an in-memory op — it must hit beads, handle failure, and refresh the issue list. Closure-based revert can't express that without re-implementing every mutation path inside the closure.

**Important install location** (claude-code's feasibility review): tombstone install fires in `App::process_action`'s arm for each tracked action. Views still return the `Action` upward unchanged from their `handle_key` — they don't manage tombstones directly. This keeps the dispatch graph linear and avoids two parallel mutation sites.

**Concrete API references** (claude-code's feasibility review caught the original spec invented `Action::SetIssueStatus`):

```rust
// IssueBrowser status set goes through Action::Issue(IssueAction::UpdateStatus)
// which is the actual variant at action.rs:7-8. Status is a String, not an enum.

// In App::process_action:
Action::Issue(IssueAction::UpdateStatus { ref id, ref status }) => {
    // Capture previous status BEFORE dispatching.
    let previous_status: String = self.issue_browser
        .as_ref()
        .and_then(|v| v.row_status(id))
        .unwrap_or_else(|| "open".into());

    let inverse = Action::Issue(IssueAction::UpdateStatus {
        id: id.clone(),
        status: previous_status,
    });
    let label = format!("issue '{}' → {}", id, status);
    self.tombstones.install(ViewId::IssueBrowser, Tombstone {
        view: ViewId::IssueBrowser,
        kind: TombstoneKind::Reversible { inverse },
        label: label.clone(),
        created_at: Instant::now(),
        expires_at: Instant::now() + Duration::from_secs(60),
    });
    self.flash_hint(format!("{}. Press u to undo (60s)", label),
                    Duration::from_secs(60));
    // THEN dispatch the original (e.g. via PM service call).
    // ... existing dispatch path ...
}
```

Equivalent shape for `Action::ToggleSessionArchive`, `Action::ToggleSessionPin`, `Action::RenameSession`. Each captures its own inverse-Action shape:
- `ToggleSessionArchive` is its own inverse (toggling twice returns).
- `ToggleSessionPin` is its own inverse.
- `RenameSession` inverse needs the previous title — captured from `RenameState.original_title` (new field; see §7).

If the inverse dispatch FAILS at backend (e.g. beads write error), the `IssueAction::UpdateStatus` arm's existing error path fires — `App` extends it to call `self.flash_hint("Undo failed: …; original action stands", Duration::from_secs(3))`. The user sees their request was rejected.

### 4.5.1 Two `SubmitReview` emit sites

Claude-code's feasibility review noted that `SubmitReview` is dispatched from two locations in `dashboard.rs`: the vim Normal handler at `:1112` and the Insert-mode handler at `:1238`. Both already route through `App::process_action(Action::SubmitReview { … })` — installing the queue tombstone in `process_action`'s `Action::SubmitReview` arm covers both sites with a single change. No view-level patching needed.

### 4.6 `u` / `Ctrl+Z` keystroke handler

`flash_hint` is defined in quick-fixes spec §4.11 (the shared `App::transient_hint` infrastructure). Both specs ship together so the API is available.

```rust
fn handle_undo(app: &mut App) -> Option<Action> {
    let view = app.current_view();
    let Some(tombstone) = app.tombstones.evict(view) else {
        app.flash_hint_short("nothing to undo");  // 2s
        return None;
    };
    match tombstone.kind {
        TombstoneKind::Reversible { inverse } => {
            app.flash_hint_short(format!("Undid: {}", tombstone.label));
            Some(inverse)  // dispatched through normal process_action
        }
        TombstoneKind::QueuedRemote { pending: _ } => {
            app.flash_hint_short(format!("Cancelled: {}", tombstone.label));
            None  // queued action dropped; never dispatched
        }
    }
}
```

Bound at the app level so `u` from vim Normal and `Ctrl+Z` from Emacs both route here. The handler is gated on `is_vim_normal() && view_owner` (vim) or `Ctrl+Z + view_owner` (emacs).

**Compose-mode passthrough**: when input bar is composing (vim Insert / Emacs typing), `u` and `Ctrl+Z` MUST flow through to the input bar (eventual text undo via tui-textarea). The activation gate is identical to leader-key §5: input-bar focused = passthrough.

### 4.7 Tombstone tick driver

Tombstones expire on a wall-clock basis. The TUI tick loop calls `app.tombstones.tick(now)` once per frame:

```rust
impl TombstoneSlots {
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut to_dispatch = Vec::new();
        self.by_view.retain(|_view, ts| {
            if now >= ts.expires_at {
                if let TombstoneKind::QueuedRemote { pending } = &ts.kind {
                    to_dispatch.push(pending.clone());
                }
                false  // drop
            } else {
                true  // keep
            }
        });
        to_dispatch
    }
}
```

Returned actions are dispatched by `App` via `process_action` after the retain pass. This keeps the tombstone slot pure-data and dispatch out-of-band.

### 4.8 Hint-slot integration

Tombstone toast renders into the bottom-of-view single-line hint slot. Per quick-fixes §6.3, slot priority (highest first):

1. Panic-Esc reset confirmation (1s flash)
2. Tombstone toast ← **this spec**
3. Leader-menu inline preview
4. Esc-cancel-stream hint
5. General status

So a tombstone toast yields immediately to a panic-reset, but otherwise shows for its full window.

### 4.9 What this design intentionally doesn't do

- **Multiple tombstones per view**: rejected. Single slot keeps mental model simple ("u undoes my last action") and keeps memory bounded.
- **Cross-view undo navigation**: rejected. Per-view isolation matches user mental model — you undo what you just did in the place you did it.
- **Persistent tombstone across sessions**: rejected. 60s window assumes the user is paying attention; longer than that, the action is finalized. Restart = fresh slate.
- **Configurable window length**: rejected. 60s and 3s are calibrated for the action classes. Don't expose tuning knobs that erode the mental model.
- **Confirmation prompt fallback**: rejected (gemini's BLOCK was right). The toast is the confirmation. Adding "press X again to confirm" on top of the queue window would double-tax users.

## 5. Test plan

| Test name | Scenario | Asserts |
|---|---|---|
| `tombstone_installs_on_archive_with_60s_window` | SessionPicker `x` | tombstone present for SessionPicker; `expires_at == created_at + 60s`; toast text includes session name |
| `tombstone_undo_dispatches_inverse_action` | archive + `u` within window | `Action::ToggleSessionArchive` re-dispatched; tombstone evicted; toast `"Undid: …"` |
| `tombstone_undo_failure_surfaces_error` | issue status set + `u` + simulated beads write failure | toast updates to `"Undo failed: …; original action stands"`; tombstone evicted |
| `tombstone_window_expiry_finalizes` | archive + wait 61s | tombstone evicted via tick; `u` after expiry → `"nothing to undo"` |
| `tombstone_per_view_isolation` | archive in SessionPicker; switch to Dashboard; press `u` | Dashboard tombstone empty; SessionPicker tombstone evicted on view-change |
| `tombstone_replaces_on_new_destructive_action` | archive A; archive B 1s later | A's tombstone evicted; B's tombstone shown; `u` undoes B only |
| `tombstone_remote_queue_dispatches_after_3s` | Review `A`; tick clock 3s | `Action::SubmitReview { Approve }` dispatched once via process_action; toast updates to `"Sent."` |
| `tombstone_remote_queue_cancel_via_u` | Review `A`; press `u` within 3s | `Action::SubmitReview` NEVER dispatched; toast `"Cancelled: Approve"` |
| `tombstone_remote_queue_displaced_by_next_action_dispatches_immediately` | Review `A`, then `D` 1s later | A dispatched immediately when D queued; D's tombstone shown |
| `tombstone_view_change_flushes_remote_queue` | Review `A`; navigate to PlanInspector | A dispatched immediately; no orphan tombstone |
| `tombstone_panic_esc_cancels_remote_without_dispatch` | Review `A`; triple-Esc within 1s | A NEVER dispatched; tombstone cleared; ViewId == Dashboard root |
| `undo_keystroke_in_emacs_uses_ctrl_z` | archive + `Ctrl+Z` (emacs mode) | identical to vim `u` behavior |
| `nothing_to_undo_status` | press `u` with empty tombstone slot | `"nothing to undo"` flashed for 1s |

## 6. Rollout & cross-spec coordination

### 6.1 Spec ordering (gemini's spec review)

This spec ships **in the same release** as `2026-04-28-tui-keybinding-quick-fixes-design.md`. Reasoning: quick-fixes commit 3 (`d`→`x` migration) changes the user's muscle memory for archiving. Without `u` available simultaneously, users hit a regression window where new keys exist but no safety net does.

**Release N** (single PR or single feature branch):
1. Quick-fixes commits 1–11 land (small surgical fixes).
2. This spec lands as one feature commit on top.
3. Both ship together as Release N.

**Release N+1**: leader-key spec ships on the stabilized routing layer.

### 6.2 Implementation surface

- New module: `crates/spur-tui/src/components/tombstone.rs` (struct `Tombstone`, `TombstoneKind`, `TombstoneSlots`).
- New field on `App`: `tombstones: TombstoneSlots`.
- New tick driver call in the main TUI loop: `let to_dispatch = app.tombstones.tick(now); for action in to_dispatch { app.process_action(action); }`.
- New keystroke routing: `u` (vim Normal) and `Ctrl+Z` (emacs) → `handle_undo` at app level, gated on view-owner status (i.e. not consumed when input bar is composing or picker active).
- Per-action call sites: SessionPicker archive/pin/rename, IssueBrowser status set, Dashboard Review submit. Each gets a `tombstone.install(...)` call AROUND its existing dispatch (or REPLACING the dispatch for queued-remote class).

NO new module for `UndoStack`, `ConfirmState`, or config types. Those structures from the original spec are not built.

### 6.3 Migration / coexistence

This spec consumes quick-fixes §4.3's `d`→`x` migration:
- Both `x` (new primary) and `d` (deprecated alias) install tombstones identically.
- The deprecation toast on `d` (quick-fixes spec) is rendered AFTER the tombstone toast — the user sees `"Archived 'foo'. Press u to undo (60s)"` first, then can dismiss to see `"d → archive renamed to x"` separately. This is fine because the deprecation toast is a one-shot per session.

There is NO config toggle — every user gets tombstone behavior. Power users who would have set `destructive = "off"` are protected by the zero-friction-tax design (rapid sequential approves auto-flush the queue, so power-user flow is unaffected).

## 7. Open questions

1. **Compose-mode `u`**: in vim Insert / Emacs typing mode, `u` and `Ctrl+Z` should pass through to the input bar (eventual text undo via tui-textarea). Tombstone undo only fires when input bar is NOT composing. Confirmed by the activation gate; tested explicitly.
2. **Tombstone visibility during fast pressing**: if user does Q1 + Q2 in <500ms, the toast for Q1 is barely visible. Acceptable — the design intent is that fast users don't need confirmation. Toast is for the slow/unsure user.
3. **Picker rename undo — `RenameState.original_title` required**: rename's "previous title" capture must happen BEFORE the rename-prompt opens. Current `RenameState` at `session_picker.rs:1476-1486` lacks an `original_title: String` field. **This spec adds that field** as part of its implementation surface. The session_picker rename mode (R key) currently lets users edit a buffer; the capture is taken at rename-mode entry, not at commit time. The Action::RenameSession dispatcher in process_action reads `RenameState.original_title` to construct the inverse Action.
4. **WorkOn session-spawn tombstone**: should `W` in IssueBrowser get the 3s queue treatment too? Subprocess spawn IS reversible at process-creation time (kill the subprocess) but introduces complexity (need to track child PID). Defer — `W` keeps current immediate-spawn behavior with a regular toast `"Spawned WorkOn session 'foo'"` (no tombstone).
5. **Tombstone display when view-overlay visible**: if leader popup or quit-confirm is open, where does the toast render? Per quick-fixes §6.3, panic-Esc preempts; otherwise the toast renders into the bottom-of-view single-line slot, possibly under the leader popup. Defer detailed rendering — simplest impl is "toast renders in slot regardless of overlay".

## 8. Method note

This spec was rewritten on 2026-04-28 after dual-track cross-review (codex APPROVE-WITH-AMENDMENTS, gemini BLOCK on the original UndoStack+ConfirmState dual system) and a 7-round L9 UX synthesis. The key insights driving the rewrite:

1. **YAGNI applies**: SPUR has 2 reversible-action families (archive/status) and 4 review-submit irrevocables. A generalized N-deep stack + ConfirmState wrapper is over-engineered for that surface (gemini's BLOCK).
2. **Gmail toast is the right pattern** for low-frequency destructive ops in productivity tools. Editor-style stacks (vim's `u`) belong with high-frequency text mutations, not chat-archive operations.
3. **Network irrevocability is solved by client-side queueing** during the toast window. Same UX path as reversible — same toast pattern, same `u` keybinding, just different mechanism (cancel-queue vs dispatch-inverse). Removes the need for separate confirm prompts.
4. **Closure-based undo doesn't work for beads-backed mutations** (codex's amendment). Inverse-Action dispatch through `process_action` does, and threads through existing failure-handling.

The original spec's `[ui.destructive]` config toggle is dropped — the behavior is universal. Codex's namespace correction (`[ui.*]` → `[tui.*]`) is moot since there's no config to namespace.
