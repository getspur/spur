# `/clear` — TUI View Reset & Ready Affordance

**Status:** draft (design)
**Date:** 2026-04-20
**Owner:** TUI
**Related code:** `crates/spur-tui/src/app.rs`, `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-tui/src/action.rs`
**Builds on:**
- `07c71d2` (feat) — `/clear` made spur-local meta-command, retires brain via `NewSessionWithMessage{blocks:empty}`.
- `18fec81` (feat) — Orchestrator emits `SpurEventBody::BrainRetired{session, reason}` + lineage cascade.
- `4cfe528` (fix) — TUI consumes `BrainRetired` to null `brain_name` + clear auto-resume pointers.

## 1. Goal & scope

Make `/clear` visually wipe the session pane **immediately** on submit and surface a short "ready for next prompt" affordance. Today the user-visible transition only happens after the *next* prompt submission, because the view-replacement path in `app.rs:919-975` is driven by a new `spur_session_id` that is not minted until lazy-respawn.

### In scope

- Eager, client-side reset of the conversation state held inside the existing `SessionDetailView` when `Action::ClearSession` fires.
- A transient "ready" banner/status affordance rendered inside the session pane until the next prompt is submitted or the user starts typing.
- Defensive belt-and-suspenders reset on the `BrainRetired` event handler, gated on `reason == UserClear`, so non-`/clear` paths that reach the same retire event (future scripted/programmatic retires) cannot regress.
- A minimal, localized patch to the view-replacement path in `app.rs` to reparent any post-`/clear` InputBar draft onto the freshly-spawned session instead of writing it back under the retired session's metadata key (see §3.5).
- Unit tests covering the eager reset, the banner lifecycle, the `UserClear` defensive path, the `ResumeSwitch` / `Shutdown` non-interference, the header/status field reset, streaming-at-clear, and the draft carry-over (enumerated in §6).

### Explicitly out of scope

- Changes to orchestrator behavior, ACP event shapes, or lineage projection — this is purely a TUI view-reset fix.
- The lazy-respawn model itself: no eager brain-spawn on `/clear`; next prompt still triggers the spawn.
- Input-history persistence semantics. The input-history ring (Ctrl-P/N) is untouched. **Draft persistence is narrowly amended** at the single call site in `app.rs:932` — `force_flush_active_draft` is gated on `!detail.cleared` so a cleared view's post-`/clear` typing is not written under the retired session's key. All other `force_save_draft` call sites (debounce, picker-open boundary, quit-confirm) are unchanged.
- Any redesign of how `SessionDetailView` is owned by `App` (`Option<SessionDetailView>` shape stays).
- Dashboard activity-log formatting — the `Brain session retired (cleared)` entry added in `4cfe528` already covers that surface.

## 2. Root cause (condensed)

`Action::ClearSession` (`app.rs:1250-1267`) sends `UserInput::NewSessionWithMessage{blocks:empty}` and sets `BrainStatus::Idle`, but does not touch `self.session_detail`. The `SpurEventBody::BrainRetired` arm (`app.rs:856-878`, added in `4cfe528`) touches `brain_name` and `metadata_store.last_active_*` but also does not touch `self.session_detail`. The new-view path (`app.rs:919-975`) only fires when a new `spur_session_id` is observed, which under lazy-respawn is deferred until the next `UserInput::PromptText` causes `BrainSpawned` to emit. Result: a window between `/clear` submit and next prompt submit where the old view's trace buffer, ReactTrace, tool-depth stack, scroll cache, and DetailPane remain visible and authoritative.

## 3. Approach

### 3.1 Reset in place, do not drop the view

Add `SessionDetailView::reset_for_clear(&mut self)` that zeros the conversation-scoped state **while preserving** the infrastructure needed for the next prompt to work without reconstruction:

**Cleared:**
- `trace` (message log)
- `tool_depth` stack
- `ReactTrace` caches (compact Lines cache, wrap cache, scroll cache — match the same invalidation surface as `drop_cache`)
- `DetailPane` body and footer
- `TriggerDetector` state (via `trigger_detector.reset()` — already a public method, used at `session_detail.rs:657`)
- Auto-resume banner (never shown for a cleared session)
- `inline_protocols` (mirrors existing `invalidate_clears_inline_protocols_on_all_ready_states` invariant)
- **Header / status fields** (added after review — these render independently of `trace` and otherwise show retired-session state through a "cleared" pane):
    - `cost` (reset to `0.0`) — `session_detail.rs:32`
    - `started_at` (reset to `Instant::now()`) — `session_detail.rs:33`
    - `current_mode` (`None`; next brain repopulates via `SessionUpdate::CurrentModeUpdate`) — `session_detail.rs:36`
    - `context_used`, `context_size` (`None`; next brain repopulates via `SessionUpdate::UsageUpdate`) — `session_detail.rs:43,46`
    - `auth_error` (`None`) — `session_detail.rs:49`
- **Stream-state flags:**
    - `stream_in_flight` (`false`) — `session_detail.rs:82`
    - `cancelling_in_flight` (`false`) — `session_detail.rs:88`

  These two must clear so a `/clear` submitted while the brain is actively streaming does not leave the "Esc to stop" hint or a permanent `cancelling…` label in the new empty pane. (See §6 test 7 — the opencode-acp-flagged streaming-at-clear case.)

**Preserved:**
- `InputBar` (keeps focus, keeps seeded input history, keeps any typed-but-unsent draft) — **but see §3.5 on draft reparenting**.
- `last_persisted_draft` and `last_draft_change_at` are **cleared** (set to empty / `None`) so the draft-debounce path does not write the carryover text back under the retired session's metadata key. This is the local piece of §3.5.
- Resolved `agent_cfg` (still valid; next brain will use the same agent unless the user explicitly switches).
- `edit_mode`.
- `mermaid_picker` / render-picker settings (if feature-gated on).
- `worker_snapshot` + `known_worker_names` — **config-derived, not session-derived** (confirmed by reviewer against `app.rs:579-595` and `session_detail.rs:121-127,1021-1035`), so preservation is safe.
- `command_registry` — the agent-advertised portion was merged from the (now retired) agent, but it is moot during the cleared window (no agent to dispatch to) and the next `BrainSpawned` constructs a fresh registry in a fresh view. Preservation avoids gratuitous churn.

**Session id:** stays pointing at the retired id. This is deliberate: the next `BrainSpawned` event carries a new `spur_session_id`, and the existing `needs_new` check at `app.rs:922` (`detail.session_id() != session`) will then construct a fresh `SessionDetailView` the normal way. However, the interaction with `force_flush_active_draft` at `app.rs:932` is NOT a no-op for a cleared view — see §3.5.

### 3.2 Ready banner

Add a `ReadyBanner` to `SessionDetailView` — a transient, in-pane notice rendered where the trace would be when the trace is empty. Text:

> `✨ Session cleared — your next prompt starts a fresh brain.`

**Lifecycle:**
- Set by `reset_for_clear()`.
- Cleared on: (a) first `BrainSpawned` that causes view replacement (new view has no banner), OR (b) first real trace entry added to the current view (defensive — the view stays across `BrainSpawned` only if session ids somehow match, which they won't under lazy-respawn). No time-based dismissal.

Rendered style: one line, dim/italic, above the (empty) trace area. Reuse existing banner-rendering primitives where present (the auto-resume banner at `app.rs:969` sets precedent — locate its render path and mirror it for `ReadyBanner`).

### 3.3 Action arm wiring

**`Action::ClearSession` (`app.rs:1250`)** — add one call after the existing Idle/NewSessionWithMessage/sync:

```rust
if let Some(ref mut detail) = self.session_detail {
    detail.reset_for_clear();
}
```

Leave `brain_status`, `user_input_tx` send, and `sync_brain_status` call untouched.

### 3.4 Defensive guard on `BrainRetired`

**`SpurEventBody::BrainRetired` arm (`app.rs:856-878`)** — extend with a `UserClear`-gated view reset:

```rust
if matches!(reason, BrainRetireReason::UserClear) {
    if let Some(ref mut detail) = self.session_detail {
        detail.reset_for_clear();
    }
}
```

Gated specifically on `UserClear` because:
- `ResumeSwitch` already loads the next brain via `ResumeSession` → new `BrainSpawned` with different `spur_session_id` → normal view replacement path at `app.rs:919-975` handles it; resetting here would briefly blank the new view mid-load.
- `Shutdown` is terminal; view reset is moot.

This arm is idempotent against §3.3 — calling `reset_for_clear()` twice on an already-cleared view is a no-op.

**Event-ordering safety (confirmed by reviewer against `event_funnel.rs:1-10,50-59` and TUI lag handling at `app.rs:2092-2161`):** the live event funnel delivers retained events in monotonic `SpurEvent.seq` order and the TUI lag handler drops older events rather than reordering them, so a `BrainRetired{UserClear}` cannot overtake a later `BrainSpawned` on the live path. Hence this arm does not need to consult `event.seq` — but that safety is a property of the funnel/broadcast contract, not of the handler. If the funnel contract weakens, this arm must be revisited.

`Shutdown` and `ResumeSwitch` reasons fall through with no action: `Shutdown` is terminal; `ResumeSwitch` is handled by the in-flight `ResumeSession` → `BrainSpawned` path at `app.rs:919-975`.

### 3.5 Draft reparenting across reset

**Problem (reviewer-flagged [CONCERN]):** `SessionDetailView::force_save_draft` (`session_detail.rs:275-287`) emits `Action::SaveDraft { session_id: self.session_id.0.clone(), draft }`. After §3.1, the view's `session_id` still points at the retired session. When the next `BrainSpawned` arrives, the replacement path at `app.rs:926-975` calls `force_flush_active_draft` (`app.rs:1811-1818`) — which would save whatever the user typed *after* `/clear` into the retired session's metadata entry. The fresh view then calls `restore_draft` (`app.rs:947-948`) keyed on the new `spur_session_id` and finds nothing. Net: the user's post-`/clear` typing is lost to the wrong session.

**Resolution — carry-over via a `cleared` marker + source-level save gating:**

1. Add `cleared: bool` field to `SessionDetailView`, default `false`.
2. `reset_for_clear` sets `cleared = true` and also resets `last_persisted_draft` to `""` and `last_draft_change_at` to `None`.
3. **Gate draft emission at the source (critical).** Both `force_save_draft` (`session_detail.rs:275-287`) and `draft_save_action` (`session_detail.rs:250-266`, called from `App::tick`) must return `None` early when `self.cleared == true`. This is the load-bearing guard — the debounce path runs on every keystroke independently of the replacement path, so gating only at the replacement-path call site is insufficient. A cleared view's `session_id` is opaque and any `Action::SaveDraft` keyed on it would corrupt the retired session's metadata.
4. In the view-replacement branch (`app.rs:919-975`, `needs_new = true`), before constructing the new view:
   - If `old_detail.is_cleared()`, capture `old_detail.input_bar_text()` into a local `carryover: String` (owned — `input_bar_text` returns `String`).
   - Call `force_flush_active_draft` unconditionally — the source-level guard in step 3 makes it a no-op for a cleared view, so no extra call-site gating is needed here.
5. After the new view is constructed and `restore_draft(&entry.draft)` has run:
   - If `!carryover.is_empty()`, call `new_view.restore_draft(&carryover)`. This overwrites the metadata-restored draft (normally empty on a freshly-minted `spur_session_id`) and marks `last_persisted_draft = carryover` so the next debounce tick is a no-op.
6. After replacement, the old view is dropped as usual.

**Invariant:** once a view is `cleared`, no `Action::SaveDraft` keyed on its `session_id` may be emitted by any path — force-flush, debounce tick, or anything else. The cleared view hosts the banner, InputBar, and carry-over text only; it is metadata-inert.

**Why source-level gating, not call-site gating?** `SessionDetailView` emits `SaveDraft` from two methods (`force_save_draft`, `draft_save_action`). The replacement path only invokes one; the other runs on the tick loop every frame while the user is typing post-`/clear`. Guarding only the call site leaves the tick-path leak open. Guarding at the source closes both.

**Why not clear the draft at reset time?** The user may have typed the prompt they want the new brain to see *before* hitting Enter on `/clear` (or between `/clear` and the next prompt). Dropping it would be a UX regression.

**Why not migrate by changing the view's `session_id` in place?** That would break the `needs_new` check at `app.rs:922` and the existing replacement invariants tested elsewhere. The carry-over approach localizes the fix to the specific replacement path.

## 4. Data flow (after fix)

```
User types "/clear" + Enter
  → CommandRegistry matches spur-local meta entry
  → Dispatch::SpurLocal(Action::ClearSession)
  → Action::ClearSession arm:
       - brain_status = Idle
       - session_detail.reset_for_clear()   ← NEW: eager wipe + banner
       - send UserInput::NewSessionWithMessage{blocks:empty}
       - sync_brain_status; dirty = true
  → (user sees: empty pane, "✨ Session cleared…" banner, InputBar still focused)
  → Orchestrator retires brain asynchronously:
       - aborts notification pump (100ms grace)
       - closes cost ledger
       - emits SpurEventBody::BrainRetired{session, reason=UserClear}
  → App.handle_spur_event(BrainRetired):
       - brain_name = None
       - metadata_store.clear_last_active_full() + save()
       - session_detail.reset_for_clear()    ← NEW defensive, idempotent
  → (user types next prompt into the preserved InputBar, hits Enter)
  → UserInput::PromptText flows → lazy-spawn new brain
  → BrainSpawned with new spur_session_id
  → app.rs:922 needs_new = true → replacement branch:
       - old_detail.cleared == true → capture carryover = input_bar.text()
       - SKIP force_flush_active_draft (would corrupt retired session's draft)
       - build new view, restore_draft from metadata (empty for new id)
       - restore_draft(carryover) → new InputBar pre-filled with user's text
  → (banner gone, trace empty, new brain streams response)
```

## 5. Files touched

| File | Change |
|---|---|
| `crates/spur-tui/src/views/session_detail.rs` | Add `reset_for_clear(&mut self)`; add `ReadyBanner` field + render path; add `cleared: bool` field + `input_bar_text(&self) -> &str` accessor for the carry-over read at replacement time. |
| `crates/spur-tui/src/app.rs` | `Action::ClearSession` arm: call `reset_for_clear`. `BrainRetired` arm: gated defensive call. Replacement path at `app.rs:926-975`: carry-over read + conditional `force_flush_active_draft` skip. |
| `crates/spur-tui/src/app.rs` (tests) | Extend `brain_retired_tests` module with the new tests listed in §6. |

No changes to `action.rs`, orchestrator, ACP types, lineage projection, `CommandRegistry`, or `SpurLocalSource`.

## 6. Tests

Added to `#[cfg(test)] mod brain_retired_tests` in `app.rs`:

1. **`clear_session_resets_session_detail_in_place`** — construct `App` with an active `SessionDetailView` containing a non-empty trace; dispatch `Action::ClearSession`; assert `session_detail.is_some()`, `trace.is_empty()`, `ready_banner.is_some()`, `session_id()` unchanged, `cleared == true`.
2. **`clear_session_preserves_input_bar_contents`** — user has typed a partial prompt into `InputBar`; `Action::ClearSession`; assert `InputBar` text is preserved (not wiped).
3. **`brain_retired_user_clear_resets_view_defensively`** — bypass `Action::ClearSession`; emit `BrainRetired{reason=UserClear}` directly; assert view reset fired.
4. **`brain_retired_resume_switch_does_not_reset_view`** — emit `BrainRetired{reason=ResumeSwitch}`; assert `session_detail` trace is **not** cleared (the in-flight resume owns the transition).
5. **`clear_session_banner_cleared_on_next_brain_spawn`** — post-`ClearSession`, emit `BrainSpawned` with a new `spur_session_id`; assert the fresh `SessionDetailView` has no `ready_banner` (it's a new view, not the reset one), and `cleared == false` on the new view.
6. **`reset_for_clear_is_idempotent`** — call `reset_for_clear` twice; second call is a no-op (ensures the Action + BrainRetired double-call path is safe).
7. **`reset_for_clear_clears_header_status_fields`** (added from review) — seed non-default values into `cost`, `started_at`, `current_mode`, `context_used`, `context_size`, `auth_error`, `stream_in_flight`, `cancelling_in_flight`; call `reset_for_clear`; assert each is zeroed / `None` / `false` per §3.1.
8. **`clear_while_streaming_does_not_panic_and_resets_flags`** (added from review — opencode-acp NIT #3) — set `stream_in_flight = true` and `cancelling_in_flight = false`; dispatch `Action::ClearSession` mid-stream; assert no panic, `stream_in_flight == false`, trace cleared, banner shown.
9. **`brain_retired_shutdown_does_not_panic`** (added from review — opencode-acp NIT #4) — emit `BrainRetired{reason=Shutdown}` with an active `SessionDetailView`; assert no panic, view state unchanged (Shutdown path is a no-op in the arm, not a reset).
10. **`draft_carryover_across_clear_to_new_brain_spawn`** (added from review — codex-acp CONCERN #2) — active session A has `InputBar` text `"draft-A"`; user submits `/clear`; user types `"post-clear-prompt"` into `InputBar`; emit `BrainSpawned` with new `spur_session_id` B; assert:
    - metadata entry for A's `session_id` was NOT overwritten with `"post-clear-prompt"` (force_flush skipped);
    - new view for B has `InputBar` text `"post-clear-prompt"` (carryover applied);
    - new view's `last_persisted_draft == "post-clear-prompt"` (the carryover's `restore_draft` marked it persisted, so the next debounce tick does not re-save it under B).
11. **`draft_carryover_empty_is_noop`** (edge case) — `/clear` with empty `InputBar`; `BrainSpawned`; assert new view's `InputBar` is empty (no-op carryover) and metadata for neither session was written.

### 3.6 Error handling on `/clear` send failure

`Action::ClearSession` sends `UserInput::NewSessionWithMessage{blocks:empty}` over a bounded channel. The pre-revision code uses `let _ = tx.try_send(...)`, silently dropping on failure. Post-`reset_for_clear`, a silent drop creates a **ghost-cleared state**: the pane is visibly wiped and the ready banner is shown, but the brain is never actually retired.

**Resolution:**

1. Reorder: call `tx.try_send(...)` **before** `reset_for_clear`. Only call `reset_for_clear` if the send succeeded.
2. On `Err`, emit `tracing::error!(err = ?e, "Action::ClearSession: user_input tx send failed — brain not retired; view NOT reset to avoid ghost-cleared state")`. The user sees nothing changed and will retry — which is the correct affordance.
3. Also set `brain_status = Idle` only on send success: an `Err` leaves the brain active, so the status line should reflect that.

This trades a very rare edge case (channel full/closed) for a correctness guarantee: the UI state and brain state stay consistent.

## 7. Risks & open questions

- **R1 (resolved — see §3.5):** Draft ownership across the retired `session_id` is handled by the `cleared` marker + carry-over pattern. Reviewer (codex-acp) flagged the naive "preserve InputBar" approach as a [CONCERN] because `force_flush_active_draft` would save post-`/clear` typing under the retired session's key. §3.5 is the closed fix. Tests 10 and 11 in §6 lock the behavior.
- **R2: Banner wording.** Copy may change; `✨` emoji may be gated by a terminal-capability check if spur has one. Implement with a const string, revisit if a11y/terminal-compat concerns surface.
- **R3: Render ordering.** The ready banner and the auto-resume banner occupy the same "top-of-empty-trace" slot. They are mutually exclusive (auto-resume fires on `BrainSpawned` matching `last_active`; `/clear` clears `last_active` before any such match can occur). But encode the invariant explicitly in the render path — prefer `ReadyBanner` if both are somehow set, and log a warning.
- **R4: `reset_for_clear` scope drift.** Future fields added to `SessionDetailView` must be classified as "cleared" or "preserved". Add a comment on `reset_for_clear` enumerating the policy so new fields get a deliberate decision.

## 8. Non-goals reaffirmed

- No change to when the brain is actually spawned (still lazy on next prompt).
- No new events on the wire.
- No change to `/clear`'s command-registry shadowing behavior from `07c71d2`.
- No attempt to address R1–R8 from the `4cfe528` commit message (those are orthogonal retire-path hardening tasks).
