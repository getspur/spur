# SPUR Bot Resumed-Ready State Machine Hardening Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)` when issue tracking is configured.
> Each task is scoped so it can become a beads issue with explicit dependencies.
>
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Source spec:** `docs/superpowers/specs/2026-04-23-spur-bot-resumed-ready-state-machine-hardening-design.md`
**Design epic:** unavailable in this workspace (`spur-mcp` issue tracker is not configured)

**Goal:** Harden `spur-bot` resumed-session binding so stale resumed-ready events cannot overwrite newer `/resume` intent, cannot bind the lobby, and cannot leave stale `session_threads` routes behind.

**Architecture:** Keep the ACP protocol and persistence model unchanged, but refactor `crates/spur-bot/src/runtime.rs` around three local invariants: same-topic pending resume supersession, strict resumed-ready resolution, and a shared `bind_active_session(...)` commit helper for both resumed and fresh ready paths. Drive the change with `runtime_flow.rs` regressions that pin both resumed-ready arrival orderings, no-pending-target drop behavior, and route replacement correctness.

**Tech Stack:** Rust 2021, `tokio`, `spur-acp`, `spur-core`, existing `crates/spur-bot/tests/runtime_flow.rs` integration-style runtime tests, `cargo test`

---

## File Map

### Existing files to modify

| File | Responsibility in this plan |
|---|---|
| `crates/spur-bot/src/runtime.rs` | Add `supersede_pending_resume`, `resolve_resumed_ready`, and `bind_active_session`; route both `AgentSessionReady` branches through the hardened state machine |
| `crates/spur-bot/tests/runtime_flow.rs` | Add resumed-ready ordering tests, no-pending-target drop coverage, stale-route replacement coverage, and fresh-path non-regression assertions |

### No new files expected

This remediation should stay contained to the runtime and its existing regression
test file. If implementation pressure suggests a new helper module, stop and
re-check the spec; that is likely unnecessary scope growth.

## Execution Shape

This plan is intentionally sequential and mirrors the approved execution model
from the design spec.

For every task:

- **Justification worker:** `claude-code`
- **Implementation worker:** `codex`
- **Review worker:** `kimi`

The worker roles are constant across tasks; what changes is the file scope and
acceptance criteria of each slice.

---

### Task 1: Evict Superseded Same-Topic Pending Resume State

**Task ID:** `task-1-supersede-pending-resume`

**Files:**
- Modify: `crates/spur-bot/src/runtime.rs`
- Modify: `crates/spur-bot/tests/runtime_flow.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `/resume Y` in a topic removes stale `pending_resume` entries that still point to that same topic before inserting the new target
- [ ] `AgentSessionReady(Y)` followed by stale `AgentSessionReady(X)` no longer leaves the topic bound to `X`
- [ ] The new regression fails before the fix and passes after it

**Suggested Worker:** `codex`
**Justification Worker:** `claude-code`
**Review Worker:** `kimi`

**Scope Boundary:**
- IN scope: same-topic `/resume` bookkeeping in `runtime.rs`, one focused ordering regression in `runtime_flow.rs`
- OUT of scope: shared commit helper extraction, no-pending-target drop behavior, fresh-session path refactors
- If you discover you need to touch files outside the two paths above, emit `scope_drift` immediately

**Implementation:**

- [ ] **Step 1: Add a failing regression for the `Y-ready then stale-X-ready` ordering**

Add to `crates/spur-bot/tests/runtime_flow.rs`:

```rust
#[tokio::test]
async fn same_topic_resume_supersession_keeps_new_binding_when_old_ready_arrives_late() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-X".into(), "kimi".into());

    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-Y")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_Y".into()),
                acp_session_id: "acp-Y".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_X".into()),
                acp_session_id: "acp-X".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::Active { acp_session_id, .. } if acp_session_id == "acp-Y"
        ),
        "late ready for X must not overwrite the newer /resume Y binding; binding was {:?}",
        record.binding
    );
}
```

- [ ] **Step 2: Run the targeted regression and verify it fails on current code**

Run:

```bash
cargo test -p spur-bot --test runtime_flow same_topic_resume_supersession_keeps_new_binding_when_old_ready_arrives_late -- --nocapture
```

Expected: FAIL because `/resume acp-Y` leaves the stale `"acp-X" -> Topic77`
entry in `pending_resume`, allowing the late `AgentSessionReady(X)` to overwrite
the binding.

- [ ] **Step 3: Commit the failing regression**

Run:

```bash
git add crates/spur-bot/tests/runtime_flow.rs
git commit -m "test(spur-bot): S1.a add same-topic resume supersession regression"
```

- [ ] **Step 4: Implement same-topic supersession cleanup in the `/resume` path**

Add a focused helper in `crates/spur-bot/src/runtime.rs` near
`evict_pending_new(...)`:

```rust
fn supersede_pending_resume(&mut self, key: &ThreadKey) {
    self.pending_resume.retain(|_, pending_key| pending_key != key);
}
```

Then call it in `BotCommand::Resume { session_id }` immediately before the new
insert:

```rust
self.pending_inputs.remove(&key);
self.evict_pending_new(&key);
self.supersede_pending_resume(&key);

handle
    .send_command(spur_core::InteractiveInput::ResumeSession {
        session_id: session_id.clone(),
    })
    .await?;
self.pending_resume.insert(session_id.clone(), key.clone());
```

- [ ] **Step 5: Re-run the targeted regression**

Run:

```bash
cargo test -p spur-bot --test runtime_flow same_topic_resume_supersession_keeps_new_binding_when_old_ready_arrives_late -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit the supersession fix**

Run:

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/tests/runtime_flow.rs
git commit -m "fix(spur-bot): S1.b evict superseded pending resumes"
```

---

### Task 2: Resolve Resumed Ready Only From Current Pending Intent

**Task ID:** `task-2-resolve-resumed-ready`

**Files:**
- Modify: `crates/spur-bot/src/runtime.rs`
- Modify: `crates/spur-bot/tests/runtime_flow.rs`

**Depends on:** `task-1-supersede-pending-resume`

**Acceptance Criteria:**
- [ ] `AgentSessionReady { resumed: true }` commits only when `pending_resume` still maps that ACP id to a topic that is still `RestorePending { same_acp_id }`
- [ ] `AgentSessionReady(X)` arriving before `AgentSessionReady(Y)` after `/resume Y` does not activate `X`
- [ ] A resumed-ready event with no pending target returns `(None, vec![])` and does not bind the lobby

**Suggested Worker:** `codex`
**Justification Worker:** `claude-code`
**Review Worker:** `kimi`

**Scope Boundary:**
- IN scope: resumed-ready resolution logic in `handle_spur_event`, two resumed-event regressions
- OUT of scope: shared active-bind helper extraction and fresh-path commit unification
- If you discover this task needs to rewrite the fresh-session FIFO selector, emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Add failing regressions for stale-old-first and no-pending-target drop**

Add to `crates/spur-bot/tests/runtime_flow.rs`:

```rust
#[tokio::test]
async fn same_topic_resume_supersession_ignores_old_ready_until_new_ready_arrives() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-X".into(), "kimi".into());

    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-Y")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_X".into()),
                acp_session_id: "acp-X".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::RestorePending { acp_session_id, .. } if acp_session_id == "acp-Y"
        ),
        "old ready for X must not activate the topic while /resume Y is pending; binding was {:?}",
        record.binding
    );
    assert!(record.live_session.is_none());
}

#[tokio::test]
async fn late_resumed_ready_without_pending_target_is_ignored() {
    let (mut runtime, _handle, _user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-77".into(), "kimi".into());

    let (key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur-late".into()),
                acp_session_id: "acp-late".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    assert!(key.is_none(), "late resumed-ready with no pending target must not route anywhere");
    assert!(renders.is_empty(), "late resumed-ready with no pending target must not render");

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::RestorePending { acp_session_id, .. } if acp_session_id == "acp-77"
        ),
        "late unrelated resumed-ready must not mutate existing restore state; binding was {:?}",
        record.binding
    );
}
```

- [ ] **Step 2: Run the targeted regressions and verify they fail**

Run:

```bash
cargo test -p spur-bot --test runtime_flow same_topic_resume_supersession_ignores_old_ready_until_new_ready_arrives -- --nocapture
cargo test -p spur-bot --test runtime_flow late_resumed_ready_without_pending_target_is_ignored -- --nocapture
```

Expected: FAIL because the current resumed path falls back through
`pending_resume.remove(...).or_else(|| self.session_threads.get(&session).cloned())`
and then lobby defaulting.

- [ ] **Step 3: Commit the failing regressions**

Run:

```bash
git add crates/spur-bot/tests/runtime_flow.rs
git commit -m "test(spur-bot): S1.c add strict resumed-ready regressions"
```

- [ ] **Step 4: Implement strict resumed-ready resolution**

Add this helper to `crates/spur-bot/src/runtime.rs`:

```rust
fn resolve_resumed_ready(&mut self, acp_session_id: &str) -> Option<ThreadKey> {
    let key = self.pending_resume.remove(acp_session_id)?;
    let still_expects = self.threads.get(&key).is_some_and(|record| {
        matches!(
            &record.binding,
            BindingState::RestorePending {
                acp_session_id: expected,
                ..
            } if expected == acp_session_id
        )
    });
    still_expects.then_some(key)
}
```

Use it in `handle_spur_event(...)`:

```rust
let key = if resumed {
    let Some(key) = self.resolve_resumed_ready(&acp_session_id) else {
        return Ok((None, vec![]));
    };
    key
} else {
    // existing FIFO selection logic
};
```

Do **not** preserve the current resumed-event fallback to `session_threads` or
lobby binding.

- [ ] **Step 5: Re-run the targeted regressions**

Run:

```bash
cargo test -p spur-bot --test runtime_flow same_topic_resume_supersession_ignores_old_ready_until_new_ready_arrives -- --nocapture
cargo test -p spur-bot --test runtime_flow late_resumed_ready_without_pending_target_is_ignored -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit the resolution guard**

Run:

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/tests/runtime_flow.rs
git commit -m "fix(spur-bot): S1.d require exact pending target for resumed ready"
```

---

### Task 3: Centralize Live-Bind Commit And Replace Stale Session Routes

**Task ID:** `task-3-bind-active-session`

**Files:**
- Modify: `crates/spur-bot/src/runtime.rs`
- Modify: `crates/spur-bot/tests/runtime_flow.rs`

**Depends on:** `task-2-resolve-resumed-ready`

**Acceptance Criteria:**
- [ ] Both `AgentSessionReady { resumed: true }` and `AgentSessionReady { resumed: false }` commit through one helper
- [ ] Rebinding a topic from a stale live session to a new live session removes the old `session_threads` route
- [ ] The new live session route remains present and routable after replacement

**Suggested Worker:** `codex`
**Justification Worker:** `claude-code`
**Review Worker:** `kimi`

**Scope Boundary:**
- IN scope: active-binding commit logic and route replacement assertions
- OUT of scope: `/resume` command parsing, persistence schema changes, prompt routing changes outside `session_threads`
- If the implementation needs a new module or broader router changes, emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Add a failing route-replacement regression**

Add to `crates/spur-bot/tests/runtime_flow.rs`:

```rust
#[tokio::test]
async fn resumed_rebind_replaces_stale_session_route_with_new_live_route() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-X".into(), "kimi".into());

    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-Y")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_X".into()),
                acp_session_id: "acp-X".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_Y".into()),
                acp_session_id: "acp-Y".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    let (old_key, _) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete {
                session: spur_acp::SessionId("spur_X".into()),
            },
        ))
        .unwrap();
    assert!(old_key.is_none(), "stale session route for X must be removed after Y binds");

    let (new_key, _) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete {
                session: spur_acp::SessionId("spur_Y".into()),
            },
        ))
        .unwrap();
    assert_eq!(
        new_key,
        Some(spur_bot::state::ThreadKey {
            chat_id: 42,
            message_thread_id: Some(77),
        }),
        "the committed Y session must still route to topic 77"
    );
}
```

- [ ] **Step 2: Run the route regression and verify it fails**

Run:

```bash
cargo test -p spur-bot --test runtime_flow resumed_rebind_replaces_stale_session_route_with_new_live_route -- --nocapture
```

Expected: FAIL because the current inline commit block inserts the new route but
never removes any stale live route already owned by the topic.

- [ ] **Step 3: Commit the failing route regression**

Run:

```bash
git add crates/spur-bot/tests/runtime_flow.rs
git commit -m "test(spur-bot): S1.e add stale route replacement regression"
```

- [ ] **Step 4: Extract `bind_active_session(...)` and route both ready branches through it**

Add this helper to `crates/spur-bot/src/runtime.rs`:

```rust
fn bind_active_session(
    &mut self,
    key: &ThreadKey,
    session: spur_acp::SessionId,
    acp_session_id: String,
    brain: String,
) {
    if let Some(existing_key) = self.session_threads.get(&session).cloned() {
        debug_assert_eq!(
            existing_key, *key,
            "session_threads collision: session already routed to another topic"
        );
    }

    if let Some(record) = self.threads.get_mut(key) {
        if let Some(old_session) = record.live_session.clone() {
            self.session_threads.remove(&old_session);
        }

        record.binding = BindingState::Active {
            session: session.clone(),
            acp_session_id: acp_session_id.clone(),
            brain: brain.clone(),
        };
        record.live_session = Some(session.clone());
        record.acp_session_id = Some(acp_session_id);
        record.brain = Some(brain);
    }

    self.session_threads.insert(session, key.clone());
}
```

Then replace the duplicated inline commit block in `handle_spur_event(...)`
with:

```rust
self.bind_active_session(&key, session.clone(), acp_session_id.clone(), brain.clone());
self.output_buffers.remove(&session);
self.state_store.save(&self.persistable_state())?;
```

Use the same helper for both resumed and fresh ready once the target topic has
been selected.

- [ ] **Step 5: Run the route regression and fresh-bind sanity test**

Run:

```bash
cargo test -p spur-bot --test runtime_flow resumed_rebind_replaces_stale_session_route_with_new_live_route -- --nocapture
cargo test -p spur-bot --test runtime_flow agent_session_ready_commits_binding_and_persists -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit the shared commit helper**

Run:

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/tests/runtime_flow.rs
git commit -m "fix(spur-bot): S1.f unify live bind commit and route replacement"
```

---

### Task 4: Lock Down Non-Regression For Fresh And Restore Flows

**Task ID:** `task-4-regression-sweep`

**Files:**
- Modify: `crates/spur-bot/tests/runtime_flow.rs`
- Modify: `crates/spur-bot/src/runtime.rs` only if a small comment or helper-name adjustment is required to keep the tests readable

**Depends on:** `task-3-bind-active-session`

**Acceptance Criteria:**
- [ ] The shared commit helper does not break normal fresh-session binding
- [ ] The restore-before-send flow still queues `ResumeSession` and flushes the pending message only after the valid ready event
- [ ] The stale-fresh-ready regression remains green after the resumed-path hardening

**Suggested Worker:** `codex`
**Justification Worker:** `claude-code`
**Review Worker:** `kimi`

**Scope Boundary:**
- IN scope: regression hardening and verification only
- OUT of scope: new product behavior, new helper APIs, unrelated RCA cleanups
- If the broad regression sweep forces unrelated runtime changes, stop and emit `scope_drift`

**Implementation:**

- [ ] **Step 1: Strengthen fresh-path routing coverage**

Extend `agent_session_ready_commits_binding_and_persists` in
`crates/spur-bot/tests/runtime_flow.rs` with a routing assertion:

```rust
let (key, _) = runtime
    .handle_spur_event(spur_acp::SpurEvent::now(
        spur_acp::SpurEventBody::TurnComplete {
            session: spur_acp::SessionId("spur_1".into()),
        },
    ))
    .unwrap();

assert_eq!(
    key,
    Some(spur_bot::state::ThreadKey {
        chat_id: 42,
        message_thread_id: Some(77),
    }),
    "fresh AgentSessionReady must still install a live route for turn completion"
);
```

- [ ] **Step 2: Run the focused non-regression tests**

Run:

```bash
cargo test -p spur-bot --test runtime_flow agent_session_ready_commits_binding_and_persists -- --nocapture
cargo test -p spur-bot --test runtime_flow stale_fresh_ready_does_not_reactivate_rebound_topic -- --nocapture
cargo test -p spur-bot --test runtime_flow restore_pending_plain_text_queues_resume_then_message -- --nocapture
```

Expected: PASS if the helper unification preserved the pre-existing fresh and
restore behavior; FAIL if the refactor broke route installation or delayed
message flush semantics.

- [ ] **Step 3: Make the minimal code adjustment only if one of the non-regression tests fails**

If you need a tiny adjustment, keep it local to the helper callsites. The
intended shape is:

```rust
let key = if resumed {
    let Some(key) = self.resolve_resumed_ready(&acp_session_id) else {
        return Ok((None, vec![]));
    };
    key
} else {
    // existing FIFO selection logic
};

self.bind_active_session(&key, session.clone(), acp_session_id.clone(), brain.clone());
self.output_buffers.remove(&session);
```

Do not reintroduce the resumed fallback path to make these tests pass.

- [ ] **Step 4: Run the full targeted runtime suite**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit the final regression hardening**

Run:

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/tests/runtime_flow.rs
git commit -m "test(spur-bot): S1.g lock down resumed-ready hardening regressions"
```

---

## Spec Coverage Check

- Same-topic supersession behavior: covered by Task 1
- Strict resumed-ready resolution: covered by Task 2
- Shared `bind_active_session(...)` commit path and route cleanup: covered by Task 3
- Fresh-path and restore-flow non-regression after helper unification: covered by Task 4

No spec requirement is intentionally left without a task.

## DAG Check

The dependency graph is intentionally linear:

```text
task-1-supersede-pending-resume
  -> task-2-resolve-resumed-ready
    -> task-3-bind-active-session
      -> task-4-regression-sweep
```

This is narrower than a maximally parallel DAG, but it matches the approved
sequential execution model for this remediation and minimizes cross-task
interference inside one runtime file.

## Placeholder Scan

This plan should contain no placeholder markers or cross-task shorthand
instructions. If you revise it, keep each task self-contained and keep the
commands exact.

## beads / Submission Note

This workspace currently has no configured issue tracker for `spur-mcp`, so a
true beads-backed `submit_plan(persist_as_epic=true)` is blocked until tracker
configuration is restored. The markdown plan is still structured so it can be
translated directly into a DAG-backed plan later.
