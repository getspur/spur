# bd-arch.21 Design Review

## Recommendation

**Scope**: Construct the `PeerMailboxBundle` at orchestrator boot (`Orchestrator::new`), spawn the reconciler loop there, and store its `JoinHandle` on the Orchestrator to ensure clean shutdown. For this ticket, accept the Stage-1 `brain_session_id` staleness by passing a placeholder (e.g., `"orchestrator-wide"`) or the initial session ID, as fixing the router's session-affinity is a larger refactoring better suited for a separate ticket. Defer panic-restart and flip-to-true to follow-ups.

**In/Out of Scope**:
- IN: Boot-time construction inside `Orchestrator::new`
- IN: `JoinHandle` storage for `run_reconciler_loop`
- OUT: `Limits` config surface (use defaults)
- OUT: Panic-restart supervisor (defer)
- OUT: Flipping `peer_mailbox_enabled` to `true` (defer)
- OUT: Removing `brain_session_id` from the router (accept staleness for now)

## Q1: Insertion point
**Answer**: `Orchestrator::new`. Building it here keeps the `spur-cli` agnostic of peer mailbox internals and automatically wires the bundle for all execution modes (`run_interactive`, `run_adhoc`, etc.). It perfectly models the Stage-2 requirement where the ledger must outlive individual sessions.
**Verdict**: BLOCKER

## Q2: `Limits` config surface
**Answer**: Hardcode `Limits::default()` for Stage-1. Adding config surfaces now adds boilerplate and testing burden for fields we don't yet know operators will need to tune. We can expose them in `SpurConfig` in a follow-up if empirical data shows the defaults are inadequate.
**Verdict**: OUT OF SCOPE

## Q3: `brain_session_id` lifetime on the bundle
**Answer**: It is a code smell. The orchestrator outlives individual brain sessions (e.g., across reconnects), but the router assumes a 1:1 lifetime. However, rebuilding the bundle per session breaks the Stage-2 persistent ledger requirement and requires messy reconciler aborts. The correct architectural fix is to remove `brain_session_id` from the router/reconciler entirely and pass it dynamically via `route()`/`emit()`. For bd-arch.21, construct the bundle once with a dummy `"orchestrator-wide"` ID and accept the staleness (the events will emit with the dummy ID).
**Verdict**: SHOULD-DO (remove field in a dedicated follow-up)

## Q4: JoinHandle tracking
**Answer**: Add an explicit shutdown handle (e.g., `Option<tokio::task::JoinHandle<()>>` on `Orchestrator`). Relying on fire-and-forget `tokio::spawn` actively worsens Risk #6 and risks panics during clean shutdown. Storing the handle allows the orchestrator to cleanly `.abort()` the reconciler on shutdown.
**Verdict**: BLOCKER

## Q5: Panic-restart supervisor
**Answer**: Deferred. The reconciler loop is a tight loop matching on ledger state with minimal complex logic. The panic surface is very small. A simple `handle.is_finished()` check or just aborting on shutdown is sufficient for Stage-1. A full supervisor is over-engineering the immediate need.
**Verdict**: OUT OF SCOPE

## Q6: Config-flag default
**Answer**: Keep `peer_mailbox_enabled: false` by default. This honors the conservative rollout strategy for Stage-1, preventing unproven code from disrupting all users. A follow-up ticket can flip it after internal validation.
**Verdict**: BLOCKER (must stay false)

## Q7: Tests
**Answer**: The proposed minimum coverage is correct and sufficient:
(a) e2e happy path with flag=true
(b) e2e ignored path with flag=false
(c) reconciler task draining a manufactured `StrandedMessage`
No additional tests are essential since the core logic is already heavily covered.
**Verdict**: BLOCKER

## Q8: Three startup-reconcile call sites
**Answer**: The three call sites (`run_adhoc`, `create_brain_session`, and `load_brain_session`) are mutually exclusive per session. A given session is either adhoc, newly created, or loaded. Thus, there is NO risk of duplicate executions or duplicate `WorkerPeerMailboxReconciled` audit events for a single session. Consolidating them into a shared helper is a good refactor, but not a correctness concern.
**Verdict**: NICE-TO-HAVE

## Q9: Stage-2 forward compatibility
**Answer**: By constructing the bundle in `Orchestrator::new` and keeping it alive across the orchestrator's lifetime, we perfectly align with Stage-2's requirement for a persistent ledger that survives session restarts. The only friction is the `brain_session_id` bake-in (Q3), which must be addressed before Stage-2 so events correctly reflect the active session.
**Verdict**: PASS

## Patch-size estimate
For the preferred scope (Orchestrator insertion + JoinHandle tracking + default limits + dummy brain_session_id):
~60-80 LoC total (including tests).
