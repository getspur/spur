# bd-22q.1 — SpurLicense Feature-Gate Freshness Contract (C+)

**Status:** Approved 2026-04-29 (codex first-principles review folded in).
**Beads:** `bd-22q.1` (P1, 8h estimate, parent epic `bd-22q`).
**Origin:** Tier-revamp follow-up filed against codex's M1.x audit
(`docs/superpowers/plans/2026-04-28-tier-revamp-m1x-followup-spurlicense-gate-refresh.md`).
**Follow-up:** `bd-22q.14` carries the bridge-hydration + subscription-pump
work that this spec defers.

## Problem

`SpurLicense::feature_gate()` returns a stale `Arc<FeatureGate>` to all
non-TUI consumers. Mutating the license through any of the four facade
methods (`activate`, `validate`, `heartbeat`, `deactivate`) does not
update the cached `feature_gate` snapshot.

After `spur auth login` activates a Pro key, callers that already hold
an `Arc<FeatureGate>` (Orchestrator, PM service, CLI subcommands) keep
seeing the startup-time entitlement set. Pro features are denied to a
user who legitimately holds a Pro license.

The TUI is unaffected because M1 already pumps `update_state` through
its `LicenseUpdated` event handler. CLI and other crate consumers have
no equivalent path.

## Verified facts (file:line)

- `crates/spur-license/src/lib.rs:212-256` — `SpurLicense` struct;
  `feature_gate` is `Arc<FeatureGate>`. `from_env`/`from_env_or_disabled`
  call `feature_gate.update_state(&provider.current_state())` exactly
  once at construction. `from_provider` (line 222) is a test injector
  that delegates the seeding contract to the caller.
- `crates/spur-license/src/lib.rs:278-292` — All four mutating methods
  are bare `provider.X().await` delegates. None refresh `feature_gate`.
- `crates/spur-license/src/licenseseat.rs:84-93` — `replace_state`
  updates `Arc<RwLock<LicenseState>>` and broadcasts a `LicenseEvent`,
  but never touches any `FeatureGate`.
- `crates/spur-license/src/licenseseat.rs:267-273` —
  `LicenseSeatProvider::heartbeat()` Err path calls `degrade_current`
  which mutates state to `LicenseStatus::Degraded` AND broadcasts
  `HeartbeatFailed`, then returns `LicenseError`. **Today's gate
  misses this entirely.**
- `crates/spur-license/src/gate.rs:64-67` — `update_state(&self, state)`
  uses `ArcSwap.store` — lock-free, idempotent, microsecond-cost.
  `arc-swap` `swap` is `SeqCst`; concurrent stores have a global
  total order.
- `crates/spur-license/src/test_support.rs:18-185` — `FakeProvider`
  has scripted Ok/Err results, broadcast on commit, and atomic call
  counters. Test surface is ready.

## Consumer audit

`license.feature_gate()` callers outside spur-tui:

- `crates/spur-cli/src/main.rs:87, 808, 828, 1107, 1121, 1136`
- `crates/spur-cli/src/commands/auth.rs:77`
- `crates/spur-cli/src/commands/flags.rs:25`
- `crates/spur-mcp/tests/feature_gate_plumbing.rs:45, 50`
- `crates/spur-license/tests/community_smoke.rs:37, 50`

**Every consumer caches the returned `Arc`** (Orchestrator stores it
at construction and reuses it across the process lifetime). Therefore
options that refresh on access (lazy-refresh-on-`feature_gate()`-call)
do not help — only updates that propagate through the shared `Arc`
reach caching consumers.

## Design (C+)

### Mechanism

Each mutating method on `SpurLicense` calls
`self.feature_gate.update_state(&new_state)` after the provider returns,
on **both** the Ok path and any Err path that internally mutates
provider state.

```rust
// crates/spur-license/src/lib.rs

pub async fn validate(&self) -> Result<LicenseState> {
    let next = self.provider.validate().await?;
    self.feature_gate.update_state(&next);
    Ok(next)
}

pub async fn activate(&self, key: &str) -> Result<LicenseState> {
    let next = self.provider.activate(key).await?;
    self.feature_gate.update_state(&next);
    Ok(next)
}

pub async fn deactivate(&self) -> Result<LicenseState> {
    let next = self.provider.deactivate().await?;
    self.feature_gate.update_state(&next);
    Ok(next)
}

pub async fn heartbeat(&self) -> Result<LicenseState> {
    match self.provider.heartbeat().await {
        Ok(next) => {
            self.feature_gate.update_state(&next);
            Ok(next)
        }
        Err(err) => {
            // LicenseSeatProvider degrades provider state internally
            // before returning Err; refresh from the canonical
            // post-mutation snapshot before propagating.
            self.feature_gate.update_state(&self.provider.current_state());
            Err(err)
        }
    }
}
```

`heartbeat` is the only Err-mutating path today. `activate/validate/
deactivate` Err paths in the production provider do not call
`replace_state` (they short-circuit before mutating). For completeness
and forward-compatibility, the same pattern would apply if a future
provider Err arm starts mutating — but adding gate refresh to those
non-mutating Err paths today would just re-broadcast Community
baseline on every transient network failure, which is wrong. **Do not
preemptively add gate refresh to non-mutating Err arms.**

### Why this is sufficient for bd-22q.1's stated scope

This closes the **caller-observable** freshness contract:

- `spur auth login` activates Pro → `Orchestrator`'s cached
  `Arc<FeatureGate>` flips to Pro before `auth` returns.
- `spur ... validate` (or runtime auto-validate) → cached gate
  reflects validated/expired/invalid state synchronously.
- Heartbeat-degrade → cached gate reflects degraded state
  synchronously.
- `Deactivate` → cached gate flips to inactive synchronously.

### What is explicitly out of scope (deferred to bd-22q.14)

- **Autonomous SDK events** (server-pushed revocation,
  offline-verification re-checks, license-loaded). These flow through
  `spawn_sdk_event_bridge` (`licenseseat.rs:95-128`) which currently
  attaches the **stale snapshot** to forwarded events. Adding a
  subscription pump to `SpurLicense` without first fixing the bridge
  hydration would propagate the stale snapshot into the gate — false
  signal worse than today's silence.
- **`validate()` Err-on-revocation leak.** When upstream revokes the
  license, the SDK clears its cache and `validate()` returns Err.
  SPUR's mapping at `licenseseat.rs:202-208` propagates the Err
  before any `replace_state` runs, so provider state stays stale.
  Fixing this is a provider-internal change tracked in `bd-22q.14`.

These are real bugs, but C+ is intentionally smaller in scope so
that bd-22q.1 ships in 8h with a tight blast radius. `bd-22q.14`
layers the bridge fix and subscription pump on top.

## Doc-comment contract

Add to `SpurLicense`:

```rust
/// Facade over a `LicenseProvider` paired with a shared
/// `Arc<FeatureGate>`.
///
/// # Freshness contract
///
/// The `Arc<FeatureGate>` returned by [`feature_gate`] is a shared
/// entitlement snapshot. Successful mutating calls (`activate`,
/// `validate`, `heartbeat`, `deactivate`) refresh the snapshot
/// synchronously before returning. Mutating error paths that update
/// provider state (e.g., `heartbeat` degrade-on-failure) refresh the
/// snapshot before returning the error.
///
/// Autonomous provider events (server-pushed revocation,
/// offline-verification re-checks) are NOT yet pumped into this
/// gate; consumers needing strict ordering for autonomous events
/// should subscribe to [`subscribe`] and reconcile against
/// [`current_state`] directly. Pumping is tracked under bd-22q.14.
///
/// [`feature_gate`]: SpurLicense::feature_gate
/// [`subscribe`]: SpurLicense::subscribe
/// [`current_state`]: SpurLicense::current_state
```

## Concurrency notes

- `FeatureGate::update_state` is `&self` and uses `ArcSwap.store`
  with `SeqCst` ordering. Concurrent calls have a global total
  order; the last writer wins. Acceptable for license state where
  eventual consistency is the right contract.
- Mutating-method races (two threads call `validate` simultaneously)
  converge: the provider serializes underlying SDK calls (single
  `RwLock`/SDK lock); two `update_state` calls land in arc-swap's
  global order with values that match the provider's
  RwLock state at each call's commit point. Inversion theoretically
  possible but bounded by the next mutation, which resyncs.
- `&new_state` is preferred over `&self.provider.current_state()`
  for the gate update because it avoids an extra RwLock read and
  matches the value the caller just received.

## Test plan

### Unit tests in `crates/spur-license/tests/feature_gate_freshness.rs` (new)

For each test, construct `SpurLicense::from_provider(fake, gate)`
where `gate` is seeded at the Community baseline.

1. **`validate_pro_state_refreshes_cached_gate`**
   - Capture `let cached = license.feature_gate();` (clone Arc).
   - Assert `cached.has(PRO_KEY) == false`.
   - `fake.push_validate_result(Ok(LicenseState::active_validated(Plan::Pro, ...)))`.
   - `license.validate().await.unwrap();`
   - Assert `cached.has(PRO_KEY) == true` — proves propagation
     through the **cached** Arc, not a fresh `feature_gate()` call.

2. **`activate_pro_state_refreshes_cached_gate`** — same shape with
   `push_activate_result`.

3. **`deactivate_refreshes_cached_gate_to_inactive`** — same shape
   with `push_deactivate_result(Ok(LicenseState::inactive("...")))`;
   assert all entitlements drop.

4. **`heartbeat_ok_refreshes_cached_gate`** — same shape with
   `push_heartbeat_result(Ok(active state))`.

5. **`heartbeat_err_with_degrade_refreshes_cached_gate`** — uses a
   custom provider (or extends `FakeProvider` with
   `degrade_current_then_err` script entry) to simulate the
   LicenseSeatProvider Err-mutating path. Assert cached gate's
   tier reflects Degraded after Err returns. **If `FakeProvider`
   doesn't currently expose this, plan task #2 covers the
   minimum extension needed.**

6. **`mutation_failure_without_state_change_keeps_gate_unchanged`** —
   `push_validate_result(Err(...))`; capture gate snapshot before;
   call `validate`; assert error returned and gate snapshot is
   bit-identical (no spurious refresh on transient network errors).

### Integration check (existing `community_smoke.rs`)

Existing tests should continue passing without modification — they
construct `SpurLicense` and immediately query `feature_gate()`,
which is covered by the construction-time seed (`lib.rs:236, 247`).
No behavior change for these.

## Implementation tasks

The implementation plan (writing-plans output) will decompose this
into roughly:

1. **Task 1 — facade refresh (`lib.rs`)**: implement the four
   mutating-method updates with the heartbeat-Err branch; add
   doc-comment.
2. **Task 2 — `FakeProvider` extension (test_support.rs)**: add
   minimum surface for simulating heartbeat-Err-with-degrade
   (e.g., `push_heartbeat_degraded_err(state, err)` that mutates
   internal state to Degraded, broadcasts, then returns Err —
   mirrors `LicenseSeatProvider::heartbeat` behavior).
3. **Task 3 — regression tests
   (`tests/feature_gate_freshness.rs`)**: write the 6 unit tests
   above; verify all pass; verify
   `cargo test -p spur-license --features test-support` is green.

## Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Concurrent `validate`/`activate` race produces brief inversion | Low | Document eventual-consistency contract; next mutation resyncs. SDK serializes underlying calls. |
| Future provider adds Err-mutating path without updating facade | Low-Medium | Doc-comment + test for heartbeat-Err sets the pattern; reviewers should catch. |
| `FakeProvider` extension introduces test-only API drift | Low | Mirror LicenseSeatProvider's degrade signature exactly; gated behind `test-support` feature. |
| Existing `community_smoke.rs` tests rely on stale-gate behavior | Very Low | Read tests; they query at construction, where seed is already correct. |

## Acceptance criteria

- [ ] All four mutating methods on `SpurLicense` refresh
  `feature_gate` per the spec (Ok always; heartbeat-Err
  additionally).
- [ ] Doc-comment added to `SpurLicense` matches the contract
  language above; cross-references bd-22q.14.
- [ ] 6 unit tests in `tests/feature_gate_freshness.rs` pass.
- [ ] No regression in existing `tests/community_smoke.rs`,
  `tests/fake_provider.rs`, `tests/invariants.rs`, or
  `tests/licenseseat_probe.rs`.
- [ ] No regression in `crates/spur-mcp/tests/feature_gate_plumbing.rs`.
- [ ] `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings`
  clean.
- [ ] `cargo fmt --all -- --check` clean.
- [ ] Beads issue `bd-22q.1` closed; `bd-22q.14` confirmed remains
  Open with codex review reference.

## Out of scope (filed elsewhere)

- Bridge hydration + autonomous-event subscription pump:
  `bd-22q.14`.
- `validate()` Err-on-revocation leak: covered under `bd-22q.14`.
- CLI end-to-end smoke for `spur auth login`: deferred to manual
  verification under `bd-22q.4`.
- TUI gate refresh: already shipped in M1
  (`docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-m1-tui-gate-refresh.md`).

## References

- Origin filing: `docs/superpowers/plans/2026-04-28-tier-revamp-m1x-followup-spurlicense-gate-refresh.md`
- M1 plan (TUI fix shipped): `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-m1-tui-gate-refresh.md`
- Codex first-principles review (this spec): `spur://continuation/beb3f286-1fdd-434c-8344-25998724a574`
- Follow-up issue: `bd-22q.14` (bridge hydration + pump)
- Plan D dependency chain: `bd-22q.1 → bd-22q.14 → bd-22q.11 → bd-22q.12`
