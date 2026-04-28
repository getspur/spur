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
7. **View change**: Reversible tombstone **PERSISTS** until time-expiry. The ambient badge (§4.10) hides when the user is on a different view, but the slot survives — navigating back to the originating view restores the badge and `u` still works for the remaining window. Per-view scoping governs *`u`'s consumption rule* (only consumed when `current_view == slot.view`), NOT the slot's *lifecycle*. (Amendment 2026-04-28: original design evicted on nav; revised after first-principles UX review found that breaks Tab-cycle workflows where the user briefly checks another view mid-task.)
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

### 4.5.1 Two `SubmitReview` emit sites + dispatch-vs-install bifurcation

Claude-code's feasibility review noted that `SubmitReview` is dispatched from two locations in `dashboard.rs`: the vim Normal handler at `:1112` and the Insert-mode handler at `:1238`. Both already route through `App::process_action(Action::SubmitReview { … })` — installing the queue tombstone in `process_action`'s `Action::SubmitReview` arm covers both sites with a single change. No view-level patching needed.

**Re-entrance hazard (amendment 2026-04-28, brain self-review found this latent at Task 9):** if both the user-press path AND the tick-driven 3s-expiry dispatch route through the same `process_action(Action::SubmitReview)` arm, the tick-expiry would re-install another tombstone instead of actually sending the review — the action would never reach the ACP backend (infinite-delay loop, review never sent).

**Resolution: bifurcate the dispatch path.** Introduce a new `Action::SubmitReviewDispatch { ... }` variant whose `process_action` arm performs the bare ACP send WITHOUT installing a tombstone. The existing `Action::SubmitReview { ... }` arm continues to install the queue tombstone (no actual send). Tombstone's `pending` field stores the *Dispatch* variant.

```rust
// User presses A → Dashboard returns Action::SubmitReview { decision: Approve, … }
// process_action arm:
Action::SubmitReview { ref executor_id, attempt_n, decision } => {
    let pending = Action::SubmitReviewDispatch {
        executor_id: executor_id.clone(),
        attempt_n,
        decision,
    };
    self.tombstones.install(Tombstone {
        view: ViewId::Dashboard,
        kind: TombstoneKind::QueuedRemote { pending },
        label: format!("{}", decision),
        created_at: Instant::now(),
        expires_at: Instant::now() + Duration::from_secs(3),
    });
    self.flash_hint(format!("{} — press u to revert (3s)", decision),
                    Duration::from_secs(2));
    // No actual send. Send happens via tick-expiry → SubmitReviewDispatch.
}

// 3s tick-expiry → tombstones.tick(now) returns vec![Action::SubmitReviewDispatch { … }]
// process_action arm:
Action::SubmitReviewDispatch { ref executor_id, attempt_n, decision } => {
    // Bare ACP send. Reuses existing send-review code path — no tombstone install.
    self.send_review_to_acp(executor_id, attempt_n, decision);
    self.flash_hint_short("Sent.");
}
```

The displaced-by-next-action path (§4.3 bullet 6) also dispatches the displaced `Action::SubmitReviewDispatch` directly — same bare-send path, no re-install.

The `u` cancel path is unchanged: tombstone evicted, `pending` (the Dispatch variant) is dropped, never dispatched.

This bifurcation also prevents the brain-self-review's secondary concern: if process_action ever runs into a cycle where install-arms call other install-arms, the bifurcation ensures dispatch-arms are terminal (no further installs).

The same bifurcation pattern applies to any future QueuedRemote action class — define a `*Dispatch` variant for the bare-send path. Reversible tombstones don't need bifurcation (their inverse is the SAME variant as the install, and the install arm correctly captures-and-dispatches; tick-expiry of Reversible silently drops without re-dispatch per §4.7).

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

**Activation-gate enumeration** (amendment 2026-04-28): the undo handler ONLY consumes `u` / `Ctrl+Z` when the active view is in pure view-key context. The handler MUST NOT fire when ANY of the following ownership flags are true (verified against quick-fixes T5 owner-order):

| Context | `u` / `Ctrl+Z` behavior | Routing |
|---|---|---|
| `input_bar.is_active() && !input_bar.is_empty()` (vim Insert / Emacs typing / vim Normal with cursor in non-empty buffer) | passthrough | input_bar handles → text-undo via tui-textarea |
| Mention picker open (`@`-trigger active) | passthrough | picker consumes navigation/edit keys |
| Slash command picker open (`/`-trigger active) | passthrough | same |
| History shell active (`Up`/`Down` history nav with body shown) | passthrough | history shell consumes |
| Permission prompt pending (`y/n/a` waiting on agent perm question) | passthrough | permission handler consumes |
| Help overlay open (`?` toggled on) | block (no-op flash `"close help to undo"`) | overlay-modal — explicit don't-route |
| Mermaid render-picker open | passthrough | render-picker consumes |
| Quit-confirm modal open | block (no-op) | modal — only `y/n` consumed |
| Leader-menu popup open (post-leader-key spec) | block (no-op) | leader sequence active |
| **None of the above** | consume → tombstone undo | normal path |

The implementation MUST grep for these context-active checks at handler entry and short-circuit BEFORE evicting the tombstone. Order matches quick-fixes T5's session_detail.rs ownership cascade (composer-non-empty > picker > history-shell > view-keys).

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

### 4.8 Hint-slot integration (two-channel split)

**Amendment 2026-04-28**: original design routed the tombstone toast into the single-line hint slot at high priority, which monopolized the slot for 60s and suppressed unrelated transient feedback (`"Copied 'foo'"`, `"Saved draft"`, etc.). Revised first-principles UX review split the channel:

**Channel A — Ambient countdown badge (NEW, see §4.10)**: persistent, low-prominence, right-aligned in the status bar. Hosts ONLY the tombstone countdown. Always visible while a tombstone is active for the current view. Does NOT compete with hint-slot flashes.

**Channel B — Transient hint slot (existing)**: short-lived 1–3s flashes for general feedback. Continues to host:
1. Panic-Esc reset confirmation (1s flash) — highest priority
2. **`"Undid: …"` / `"Cancelled: …"` confirmation flash on undo** (1s)
3. Leader-menu inline preview
4. Esc-cancel-stream hint
5. General status (free for `"Copied"`, `"Saved"`, etc.)

The 60s "Press u to undo" copy lives in Channel A (badge); the 1s post-action confirmation flash lives in Channel B. Tombstone install fires BOTH channels at install time:
- Channel A: badge appears, counts down for 60s (or 3s).
- Channel B: optional install flash `"Archived 'foo' — press u to undo"` for 2s, then yields slot.

The 2-second install flash is the "you've been heard" feedback. After it expires, the badge in Channel A continues showing the countdown unobtrusively. Other transient hints can flash in Channel B without affecting the badge.

### 4.9 What this design intentionally doesn't do

- **Multiple tombstones per view**: rejected. Single slot keeps mental model simple ("u undoes my last action") and keeps memory bounded.
- **Cross-view undo navigation**: rejected. Per-view isolation matches user mental model — you undo what you just did in the place you did it. (Note: the tombstone *slot* now persists across nav per amended §4.2 bullet 7, but `u` still consumes only when current_view matches slot.view.)
- **Persistent tombstone across sessions**: rejected. 60s window assumes the user is paying attention; longer than that, the action is finalized. Restart = fresh slate.
- **Configurable window length**: rejected. 60s and 3s are calibrated for the action classes. Don't expose tuning knobs that erode the mental model.
- **Confirmation prompt fallback**: rejected (gemini's BLOCK was right). The toast is the confirmation. Adding "press X again to confirm" on top of the queue window would double-tax users.

### 4.10 Ambient countdown badge — display location and format

**Channel A** (new). The badge renders in the status bar, right-aligned, AFTER the license badge and before any clock/right-edge element. Format:

```
  [u: archived 'foo' 45s]
```

Components:
- `[u: …]` prefix: literal `u` character + colon + space. Hints both at the action AND the keystroke. Reads as "press u: …".
- `archived 'foo'` (or other label): the `Tombstone.label` field, abbreviated to fit. If the label > 24 chars, truncate with ellipsis: `archived 'verylongsessio…'`.
- `45s` countdown: integer seconds remaining, monotonically decreasing. Updates every render frame (33ms tick); only visibly changes once per second.
- Style: `Style::default().fg(Color::DarkGray)` matching other ambient indicators (license badge color). NOT bold, NOT highlighted — the goal is "ambient, glanceable, doesn't fight for attention."

For QueuedRemote (3s window), the format adapts:
```
  [u: revert Approve 2s]
```
The `revert` verb reads more naturally for "you can still cancel" semantics than `undo`.

The badge is rendered by a new `render_tombstone_badge` helper in `crates/spur-tui/src/components/status_bar.rs`. The helper takes `Option<&Tombstone>` (peek result from TombstoneSlots filtered to current_view) and returns an empty `Line` when None.

**Visibility rule**: badge displays ONLY when `current_view == slot.view`. If the user navigates away from the originating view, the slot persists (per amended §4.2 bullet 7) but the badge is hidden. Returning to the view restores the badge with the remaining time.

**Width budget**: 30 chars max (`[u: archived 'verylongsessio…' 60s]`). The status bar's right-aligned region must reserve this when active. If status bar width < 80 cols, the badge MAY render in shortened form `[u 45s]` (drop the label). Long labels, narrow widths — defer detailed width-aware truncation to render-time.

### 4.11 `u` keybinding collision audit (PREREQUISITE — runs before Task 4)

Before Task 4 wires the undo handler, an audit task verifies `u` (lowercase, no modifiers) is currently unbound in every view that will host a tombstone slot. The audit is a 5-minute grep + visual inspection task:

```bash
# Find every match of `KeyCode::Char('u')` in spur-tui:
rg "KeyCode::Char\('u'\)" crates/spur-tui/src/views/
```

Expected outcome (verified at spec-amendment time, 2026-04-28): no matches in any of `dashboard.rs`, `session_picker.rs`, `issue_browser.rs`, `plan_inspector.rs`. SessionDetail's `u` may match for tui-textarea text undo; that's input-bar-owned and handled by §4.6's compose-mode passthrough.

If the audit finds an unexpected `u` binding in a view-key context, the spec MUST be amended to either rebind that key or reroute the tombstone undo to an alternate (`U`?). Don't proceed to Task 4 until the audit is documented.

The audit deliverable: a one-paragraph audit-result note appended to this spec confirming `u` is free, OR an amendment to use a different key.

**Audit result (2026-04-28, executed by brain at spec amendment time):**

```
$ rg "Char\('u'\)" crates/spur-tui/src/views/
crates/spur-tui/src/views/dashboard.rs:1128:    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
```

Only one view-level match: `dashboard.rs:1128` — `Ctrl+U` for scroll-up-by-5. Bare lowercase `u` is **unbound in every view** (Dashboard, SessionPicker, IssueBrowser, PlanInspector, SessionDetail view-keys). Tombstone undo can safely claim bare `u` without collision. `Ctrl+U` continues to scroll Dashboard. `Ctrl+Z` (emacs undo) is also unbound at view level — `input_bar.rs:345` binds `Ctrl+U` for emacs kill-line-backward, which is composer-internal and gated by §4.6's compose-mode passthrough.

## 5. Test plan

| Test name | Scenario | Asserts |
|---|---|---|
| `tombstone_installs_on_archive_with_60s_window` | SessionPicker `x` | tombstone present for SessionPicker; `expires_at == created_at + 60s`; toast text includes session name |
| `tombstone_undo_dispatches_inverse_action` | archive + `u` within window | `Action::ToggleSessionArchive` re-dispatched; tombstone evicted; toast `"Undid: …"` |
| `tombstone_undo_failure_surfaces_error` | issue status set + `u` + simulated beads write failure | toast updates to `"Undo failed: …; original action stands"`; tombstone evicted |
| `tombstone_window_expiry_finalizes` | archive + wait 61s | tombstone evicted via tick; `u` after expiry → `"nothing to undo"` |
| `tombstone_per_view_isolation` | archive in SessionPicker; switch to Dashboard; press `u` | Dashboard tombstone empty; press `u` → `"nothing to undo"` flash; SessionPicker slot still active (amended §4.2 bullet 7) |
| `tombstone_persists_across_view_change` (NEW, amend) | archive in SessionPicker; nav to Dashboard; nav back to SessionPicker; press `u` within original 60s | tombstone still active; `u` consumes; inverse dispatched |
| `tombstone_badge_hidden_when_off_view` (NEW, amend) | archive in SessionPicker; nav to Dashboard | render frame on Dashboard does NOT include `[u: archived ...]` badge in status bar; nav back → badge restored with reduced countdown |
| `tombstone_install_flash_yields_after_2s` (NEW, amend §4.8) | archive | install-flash `"Archived 'foo' — press u to undo"` shows for 2s in Channel B; after 2s, badge in Channel A continues showing the countdown; Channel B is free for general status |
| `general_status_flash_does_not_clobber_badge` (NEW, amend §4.8) | archive; immediately trigger general-status flash (e.g. `"Copied 'foo'"`) | Channel B shows the copy flash; Channel A badge continues unchanged |
| `undo_blocked_by_picker_open` (NEW, amend §4.6) | archive in SessionPicker; open mention picker; press `u` | tombstone NOT consumed; `u` flows to picker; tombstone slot still active |
| `undo_blocked_by_help_overlay` (NEW, amend §4.6) | archive in SessionPicker; open `?` help; press `u` | tombstone NOT consumed; flash `"close help to undo"`; help still open; tombstone slot still active |
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

1. **Compose-mode `u`** — RESOLVED in §4.6 amendment: explicit ownership cascade enumerated. Input-bar non-empty (any mode) → passthrough; pickers/history-shell/permission-prompt → passthrough; help-overlay/quit-confirm/leader-popup → block (no-op flash). Tombstone consumes only in pure view-key context.
2. **Tombstone visibility during fast pressing** — RESOLVED in §4.8 two-channel split: ambient badge persists across rapid actions; install-flash on each new action confirms "you've been heard." Power users see the badge update without losing other status flashes.
3. **Picker rename undo — `RenameState.original_title` required**: rename's "previous title" capture must happen BEFORE the rename-prompt opens. Current `RenameState` at `session_picker.rs:1476-1486` lacks an `original_title: String` field. **This spec adds that field** as part of its implementation surface. The session_picker rename mode (R key) currently lets users edit a buffer; the capture is taken at rename-mode entry, not at commit time. The Action::RenameSession dispatcher in process_action reads `RenameState.original_title` to construct the inverse Action.
4. **WorkOn session-spawn tombstone**: should `W` in IssueBrowser get the 3s queue treatment too? Subprocess spawn IS reversible at process-creation time (kill the subprocess) but introduces complexity (need to track child PID). Defer — `W` keeps current immediate-spawn behavior with a regular toast `"Spawned WorkOn session 'foo'"` (no tombstone).
5. **Tombstone display when view-overlay visible** — RESOLVED in §4.10 (badge in status bar, right-aligned, ambient). Status bar renders below all overlays; badge is never occluded. Channel B (transient hint slot) follows quick-fixes §6.3 priority — panic-Esc preempts. Leader popup is a separate render layer that doesn't touch the status bar.
6. **3s SubmitReview queue creates "schrödinger's submission" on crash/quick-close**: the action sits client-side for up to 3s before dispatch. If the app crashes in those 3s, the review is lost. Mitigation: the auto-flush-on-next-action rule (§4.3 bullet 6) and view-change rule (§4.3 bullet 7) collapse the window for power users. The bare-tail-of-batch case (single review, walk away) remains a 3s data-loss window. Accepted tradeoff — the user explicitly asked the spec to err on the safety side. If telemetry shows lost reviews from this window, ADR for an in-flight "draining queue on shutdown" handler.

## 8. Method note

This spec was rewritten on 2026-04-28 after dual-track cross-review (codex APPROVE-WITH-AMENDMENTS, gemini BLOCK on the original UndoStack+ConfirmState dual system) and a 7-round L9 UX synthesis. The key insights driving the rewrite:

1. **YAGNI applies**: SPUR has 2 reversible-action families (archive/status) and 4 review-submit irrevocables. A generalized N-deep stack + ConfirmState wrapper is over-engineered for that surface (gemini's BLOCK).
2. **Gmail toast is the right pattern** for low-frequency destructive ops in productivity tools. Editor-style stacks (vim's `u`) belong with high-frequency text mutations, not chat-archive operations.
3. **Network irrevocability is solved by client-side queueing** during the toast window. Same UX path as reversible — same toast pattern, same `u` keybinding, just different mechanism (cancel-queue vs dispatch-inverse). Removes the need for separate confirm prompts.
4. **Closure-based undo doesn't work for beads-backed mutations** (codex's amendment). Inverse-Action dispatch through `process_action` does, and threads through existing failure-handling.

The original spec's `[ui.destructive]` config toggle is dropped — the behavior is universal. Codex's namespace correction (`[ui.*]` → `[tui.*]`) is moot since there's no config to namespace.
