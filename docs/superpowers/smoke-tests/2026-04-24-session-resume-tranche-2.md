# Manual smoke test: Session resume — Tranche 2 (optimistic navigation)

**Associated plan:** `docs/superpowers/plans/2026-04-24-session-resume-tranche-2-optimistic-nav.md`
**Spec:** `docs/superpowers/specs/2026-04-24-session-resume-optimistic-nav-design.md`

**Why a manual test:** several milestones in Tranche 2's resume pipeline (`BrainConnecting`, `SessionLoading`, `SessionLoaded`) require a successful ACP spawn to emit. The codebase has no `MockAgentConnection`, so these end-to-end paths are not covered by automated integration tests. This guide walks a human through exercising them.

**Pre-reqs:**
- Clean working tree at HEAD of `main` containing Tranche 2 commits.
- `cargo build --release` (or `cargo build --workspace`) succeeds.
- At least one pre-existing session stored locally.
- A working brain configuration (Claude Code, Codex, or similar) spawnable by the orchestrator.

---

## Scenario 1 — Warm resume (common path)

1. Launch spur in interactive mode.
2. Exchange one prompt/response with the active session to confirm it's healthy.
3. Open the session picker (the key depends on your binding — typically a slash command or a binding visible in the status bar).
4. Press Enter on a different existing session row.

**Expected:**
- The picker dismisses in a single frame. No "Resuming…" spinner on the picker.
- `SessionDetail` briefly shows **"Retiring previous session…"** (the old brain is being torn down).
- Then **"Connecting to <brain_name>…"** if a cold spawn is needed (or skipped if the ACP connection was reused).
- Then **"Loading session history…"** for a moment.
- Then the full session UI renders, history replayed.

**Pass:** ≤ 5 seconds wall-clock from Enter to fully-loaded session; no stuck label; no visible glitch on the picker.

**Fail signals:** picker shows its own spinner; SessionDetail stuck on "Retiring…" or "Loading…" past ~10 s; any panic trace.

---

## Scenario 2 — Cold resume (no active brain yet)

1. Launch spur fresh, land on the dashboard without an active session.
2. Open the session picker.
3. Press Enter on any existing session.

**Expected:**
- Picker dismisses immediately.
- `SessionDetail` starts in the initial state. Because there's no prior brain to retire, the orchestrator emits **no** `SessionRetireStart` or `SessionRetireComplete` events (Tranche 2 Task 2 pairs them strictly).
- The first visible label is either **"Retiring previous session…"** (the initial default, if no event arrives before render) briefly, or directly **"Connecting to <brain_name>…"**.
- Then **"Loading session history…"** → fully loaded.

**Pass:** No hang; transition from initial Retiring label to Connecting happens within 1–2 frames once `BrainConnecting` fires.

**Fail signals:** SessionDetail stuck on "Retiring…" forever — would indicate the milestone events aren't reaching the view (regression in Task 6 dispatch wiring).

---

## Scenario 3 — Failure path

1. Break your brain configuration temporarily. Easiest: point the brain's `binary_path` in the spur config to a non-existent file, or kill the binary. (Back up the config first.)
2. Open the picker and press Enter on a session.

**Expected:**
- Picker dismisses.
- SessionDetail briefly shows a Retiring/Connecting label.
- Then transitions to a **Failed** state rendering the error message (e.g. "Brain agent 'claude-code' not found in registry" or a spawn error).
- Pressing Esc returns to the dashboard/picker.

**Pass:** Clear error visible in the UI; no frozen spinner; user can navigate away.

**Fail signals:** UI stays in a loading label with no error displayed (would indicate `BrainError` isn't correlating to this session's `LoadState::Failed` — check the Tranche 1 fix at `orchestrator.rs:1358`/`:1431`).

**Restore your brain config before proceeding.**

---

## Scenario 4 — Rapid session switching

1. From a loaded session, open the picker.
2. Press Enter on session A.
3. IMMEDIATELY open the picker again (before A finishes loading if possible) and press Enter on session B.

**Expected:**
- `SessionDetail` ultimately reflects session B, not A.
- Any milestone events from A arriving late are ignored because `SessionDetailView.session_id` is now B (the session-id guard in `apply_milestone_event` drops them).
- History in the view is B's history, not A's.

**Pass:** No stale A content visible once B loads.

**Fail signals:** A's messages or labels appearing in B's view — would indicate a session-id guard bug in Task 5.

---

## Regression checklist (must NOT happen)

- [ ] Picker stuck showing a spinner after Enter (we deleted `resuming` in Task 3).
- [ ] SessionDetail stuck on "Retiring…" for more than ~5 seconds on cold resume.
- [ ] "Connecting to …" with an empty brain name (Task 2 review fix: uses `self.selected_brain_name(...)` with default fallback).
- [ ] `BrainError` arriving and not updating the UI (LoadState must transition to Failed via Tranche 1's faithful session-id fix).
- [ ] Old session's message history leaking into new session's `SessionDetail` view.

---

## Reporting issues

If any scenario fails, capture:

1. The scenario number and which step failed.
2. The UI state at the failure point (screenshot or text describe).
3. `~/.cache/spur/logs` or the current session log if one is written.
4. The last ~20 lines of `spur` stderr/tracing output if running in verbose mode.

Cross-reference the Tranche 2 plan sections corresponding to the failing scenario:
- Scenario 1/2 failure in Retiring/Connecting labels → Task 2 (milestone event emission) or Task 5 (LoadState projection).
- Scenario 3 failure → Tranche 1 Task 3 (BrainError session correlation) or Task 5 (Failed render branch).
- Scenario 4 failure → Task 5 (session-id guard in `apply_milestone_event`).
