# bd-22q.1 — SpurLicense Feature-Gate Freshness Contract (C+)

**Status:** Approved 2026-04-29 (codex/gemini/kimi triple review folded in).
**Beads:** `bd-22q.1` (P1, 8h estimate, parent epic `bd-22q`).
**Origin:** Tier-revamp follow-up filed against codex's M1.x audit
(`docs/superpowers/plans/2026-04-28-tier-revamp-m1x-followup-spurlicense-gate-refresh.md`).
**Follow-ups carrying deferred work:**
- `bd-22q.14` — bridge hydration + subscription pump (autonomous events).
- `bd-22q.15` — `LicenseSeatProvider` cross-method operation serialization
  (closes the over-permissioning race kimi's adversarial review surfaced).

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
  rebuilds an `EntitlementSnapshot` (policy resolution + quota merge
  + feature filtering) and stores it via `ArcSwap.store` — lock-free
  and idempotent. `arc-swap` `swap` is `SeqCst`
  (`arc-swap-1.9.1/src/lib.rs:477-491`); concurrent stores have a
  global total order. The pointer-swap is microseconds; the snapshot
  rebuild is bounded but not microsecond-cheap. At our call frequency
  (≤ once per mutating method call) the cost is irrelevant.
- `crates/spur-license/src/test_support.rs:18-185` — `FakeProvider`
  has scripted Ok/Err results, broadcasts on Ok `commit`, and atomic
  call counters. Heartbeat-Err-with-degrade is **not yet modelled**:
  plain `Err` script entries do not mutate provider state, so Test 5
  below requires a new `push_heartbeat_degraded_err(state, err)`
  script entry (Task 2 in the implementation plan).

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
`replace_state` — they short-circuit before mutating
(`licenseseat.rs:184-190`, `:202-208`, `:277-283`). The
`LicenseProvider` trait does **not** document Err-arm state-mutation
behavior; this spec relies on the LicenseSeatProvider invariant that
only `heartbeat` Err mutates. Adding gate refresh to the other Err
arms today would be at best redundant (rewriting the same snapshot)
and at worst misleading: it would make a stale-or-unvalidated provider
snapshot look freshly reconciled with no signal that anything
changed. **Do not preemptively add gate refresh to non-mutating Err
arms.** If a future `LicenseProvider` impl adds an Err-mutating path,
it must be paired with a matching facade update — see Risk register.

### Why this is sufficient for bd-22q.1's stated scope

This closes the **caller-observable** freshness contract:

- `spur auth login` activates Pro → `Orchestrator`'s cached
  `Arc<FeatureGate>` flips to Pro before `auth` returns.
- `spur ... validate` (or runtime auto-validate) → cached gate
  reflects validated/expired/invalid entitlements synchronously.
- Heartbeat-degrade → cached gate refreshes from the provider's
  post-degrade state synchronously. Note: `FeatureGate` exposes
  tier/features/quotas/source-metadata only — it does NOT carry
  `LicenseStatus`, so the visible effect of heartbeat-degrade is
  whatever entitlement set `build_snapshot` produces from the
  degraded `LicenseState` (see `gate.rs:88-90`: `is_active() == false`
  drops to `EntitlementSnapshot::default()`; `Degraded` is treated as
  active per `lib.rs:172-174`, so entitlements survive — the gate
  does not become observably "degraded" via `tier()` alone).
- `Deactivate` → cached gate flips to inactive synchronously.

### What is explicitly out of scope

**Deferred to bd-22q.14 (bridge hydration + subscription pump):**

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

**Deferred to bd-22q.15 (cross-method serialization):**

- **`LicenseSeatProvider` does not serialize cross-method
  operations.** SDK calls in `activate`/`validate`/`heartbeat`/
  `deactivate` are awaited without any lock held; only the brief
  `replace_state` write is serialized via the `RwLock`. A concurrent
  `validate`/`deactivate` race can produce **durable
  over-permissioning** (gate ends up at Pro when the user just
  deactivated; subsequent heartbeat reinforces the bad state because
  it reads `current_snapshot()`). C+ does NOT introduce this race —
  it pre-exists at the provider layer — but C+'s gate refresh makes
  it observable across all caching consumers. See `bd-22q.15` for
  the fix (operation-scope mutex or version stamps).

These are real bugs, but C+ is intentionally smaller in scope so
that bd-22q.1 ships in 8h with a tight blast radius. `bd-22q.14`
layers the bridge fix and subscription pump on top; `bd-22q.15`
closes the cross-method race.

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
/// synchronously before returning. If an operation's error path
/// internally degrades the license state (today, only
/// `LicenseSeatProvider::heartbeat` degrade-on-failure), the
/// snapshot is refreshed from [`current_state`] before the error
/// is returned.
///
/// # Out-of-scope (tracked separately)
///
/// - Autonomous provider events (server-pushed revocation,
///   offline-verification re-checks) are NOT yet pumped into this
///   gate. Consumers that need to react to autonomous events
///   should call [`subscribe`] and update their own state on each
///   `LicenseEvent`. Tracked: `bd-22q.14`.
/// - Concurrent mutations of this facade are serialized only at
///   the `replace_state` granularity inside the provider, not
///   across the full SDK round-trip. A `validate`/`deactivate`
///   race can produce a transient over-permissioning window.
///   Tracked: `bd-22q.15`.
///
/// [`feature_gate`]: SpurLicense::feature_gate
/// [`subscribe`]: SpurLicense::subscribe
/// [`current_state`]: SpurLicense::current_state
```

Add to the `LicenseProvider` trait (location: top of trait block in
`crates/spur-license/src/provider.rs`):

```rust
/// Trait for license backend implementations.
///
/// # Err-arm state mutation contract
///
/// Implementations are free to mutate internal state on `Err`
/// returns (e.g., `LicenseSeatProvider::heartbeat` degrades state
/// to `Degraded` before returning the error). However, any such
/// Err-mutating arm MUST be paired with a corresponding
/// `feature_gate.update_state(&self.provider.current_state())`
/// call inside the matching method on `SpurLicense`. As of
/// 2026-04-29, only `heartbeat` follows this pattern. Adding a
/// new Err-mutating path without updating `SpurLicense` will
/// silently leave consumers' cached `Arc<FeatureGate>` stale.
```

## Concurrency notes

- `FeatureGate::update_state` is `&self` and uses `ArcSwap.store`
  with `SeqCst` ordering (verified: `arc-swap-1.9.1/src/lib.rs:477-491`).
  Concurrent stores have a global total order; the last writer wins
  in that order. The cost of a single `update_state` call is the cost
  of `build_snapshot` (policy resolution + quota merge + feature
  filtering — bounded but not microsecond-cheap) plus an `ArcSwap`
  pointer swap. For our call frequency (≤ once per mutating method
  call) this is irrelevant.
- **Cross-method races are NOT bounded by the next mutation.** A
  concurrent `validate` (slow SDK await) and `deactivate` (fast SDK
  await) can interleave such that validate's `replace_state(Pro)`
  lands AFTER deactivate's `replace_state(Inactive)`, producing a
  durable over-permissioning window until the next validate (~1
  hour). Heartbeat in this window reads `current_snapshot()` which
  is now Pro and **reinforces** rather than resyncs the bad state.
  This is a pre-existing provider-level hazard tracked under
  `bd-22q.15`. C+ does not fix it; C+ only ensures the gate stays
  consistent with whatever state the provider commits.
- **Asymmetric impact:** stale-deny is user friction; stale-allow
  is an entitlement leak. The cross-method race produces stale-allow
  in the validate/deactivate case above. `bd-22q.15` closes this.
- **`&new_state` is a correctness requirement, not an optimization.**
  `LicenseSeatProvider::current_state()` patches
  `Inactive | ConfigError → Active` when `sdk.current_license().is_some()`
  (`licenseseat.rs:148-161`). After a successful `deactivate`, the
  provider's RwLock holds `Inactive`, but `current_state()` would
  report `Active` if the SDK cache still contains the license. Using
  `&new_state` for Ok-path refreshes (where the new state is
  delivered directly from the provider's commit) avoids this hazard.
  Refactoring the Ok-path refreshes to use `current_state()` for
  uniformity would silently break deactivation. The heartbeat-Err
  path uses `current_state()` because (a) the Err-arm has no
  `next_state` value to refresh from, and (b) `degrade_current` sets
  status to `Degraded`, which the patching logic does NOT touch
  (it only patches `Inactive | ConfigError`).

## Test plan

### Unit tests in `crates/spur-license/tests/feature_gate_freshness.rs` (new)

For each test, construct `SpurLicense::from_provider(fake, gate)`
where `gate` is seeded at the Community baseline. All
entitlement-set assertions reference real
`spur_license::policy::FeatureKey` enum variants — pick any
Pro-tier-only key not present in the Community policy overlay
(e.g., `FeatureKey::BLOB_PRO_NAMESPACE_DELETION` per
`feature_key.rs:341`); call this `pro_only` in the test.

1. **`validate_pro_state_refreshes_cached_gate`**
   - `let cached = license.feature_gate();` (clones the Arc; the
     test now references the same allocation as Orchestrator
     would).
   - Assert `!cached.has(pro_only)` (Community baseline lacks it).
   - `fake.push_validate_result(Ok(LicenseState::active_validated(Plan::Pro, ...)))`.
   - `license.validate().await.unwrap();`
   - Assert `cached.has(pro_only)` — proves propagation through
     the **cached** Arc, not a fresh `feature_gate()` call.

2. **`activate_pro_state_refreshes_cached_gate`** — same shape via
   `fake.push_activate_result(...)`; call `license.activate("KEY")`.

3. **`deactivate_refreshes_cached_gate_to_inactive`** — same shape
   via `fake.push_deactivate_result(Ok(LicenseState::inactive("...")))`;
   assert all Pro entitlements drop AND that
   `cached.tier() == Tier::Community` after deactivate (also
   verifies the `current_state()` Inactive→Active patching hazard
   does NOT affect Ok-path refreshes — important regression test).

4. **`heartbeat_ok_refreshes_cached_gate`** — same shape via
   `fake.push_heartbeat_result(Ok(active state))`.

5. **`heartbeat_err_with_degrade_refreshes_cached_gate_to_provider_state`**
   — drives the new
   `FakeProvider::push_heartbeat_degraded_err(degraded_state, err)`
   (Task 2). The script entry mutates internal state to
   `degraded_state`, broadcasts, then returns `Err(err)`.
   - Capture `let cached = license.feature_gate();`.
   - Capture `let pre_snapshot_ptr = Arc::as_ptr(&cached.snapshot());`
     (pointer identity baseline).
   - `fake.push_heartbeat_degraded_err(specific_degraded_pro_state, err);`
   - Call `license.heartbeat().await.unwrap_err();`
   - Assert `Arc::as_ptr(&cached.snapshot()) != pre_snapshot_ptr`
     (a store happened — pointer identity, not value equality).
   - Assert the gate's resulting `EntitlementSnapshot` matches the
     snapshot built from `specific_degraded_pro_state` exactly
     (proves the refresh source was `provider.current_state()`,
     not some hardcoded fallback). Verify by either: (a) comparing
     `cached.snapshot().features` to the expected feature set
     derived from `specific_degraded_pro_state.features`, or
     (b) calling `cached.snapshot().source.plan` and asserting
     `Plan::Pro` (proves source-of-truth was the degraded-Pro state,
     not a synthesized Degraded fallback).

6. **`mutation_failure_without_state_change_keeps_gate_unchanged`** —
   `fake.push_validate_result(Err(...))`; capture
   `let pre_snapshot_ptr = Arc::as_ptr(&cached.snapshot());`; call
   `license.validate().await.unwrap_err();`; assert
   `Arc::as_ptr(&cached.snapshot()) == pre_snapshot_ptr` (pointer
   identity proves NO `update_state` call happened — value equality
   would be a weaker check that succeeds even on no-op refreshes).

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
   `push_heartbeat_degraded_err(state, err)` script entry. The
   `heartbeat()` impl on FakeProvider must, when this script entry
   is consumed, call `commit(state, LicenseEventKind::HeartbeatFailed)`
   (which mutates `self.state` and broadcasts), then return
   `Err(err)`. Mirrors `LicenseSeatProvider::heartbeat` Err-arm
   semantics. Gated behind the existing `test-support` feature
   flag. No new public types; just one new method on the FakeProvider
   impl.
3. **Task 3 — regression tests
   (`tests/feature_gate_freshness.rs`)**: write the 6 unit tests
   above; verify all pass; verify
   `cargo test -p spur-license --features test-support` is green.

## Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Cross-method `validate`/`deactivate` race produces durable over-permissioning | Medium | Out of scope for C+; documented honestly in concurrency notes; tracked under `bd-22q.15`. C+ does not introduce the race, only makes it observable. |
| Future provider adds Err-mutating path without updating facade | Low-Medium | (a) Doc-comment on `LicenseProvider` trait warning future implementers that Err-arms which mutate state must coordinate with the facade. (b) Risk-register entry in this spec. (c) `bd-22q.15`'s test infrastructure can serve as a regression harness for new mutating Err paths. |
| Future refactor "uniformly" replaces Ok-path `&new_state` with `current_state()` | Medium-High | Concurrency notes section explicitly documents this as a CORRECTNESS requirement (not an optimization) with the deactivation hazard quoted. Test 3 (`deactivate_refreshes_cached_gate_to_inactive`) is the regression guard. |
| `FakeProvider` extension introduces test-only API drift | Low | Mirror `LicenseSeatProvider::heartbeat` Err-arm behavior exactly (replace_state then return Err); gated behind `test-support` feature flag; document the parallel in test_support.rs. |
| Existing `community_smoke.rs` tests rely on stale-gate behavior | Very Low | Read tests; they query at construction, where seed is already correct. |
| Heartbeat-degrade gate refresh is observably "weak" because `FeatureGate` lacks `LicenseStatus` field | Low | Documented in "Why this is sufficient" section. Consumers needing degraded-status visibility can call `license.current_state().is_degraded()` directly. |

## Acceptance criteria

- [ ] All four mutating methods on `SpurLicense` refresh
  `feature_gate` per the spec (Ok always; heartbeat-Err
  additionally).
- [ ] Doc-comment added to `SpurLicense` matches the contract
  language above; cross-references `bd-22q.14` and `bd-22q.15`.
- [ ] Advisory doc-comment added to the `LicenseProvider` trait
  warning future implementers that any Err arm which mutates
  internal state must coordinate with the `SpurLicense` facade
  (the facade today only refreshes on `heartbeat`-Err; new
  Err-mutating arms require a paired facade update).
- [ ] 6 unit tests in `tests/feature_gate_freshness.rs` pass.
- [ ] No regression in existing `tests/community_smoke.rs`,
  `tests/fake_provider.rs`, `tests/invariants.rs`, or
  `tests/licenseseat_probe.rs`.
- [ ] No regression in `crates/spur-mcp/tests/feature_gate_plumbing.rs`.
- [ ] `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings`
  clean.
- [ ] `cargo fmt --all -- --check` clean.
- [ ] Beads issue `bd-22q.1` closed; `bd-22q.14` and `bd-22q.15`
  confirmed remain Open with review references.

## Out of scope (filed elsewhere)

- Bridge hydration + autonomous-event subscription pump:
  `bd-22q.14`.
- `validate()` Err-on-revocation leak: covered under `bd-22q.14`.
- Cross-method operation serialization in `LicenseSeatProvider`:
  `bd-22q.15`.
- CLI end-to-end smoke for `spur auth login`: deferred to manual
  verification under `bd-22q.4`.
- TUI gate refresh: already shipped in M1
  (`docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-m1-tui-gate-refresh.md`).

## References

- Origin filing: `docs/superpowers/plans/2026-04-28-tier-revamp-m1x-followup-spurlicense-gate-refresh.md`
- M1 plan (TUI fix shipped): `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-m1-tui-gate-refresh.md`
- Codex first-principles review (option-selection): `spur://continuation/beb3f286-1fdd-434c-8344-25998724a574`
- Codex spec-fidelity review: `spur://continuation/844b635e-8c1d-4002-ba19-6e6f1470bbe9`
- Gemini clarity review: `spur://continuation/5d83b8af-a0e4-40cc-8745-569100a7ef69`
- Kimi adversarial review: `spur://continuation/2aef1302-678a-42c5-bf79-c17430c7e648`
- Follow-up issues: `bd-22q.14` (bridge hydration + pump),
  `bd-22q.15` (cross-method serialization).
- Plan D dependency chain: `bd-22q.1 → bd-22q.14 → bd-22q.11 → bd-22q.12`
