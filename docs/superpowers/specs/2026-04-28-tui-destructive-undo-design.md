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

Provide either undo or pre-action confirmation (via configuration) for every key that durably mutates external state, without forcing users into a click-heavy workflow. After this work:

- Every destructive action either produces an undo entry OR shows a one-keystroke confirmation, governed by a config setting.
- Default mode is "soft confirm" — destructive keys flash an inline "press X again to confirm" hint that auto-clears.
- An "undo stack" tracks the last N reversible actions per view; `u` (vim) or `Ctrl+Z` (emacs) reverts the most recent.

## 2. Non-goals

- **No general text-input undo.** tui-textarea may have undo support; if so, expose `u`/`Ctrl+Z` in input bar — but that's a separate small task. This spec is about state-mutation undo.
- **No undo for actions that are inherently destructive at a remote.** "Submit review Approve" sends a message to the agent; once delivered, no client-side undo can call it back. Those get the confirm UX, not the undo UX.
- **No undo across sessions/restarts.** Undo stack is in-memory only.
- **No multi-step undo replay** ("undo the last 5 actions"). Single-step `u`.
- **No redo.** Single-direction undo. `Ctrl+r` for redo is a future ADR.
- **No transactional batching** ("group these 3 actions; undo as one"). Each action is its own undo entry.

## 3. Background — taxonomy of mutations

| Mutation class | Examples | Reversible? | UX |
|---|---|---|---|
| Local-only state toggle | pin/unpin session, archive session | YES (write opposite) | undo stack |
| Local-only status set | issue status → open/in_progress/blocked/closed | YES (write previous status) | undo stack |
| Remote-trigger (irrevocable) | submit review Approve/Reject/Modify, send message, cancel stream | NO | confirm prompt |
| External resource (irrevocable) | spawn WorkOn session (subprocess) | NO | confirm prompt |

The reversible class gets undo. The irrevocable class gets one-keystroke "press X again within 2s to confirm".

## 4. Design

### 4.1 Config setting

```toml
[ui.destructive]
# "undo": for reversible actions, no confirm prompt; one undo stack per view.
# "confirm": confirm prompt before EVERY destructive action.
# "off": no confirm, no undo. Power-user mode.
default = "undo"
```

Default is `undo`. `confirm` is the safest. `off` matches today's behavior.

### 4.2 Undo stack — reversible actions

```rust
pub struct UndoEntry {
    pub view: ViewId,
    pub action_label: String,    // "Archived session 'foo'"
    pub revert: Box<dyn FnOnce(&mut AppState)>,  // closure to undo
    pub timestamp: Instant,
}

pub struct UndoStack {
    entries: VecDeque<UndoEntry>,
    cap: usize,  // default 10
}

impl UndoStack {
    pub fn push<F: FnOnce(&mut AppState) + 'static>(
        &mut self,
        view: ViewId,
        label: impl Into<String>,
        revert: F,
    ) { /* ... */ }

    pub fn pop_for_view(&mut self, view: ViewId) -> Option<UndoEntry> { /* ... */ }
}
```

Each reversible-class action push:

```rust
// session_picker.rs, on x/d archive:
let prev_archived = current_archived_state(&session_id);
app_state.toggle_archive(&session_id);
undo_stack.push(
    ViewId::SessionPicker,
    format!("archived '{}'", session_label),
    move |st| st.set_archived(&session_id, prev_archived),
);
```

Undo binding (per view):
- Vim Normal: `u` → pop most recent entry for current view, run revert closure, status hint: `"undid: archived 'foo'"`.
- Emacs / not-vim: `Ctrl+Z`.

If stack is empty: status hint `"nothing to undo"`.

### 4.3 Confirm prompt — irrevocable actions

For actions where revert is impossible (or remote), display a one-shot inline hint:

```
[Review] Approve · press A again within 2s to confirm
```

State machine in the view:
```rust
enum ConfirmState {
    Idle,
    Pending { key: KeyCode, action: Action, deadline: Instant },
}
```

First press of `A` (Approve): set Pending, render hint.
Second press of same key within 2s: emit action, clear state.
Different key OR timeout: clear state silently.

This is the same UX pattern as `Ctrl+C`/`Ctrl+Q` quit-confirm.

### 4.4 Coverage map

| Action | Class | UX |
|---|---|---|
| SessionPicker archive (`x`/`d`) | reversible | undo stack |
| SessionPicker pin (`p`) | reversible | undo stack (toggle) |
| SessionPicker rename | reversible | undo stack (restore prev title) |
| IssueBrowser status set | reversible | undo stack (restore prev status) |
| IssueBrowser WorkOn (`W`) | irrevocable | confirm prompt |
| Dashboard Review Approve (`A`) | irrevocable | confirm prompt |
| Dashboard Review Reject (`D`) | irrevocable | confirm prompt |
| Dashboard Review Modify (`M`) | irrevocable | confirm prompt |
| Dashboard Review Retry (`R`) | irrevocable | confirm prompt |
| SessionDetail Esc-cancel-stream | irrevocable | hint after-the-fact (already covered in quick-fixes spec §4.9) |

**Carve-out**: review submit can also be configured to skip confirm if `[ui.review.confirm = false]`. Reviewers may prefer fast workflow.

### 4.5 Hint rendering

Both undo confirmations and confirm prompts use the existing one-shot status-hint slot in the input-bar area (same slot that displays "Esc cancelled the active turn" from quick-fixes §4.9). One slot, one hint at a time. Newer hints overwrite older.

### 4.6 Implementation surface

- New module: `crates/spur-tui/src/components/undo.rs`
- New struct in `App`: `undo_stack: UndoStack`
- New struct in views: `confirm_state: ConfirmState`
- Per-view key handlers gain a "first invoke confirm; second invoke real" wrapper.

A small helper:

```rust
fn confirm_or_dispatch(
    state: &mut ConfirmState,
    key: KeyCode,
    action: Action,
    setting: DestructivePolicy,
) -> Option<Action> {
    if setting == DestructivePolicy::Off {
        return Some(action);
    }
    match state {
        ConfirmState::Pending { key: pk, .. }
            if *pk == key
                && state.deadline > Instant::now() => {
            *state = ConfirmState::Idle;
            Some(action)
        }
        _ => {
            *state = ConfirmState::Pending {
                key,
                action: action.clone(),
                deadline: Instant::now() + Duration::from_secs(2),
            };
            // Status hint set elsewhere via state observer
            None
        }
    }
}
```

## 5. Test plan

| Test name | Scenario | Asserts |
|---|---|---|
| `undo_stack_pushes_on_archive` | SessionPicker `x` | stack has one entry, label includes session name |
| `undo_stack_revert_restores_state` | archive + `u` | session unarchived; stack empty for view |
| `undo_stack_per_view_isolation` | archive in picker, switch to dashboard, press `u` | nothing to undo (different view stack) |
| `undo_stack_capped` | 11 archives | only 10 in stack; oldest evicted |
| `confirm_prompt_first_press_doesnt_dispatch` | Review `A` once | no `SubmitReview` action emitted |
| `confirm_prompt_second_press_within_window_dispatches` | Review `A`, `A` within 1s | `SubmitReview { Approve }` emitted |
| `confirm_prompt_timeout_clears_state` | Review `A`, wait 3s, `A` | first press of second `A` re-enters pending; no double-dispatch |
| `confirm_prompt_different_key_clears` | Review `A`, then `D` | `D` re-enters pending; no Approve |
| `policy_off_skips_confirm` | config `destructive = "off"` + Review `A` | Approve emits on first press |
| `policy_confirm_applies_to_reversible_too` | config `destructive = "confirm"` + archive | first `x` shows confirm; second `x` archives |

## 6. Rollout

Phase 1 (this ADR's PR):
- Undo stack implementation, capped at 10 entries per view.
- ConfirmState wrapper for irrevocable actions.
- Config setting `[ui.destructive]` with default `undo`.
- Integration: SessionPicker archive/pin, IssueBrowser status keys, Dashboard Review.

Phase 2 (separate):
- Redo (`Ctrl+R` / `<C-r>`).
- Configurable per-action policy (`[ui.review.confirm = false]`).
- Persistent undo log (cross-session) — optional.

## 7. Migration / coexistence

This spec coexists with quick-fixes §4.3 (the `d`→`x` migration). Order:
1. Quick-fixes ships first, adding `x` alongside `d` as deprecation alias.
2. This ADR ships second, layering undo on top of the (already-aliased) `x` archive.

If a user is on `destructive = "off"`, behavior is identical to today. No regression.

## 8. Open questions

1. **Default policy**: `undo` vs `confirm`? Recommend `undo` — it's strictly less interruptive and the safety net is real (capped stack, per-view).
2. **Confirm prompt window**: 2 seconds? Recommend yes; align with `Ctrl+C` quit-confirm timing.
3. **Should Review submit always confirm**, even with `destructive = "undo"`? Reviews are remote-irrevocable, so yes — confirm regardless of policy. Override only via per-key opt-out.
4. **Capping the undo stack**: 10 entries per view? 50? Recommend 10 — keeps memory bounded; users undo recent, not week-old.
5. **Cross-view undo navigation**: should `u` in Dashboard see SessionPicker's stack? Reject — per-view isolation is simpler and matches users' mental model.
6. **Stream cancel as undo entry**: currently §4.9 of quick-fixes treats it as a one-shot hint. Should it also be on the undo stack ("undo cancel" = re-send the message)? Reject — the message was already cancelled at the network level; resending is a new request, not a revert.

## 9. Method note

This spec consumes the cross-check finding that destructive-action UX is "trust-breaking" (codex) without confirm/undo, while general text undo is deferrable (gemini). The split enables shipping the safety net for state mutations without coupling to the much larger work of integrating tui-textarea undo or building a custom edit history.

The confirm-prompt UX intentionally mirrors `Ctrl+C/Q` quit-confirm so users only learn one pattern.
