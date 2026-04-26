# bd-arch.21 Operational Review — Kimi

**Commit under review:** `e07a6cd1` (framing doc on main) — peer mailbox production wiring + reconciler spawn
**Reviewer:** kimi
**Date:** 2026-04-26
**Scope:** Production wire-up for the Stage-1 peer mailbox subsystem hardened by bd-cpf.1–7. Evaluates boot-time bundle construction, reconciler spawn, config surface, shutdown ordering, test coverage, and deployment risk.

---

## Verdict

**Approve with BLOCKER on JoinHandle storage + shutdown ordering, SHOULD-DO on integration tests, NICE-TO-HAVE on `Limits` config bridge.**

Ship `peer_mailbox_enabled=false` by default. The wire-up must store the reconciler `JoinHandle` in `Orchestrator.background_tasks` so `Drop` aborts it cleanly. Do NOT flip the default to `true` in this ticket; file a follow-up after SPUR-internal validation.

---

## Pager-risk classification

| Deployment path | Pager risk | Rationale |
|---|---|---|
| **Default-off (`peer_mailbox_enabled=false`)** | **Near zero** | No new code paths execute. The only risk is latent bug in the wire-up itself (e.g., constructor panic) — but the `if config.peer_mailbox_enabled` gate prevents that. |
| **Opted-in (`peer_mailbox_enabled=true`)** | **Medium** | New event volume (`WorkerPeerMessageAccepted`, `Rejected`, `Undeliverable`, `Reconciled`), new `tokio::spawn` (reconciler), new mpsc channel (stranded messages). Risk #22 (unbounded in-memory ledger) becomes live. 62 unit tests cover logic but zero production miles exist. |

Overall ticket risk: **Low-to-medium** — strictly additive when off; first-time exercise of hardened but unproven code when on.

---

## Recommendation: scope, alert thresholds, rollback story

### In scope for bd-arch.21

1. **Boot-time bundle construction + attach**
   - Insert immediately after `Orchestrator::new` returns, before any `run_*` call.
   - Gate on `config.peer_mailbox_enabled`.
   - Use `Limits::default()` for Stage-1 (no config bridge).

2. **Reconciler spawn + JoinHandle storage**
   - Spawn `run_reconciler_loop` with the `reconciler_rx` half.
   - Push the `JoinHandle` into `Orchestrator.background_tasks` so `Drop` aborts it.
   - This is required for correct shutdown ordering and directly helps architecture Risk #6 (untracked spawns).

3. **Integration tests (minimum)**
   - Test (a): `peer_mailbox_enabled=true` → worker emits `_spur/peer_message` → message reaches ledger + event emitted.
   - Test (b): `peer_mailbox_enabled=false` (default) → same notification silently dropped.
   - Test (c): reconciler task drains a manufactured `StrandedMessage` and emits `WorkerPeerMessageUndeliverable`.

### Out of scope (deferred)

- **`Limits` config surface** — NICE-TO-HAVE for Stage-1; required for Stage-2 production tuning.
- **Panic-restart supervisor** — reconciler body is small and mostly infallible; defer to post-Stage-2.
- **`peer_mailbox_enabled` default flip to `true`** — follow-up ticket after internal validation.
- **Risk #22 (ledger pruning)** — separate ticket; wire-up choice does not materially affect pruning difficulty.

### Alert thresholds-of-interest (default-on path only)

| Signal | Threshold | Page? |
|---|---|---|
| `WorkerPeerMessageUndeliverable` rate > 5/min | Worker guards are dropping unfinalized | Stage-2 candidate |
| `WorkerPeerMailboxReconciled` with `remaining > 0` after startup | Stranded messages from previous session | Stage-2 candidate |
| `PeerMailboxRouter` rejection rate > 20% | Worker sending malformed peer messages | No — diagnostic |
| In-memory ledger entry count growth | Risk #22 live | Stage-2 candidate (needs metric first) |

### Rollback story

**Runtime flip-off without restart: NO.**

`peer_mailbox_enabled` is evaluated once at orchestrator construction time (config load → `Orchestrator::new`). There is no hot-reload path for `SpurConfig`. If an incident occurs with `peer_mailbox_enabled=true`:

1. **Immediate:** Kill the SPUR process (Ctrl-C / SIGTERM). `Orchestrator::drop` aborts `background_tasks`, including the reconciler. Stranded messages in the mpsc buffer are dropped; ledger entries leak in `Accepted`/`Queued` until process restart (acceptable in Stage-1 with in-memory ledger).
2. **Short-term:** Edit config to `peer_mailbox_enabled=false`, restart SPUR.
3. **Mid-term:** Root-cause via event funnel logs (`WorkerPeerMessage*` events).

Document this limitation explicitly in `CHANGELOG.md` and config docs.

### Patch-size estimate

- Boot-time bundle construction + attach (`spur-cli` or `Orchestrator`): ~30 LoC
- Reconciler spawn + `background_tasks.push`: ~15 LoC
- Integration tests (3 tests): ~90 LoC
- `CHANGELOG.md` entry: ~10 LoC
- **Total: ~145 LoC across 2–3 files + test module.**

---

## Direct answers to design questions (Q1–Q9)

### Q1. Insertion point: where should boot-time bundle construction live?

**Inside `Orchestrator::new`, gated by `config.peer_mailbox_enabled`.**

Rationale:
- `spur-cli` has ~6 construction sites (`load_orchestrator`, `build_interactive_host`, `commands/init.rs`, etc.). Repeating the wire-up in every caller is error-prone and guarantees drift.
- `Orchestrator::new` already owns config, funnel, and `background_tasks`. It is the natural owner of subsystem lifecycle.
- Precedent: `sweep_handle` (outcome store GC) is spawned inside `Orchestrator::new` and pushed to `background_tasks`. The reconciler should follow the same pattern.

Implementation sketch:
```rust
if config.peer_mailbox_enabled {
    let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
    let (reconciler_tx, reconciler_rx) = tokio::sync::mpsc::unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel.clone(),
        reconciler_tx,
        Limits::default(),
        // brain_session_id: see Q3
        "".to_string(), // placeholder — Q3 addresses this
    ));
    let builder = Arc::new(PeerPromptContextBuilder::new(ledger.clone()));
    let bundle = PeerMailboxBundle { router, builder, ledger };
    orchestrator.peer_mailbox = Some(bundle);
    let reconciler_handle = tokio::spawn(crate::peer_mailbox::run_reconciler_loop(
        reconciler_rx,
        ledger.clone(),
        funnel.clone(),
        // brain_session_id: see Q3
        "".to_string(),
    ));
    orchestrator.background_tasks.push(reconciler_handle);
}
```

### Q2. `Limits` config surface: expose in `SpurConfig` or hardcode `Limits::default()`?

**Hardcode `Limits::default()` for Stage-1. Defer config bridge to Stage-2 or a dedicated tuning ticket.**

Rationale:
- Stage-1 is explicitly "in-memory ledger, single-process, opt-in." The defaults (`drain_quiet_window_ms=2000`, `drain_max_total_ms=10000`, `max_pending_mailbox_depth=8`) are conservative and match the values already validated by the 62 peer-mailbox tests.
- Adding config fields creates forward-compat pressure: if we rename or restructure `Limits` for Stage-2 (e.g., per-delegation overrides), we must support migration for the config keys we introduced in Stage-1.
- Operator tuning is not a stated Stage-1 need. The only knob likely to need adjustment is `drain_max_total_ms`, and that can be done via a one-line code change + deploy.

**NICE-TO-HAVE compromise:** Add a `peer_mailbox: Option<PeerMailboxConfig>` section to `SpurConfig` with all fields optional and defaulting to `Limits::default()`. This is ~40 LoC and zero migration risk because every field is `#[serde(default)]`. Do this only if the implementer has spare capacity; otherwise defer.

### Q3. `brain_session_id` lifetime on the bundle: code smell?

**Yes — it is a design tension, but acceptable for Stage-1 with a session-id resolver refactor.**

Current state:
- `PeerMailboxRouter::new` takes `brain_session_id: String` and stores it.
- `run_reconciler_loop` takes `brain_session_id: String` and bakes it into `WorkerPeerMessageUndeliverable` events.
- The orchestrator handles many brain sessions over its lifetime (reconnect, resume, new `run_adhoc` calls).

Problem:
- If brain reconnects with a new session ID, the router still emits `WorkerPeerMessageAccepted` with the OLD session ID. The reconciler emits `WorkerPeerMessageUndeliverable` with the OLD session ID. Dashboards and lineage will attribute peer-mailbox events to a stale session.

Options:
- **(a) Remove `brain_session_id` from the bundle; pass per-emit-call.** Cleanest, but requires refactoring every router emission site and the reconciler signature. The reconciler is async and does not have access to a "current session" getter.
- **(b) Move bundle into per-session state, rebuild on session swap.** Breaks the "survives across attempts" doc-comment on `run_reconciler_loop`. Also means the reconciler restarts on every reconnect, losing any stranded messages in the mpsc buffer.
- **(c) Accept staleness for Stage-1.** Simplest; the event volume is low and the session-ID mismatch is a cosmetic/dashboard issue, not a correctness issue.
- **(d) Refactor to `Arc<AtomicCell<String>>` or `Fn() -> String` resolver.** Best long-term: the router and reconciler hold a resolver, not a value. On session swap, update the atomic. No restart, no stale events.

**Recommendation:** Implement **(d)** as a `Arc<std::sync::RwLock<String>>` or `Arc<AtomicCell<String>>` in bd-arch.21. The refactor touches:
- `PeerMailboxRouter::new` signature: `brain_session_id: Arc<RwLock<String>>`
- `run_reconciler_loop` signature: same
- Orchestrator: create the `Arc` in `new`, update it at session-start sites (`create_brain_session`, `load_brain_session`, `run_adhoc`)

Cost: ~25 LoC. Worth it to avoid baking a known bug into production.

### Q4. JoinHandle tracking: explicit shutdown handle or fire-and-forget?

**BLOCKER: Must store the JoinHandle in `Orchestrator.background_tasks`.**

Evidence:
- `Orchestrator` already has `background_tasks: Vec<JoinHandle<()>>` (line 616).
- `Drop for Orchestrator` already iterates and aborts every handle (lines 718–724).
- The sweep task (`sweep_handle`) is pushed to this vec at line 859 — this is the established pattern.

Fire-and-forget would:
- Increase the un-tracked spawn count (Risk #6).
- Leave the reconciler task running after `Orchestrator` drops, which extends lifetime past cleanup and may panic if the funnel is dropped first.
- Lose stranded messages in the mpsc buffer without aborting the task (the task would hang on `rx.recv().await` until the sender half is dropped, which is at process exit).

Correct ordering:
1. `Orchestrator::drop` aborts `background_tasks` (including reconciler).
2. Aborting the reconciler drops its `rx` half of the mpsc.
3. Any future `PeerMessageGuard::drop` calls (from worker cleanup) will get `Err(SendError)` on `reconciler_tx.send`; the guard already logs a warning and defers to startup reconcile (line 91–94 of `guard.rs`).
4. No panic, no leak beyond the known Risk #22.

### Q5. Panic-restart supervisor: in scope or deferred?

**Deferred.**

Rationale:
- `run_reconciler_loop` body is ~45 LoC: a `while let Some(stranded) = rx.recv().await` loop, one `ledger.transition` call, one `funnel.emit` call.
- The only `unwrap`/`expect` paths are in `PeerMailboxRouter::new` (assert on `drain_max_total_ms > 0`), which runs at construction time, not in the reconciler task.
- A panic in the reconciler would abort the task and drop the `rx` half. Subsequent `PeerMessageGuard::drop` attempts would log a warning (line 91) and defer to startup reconcile. The startup reconcile runs at next brain session start, so the stranded messages would be recovered then.
- For Stage-1, a `JoinHandle::is_finished()` check on shutdown (already implicit via `Drop`) is sufficient. A full panic-restart supervisor (e.g., `tokio::task::Builder` + `TaskTracker` + restart loop) is Stage-2 work when the ledger is persistent and cross-restart durability matters.

### Q6. Config-flag default: keep `false` or flip to `true`?

**Keep `false` by default in bd-arch.21. File a follow-up ticket to flip after internal validation.**

Rationale:
- bd-cpf.1–7 hardened the implementation, but it has **zero production miles**. Every path — router acceptance, guard finalize, drain timeout, startup reconcile, reconciler stranded recovery — has been tested in fixtures but never exercised in a live brain-worker loop.
- Default-on would expose every SPUR install to new event volume, new mpsc channels, and the unbounded ledger (Risk #22). A regression would affect all users, including those who never asked for peer mailbox.
- Conservative default aligns with the framing doc: "A follow-up flip-the-default ticket can be filed once SPUR-internal users have validated."
- The wire-up is still valuable when default-off: it proves the boot path works, keeps the code exercised in opt-in deployments, and provides a clean migration path.

Counter-argument (noted but rejected): "If no one opts in, we don't learn about bugs." Mitigation: SPUR-internal dogfooding can opt in explicitly. The framing doc already plans this.

### Q7. Minimum tests for production confidence

**Three integration tests are essential. Anything beyond that is SHOULD-DO or NICE-TO-HAVE.**

1. **Happy-path end-to-end (enabled)**
   - Construct `Orchestrator` with `peer_mailbox_enabled=true`.
   - Emit a synthetic `_spur/peer_message` ext-notification through `spur_ext_interp::interpret_peer_message`.
   - Assert ledger contains the message in `Accepted` state.
   - Assert `WorkerPeerMessageAccepted` event was emitted to the funnel.

2. **Default-off silent drop**
   - Construct `Orchestrator` with default config (`peer_mailbox_enabled=false`).
   - Emit same synthetic notification.
   - Assert ledger is empty (no router invocation).
   - Assert no peer-mailbox event was emitted.

3. **Reconciler task drains stranded message**
   - Construct `Orchestrator` with `peer_mailbox_enabled=true`.
   - Manually create a `PeerMessageGuard`, drop it without `finalize()`.
   - Assert reconciler transitions ledger to `Undeliverable`.
   - Assert `WorkerPeerMessageUndeliverable` event was emitted.

**SHOULD-DO additions:**
- Test that `Orchestrator::drop` aborts the reconciler task cleanly (spawn orch, drop it, assert handle.is_finished()).
- Test that three `run_startup_reconcile` call sites do not duplicate events when reached in sequence (see Q8).

**NICE-TO-HAVE:**
- Test `brain_session_id` resolver update across session swap (validates Q3 fix).

### Q8. Three startup-reconcile call sites: reachable, necessary, duplicate-risk?

**All three are reachable and necessary. Duplicate-risk exists but is mitigated by idempotent transition logic.**

Reachability:
- `:1150` — inside `run_adhoc` (one-shot non-interactive path). Reachable via `spur run` / `spur exec` CLI commands.
- `:2540` — inside `create_brain_session` (new interactive session). Reachable on first `run_interactive` or after brain crash/restart.
- `:2806` — inside `load_brain_session` (resume/reconnect path). Reachable on brain reconnect with existing session ID.

Necessity:
- In Stage-1 with in-memory ledger, `:1150` (`run_adhoc`) is the only site that matters for the ad-hoc path. `:2540` and `:2806` cover interactive new/resume. All three are needed because the orchestrator is long-lived and brain sessions come and go.

Duplicate-risk:
- `run_startup_reconcile` iterates the ledger and calls `transition(&message_id, Undeliverable)` for each non-terminal entry.
- The ledger's `transition` is idempotent: if the entry is already `Undeliverable`, it returns `TransitionOutcome::Unchanged`.
- `run_startup_reconcile` only emits `WorkerPeerMailboxReconciled` on `Changed` (per bd-cpf.5b/5c hardening). So duplicate calls do not duplicate events.
- **However**, duplicate calls still burn CPU iterating the ledger. In Stage-1 with small ledger this is negligible. In Stage-2 with persistent/SQLite ledger, consolidate to a single per-session helper.

**Recommendation for bd-arch.21:** Leave the three sites as-is. The idempotent transition logic prevents duplicate events. Add a code comment at each site noting "Idempotent; safe to call multiple times per session." Consolidation is Stage-2 cleanup.

### Q9. Stage-2 forward compatibility: does the wire-up shape generalize?

**Yes, with two caveats.**

What generalizes well:
- Bundle construction inside `Orchestrator::new` generalizes to persistent ledger: replace `Arc::new(InMemoryLedger::new())` with `Arc::new(SqliteLedger::open(path).await?)`.
- `background_tasks.push(reconciler_handle)` generalizes regardless of ledger backend.
- The `peer_mailbox_enabled` gate generalizes to a feature-flag or license check.

What needs re-architecture in Stage-2:
1. **Ledger persistence → reconciler must drain before process exit.** With SQLite, `Orchestrator::drop` should await reconciler drain (with timeout) rather than abort, to ensure stranded messages are committed to disk. This changes shutdown ordering from "abort" to "graceful drain + abort fallback."
2. **`Limits` config bridge.** Stage-2 will need operator-tunable knobs. If we deferred the config surface in bd-arch.21, Stage-2 will add it then — no harder than adding it now.
3. **`brain_session_id` resolver (Q3).** The `Arc<RwLock<String>>` pattern generalizes cleanly to Stage-2.

**Conclusion:** The bd-arch.21 wire-up shape does not paint Stage-2 into a corner. The two meaningful changes (ledger construction site, shutdown ordering) are localized and expected.

---

## Classification summary

| Item | Verdict | Classification | Pager risk |
|---|---|---|---|
| Boot-time bundle construction in `Orchestrator::new` | Ship | **SHOULD-DO** | Near zero (default-off) |
| Reconciler spawn + `JoinHandle` in `background_tasks` | Ship | **BLOCKER** | Low when on |
| `Limits::default()` (no config bridge) | Ship | **NICE-TO-HAVE** to add config | Near zero |
| `brain_session_id` → `Arc<RwLock<String>>` resolver | Ship | **SHOULD-DO** | Low when on |
| Keep `peer_mailbox_enabled=false` default | Ship | **SHOULD-DO** | Near zero |
| Integration test (enabled happy path) | Add | **SHOULD-DO** | N/A |
| Integration test (default-off silent drop) | Add | **SHOULD-DO** | N/A |
| Integration test (reconciler drains stranded) | Add | **SHOULD-DO** | N/A |
| Panic-restart supervisor | Defer | **NICE-TO-HAVE** | N/A |
| Default-flip follow-up ticket | File after merge | **SHOULD-DO** | N/A |

---

## CHANGELOG strategy

Add to `CHANGELOG.md` under `## Unreleased` → `### Added`:

```markdown
- **Peer mailbox production wire-up (Stage-1).** The peer mailbox subsystem
  (hardened in bd-cpf.1–7) is now constructed and attached when
  `peer_mailbox_enabled = true` is set in config. A long-lived reconciler
  task drains stranded peer messages and emits audit events. Startup
  reconcile runs at brain session boundaries. Default is `false`; no
  behavioral change for existing deployments. Operators who opt in should
  monitor for `WorkerPeerMessageUndeliverable` events and be aware that
  the in-memory ledger does not prune entries (Risk #22). To disable,
  set `peer_mailbox_enabled = false` and restart SPUR — runtime toggle
  is not supported. (bd-arch.21)
```

Also add under `### Fixed`:

```markdown
- **Architecture Risk #21 (partial).** The peer mailbox reconciler is now
  spawned at orchestrator boot and aborted on shutdown. Previously the
  receiver was dropped immediately after construction, causing stranded
  messages to be silently lost. (bd-arch.21)
```

---

## Summary

bd-arch.21 is a small, high-leverage wire-up: ~145 LoC to bring 7,800+ lines of hardened peer-mailbox code into production reach. The key operational decisions are:

1. **Store the reconciler JoinHandle** — non-negotiable for shutdown correctness and Risk #6 hygiene.
2. **Keep default-off** — conservative, appropriate for zero-mileage code.
3. **Fix `brain_session_id` staleness** with an `Arc<RwLock<String>>` resolver to avoid baking a dashboard-bug into production.
4. **Three integration tests** validate the happy path, the default-off silent path, and reconciler drainage.
5. **Document no-runtime-toggle** in CHANGELOG so operators know rollback requires restart.

With these in place, the default-off path carries near-zero pager risk, and the opt-in path has a clear operational contract, observable failure surface, and documented limitations.
