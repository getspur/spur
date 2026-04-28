# bd-22q.1 — SpurLicense Feature-Gate Freshness (C+) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `SpurLicense::feature_gate()` return a fresh `Arc<FeatureGate>` to non-TUI consumers by refreshing the cached gate after every successful mutating call and on the heartbeat-Err degrade path.

**Architecture:** Each of the four mutating methods (`activate`, `validate`, `heartbeat`, `deactivate`) on the `SpurLicense` facade calls `self.feature_gate.update_state(&new_state)` after the provider returns Ok. `heartbeat`'s Err arm additionally refreshes from `provider.current_state()` because `LicenseSeatProvider` degrades state internally before propagating the error. Refresh propagates to caching consumers (Orchestrator, CLI, MCP) through the shared `Arc<FeatureGate>` allocation via lock-free `ArcSwap.store`.

**Tech Stack:** Rust, tokio (async), arc-swap (lock-free atomic snapshot), broadcast channels.

**Spec:** `docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md`

**Out of scope:** Bridge hydration & subscription pump (`bd-22q.14`); cross-method serialization (`bd-22q.15`); TUI gate refresh (already shipped in M1).

---

## File map

- **Modify**: `crates/spur-license/src/lib.rs` (lines 278-292: 4 mutating methods + new doc-comment on `SpurLicense` struct around line 213)
- **Modify**: `crates/spur-license/src/provider.rs` (lines 22-41: add advisory doc-comment on `LicenseProvider` trait)
- **Modify**: `crates/spur-license/src/test_support.rs` (extend `Script` enum + add `push_heartbeat_degraded_err`)
- **Create**: `crates/spur-license/tests/feature_gate_freshness.rs` (6 unit tests)

No production-side new files. No public API changes (only added doc-comments and a new test-only method on `FakeProvider`).

---

## Task 1: Write failing tests for Ok-path freshness (Tests 1–4)

**Files:**
- Create: `crates/spur-license/tests/feature_gate_freshness.rs`
- Reference (read-only): `crates/spur-license/tests/fake_provider.rs` for setup conventions
- Reference (read-only): `crates/spur-license/src/test_support.rs:38-89` for FakeProvider API

- [ ] **Step 1.1: Write the 4 Ok-path tests**

Create `crates/spur-license/tests/feature_gate_freshness.rs` with this exact content:

```rust
//! Regression tests for bd-22q.1: SpurLicense::feature_gate() must
//! return a fresh Arc<FeatureGate> after every successful mutating
//! call. The tests capture the cached Arc once and assert its
//! contents change in place — proving propagation through the
//! shared allocation, which is what caching consumers (Orchestrator,
//! CLI subcommands, MCP) rely on.
//!
//! Spec: docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md

use std::collections::BTreeSet;
use std::sync::Arc;

use spur_license::policy::FeatureKey;
use spur_license::test_support::FakeProvider;
use spur_license::{
    FeatureGate, LicenseState, Plan, SpurLicense,
};

fn build_license_with_community_seed() -> (Arc<FakeProvider>, SpurLicense, Arc<FeatureGate>) {
    let mut features = BTreeSet::new();
    features.insert("chat".to_string());
    let community = LicenseState::active_community(features);
    let fake = Arc::new(FakeProvider::new(community.clone()));
    let policy = spur_license::policy::PolicyResolver::with_default_overlay();
    let gate = Arc::new(FeatureGate::new(policy));
    gate.update_state(&community);
    let license = SpurLicense::from_provider(fake.clone(), gate.clone());
    (fake, license, gate)
}

fn pro_state() -> LicenseState {
    let mut feats = BTreeSet::new();
    // Real Pro-only feature key per spec; not in Community policy overlay.
    feats.insert("blob_pro_namespace_deletion".to_string());
    LicenseState::active_validated(Plan::Pro, feats)
}

#[tokio::test]
async fn validate_pro_state_refreshes_cached_gate() {
    let (fake, license, _gate_for_lifetime) = build_license_with_community_seed();
    let cached = license.feature_gate();
    assert!(
        !cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION),
        "Community baseline must not have Pro entitlement",
    );

    fake.push_validate_result(Ok(pro_state()));
    license.validate().await.expect("validate should succeed");

    assert!(
        cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION),
        "cached Arc<FeatureGate> must reflect Pro after validate",
    );
}

#[tokio::test]
async fn activate_pro_state_refreshes_cached_gate() {
    let (fake, license, _g) = build_license_with_community_seed();
    let cached = license.feature_gate();
    assert!(!cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));

    fake.push_activate_result(Ok(pro_state()));
    license.activate("PRO_KEY").await.expect("activate should succeed");

    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));
}

#[tokio::test]
async fn deactivate_refreshes_cached_gate_to_inactive() {
    let (fake, license, _g) = build_license_with_community_seed();
    // First activate to Pro so deactivate has something to clear.
    fake.push_activate_result(Ok(pro_state()));
    license.activate("PRO_KEY").await.unwrap();
    let cached = license.feature_gate();
    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));

    fake.push_deactivate_result(Ok(LicenseState::inactive("user requested")));
    license.deactivate().await.expect("deactivate should succeed");

    assert!(
        !cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION),
        "deactivate must drop Pro entitlement from cached gate",
    );
}

#[tokio::test]
async fn heartbeat_ok_refreshes_cached_gate() {
    let (fake, license, _g) = build_license_with_community_seed();
    let cached = license.feature_gate();
    assert!(!cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));

    fake.push_heartbeat_result(Ok(pro_state()));
    license.heartbeat().await.expect("heartbeat should succeed");

    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));
}
```

- [ ] **Step 1.2: Run the tests to verify they fail**

Run: `cargo test -p spur-license --features test-support --test feature_gate_freshness`

Expected: 4 tests, all FAIL with assertion failures like "cached Arc<FeatureGate> must reflect Pro after validate". The Community baseline assertion at the start of each test should PASS, and the post-mutation assertion should FAIL because `SpurLicense` does not yet refresh the gate.

- [ ] **Step 1.3: Commit the failing tests**

```bash
git add crates/spur-license/tests/feature_gate_freshness.rs
git commit -m "test(spur-license): bd-22q.1 add failing freshness tests for Ok-path mutations

Captures the cached Arc<FeatureGate> once and asserts entitlements
flip after each mutating method (validate/activate/deactivate/
heartbeat-Ok). Tests fail today because the facade does not
propagate state through the shared allocation. Implementation
follows in next commit.

Refs: bd-22q.1
Spec: docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md"
```

---

## Task 2: Implement Ok-path refresh in the 4 mutating methods

**Files:**
- Modify: `crates/spur-license/src/lib.rs` (lines 278-292)

- [ ] **Step 2.1: Replace the four mutating methods**

Open `crates/spur-license/src/lib.rs`. Find the existing block (currently lines 278-292):

```rust
    pub async fn activate(&self, key: &str) -> Result<LicenseState> {
        self.provider.activate(key).await
    }

    pub async fn validate(&self) -> Result<LicenseState> {
        self.provider.validate().await
    }

    pub async fn heartbeat(&self) -> Result<LicenseState> {
        self.provider.heartbeat().await
    }

    pub async fn deactivate(&self) -> Result<LicenseState> {
        self.provider.deactivate().await
    }
```

Replace the block with:

```rust
    pub async fn activate(&self, key: &str) -> Result<LicenseState> {
        let next = self.provider.activate(key).await?;
        self.feature_gate.update_state(&next);
        Ok(next)
    }

    pub async fn validate(&self) -> Result<LicenseState> {
        let next = self.provider.validate().await?;
        self.feature_gate.update_state(&next);
        Ok(next)
    }

    pub async fn heartbeat(&self) -> Result<LicenseState> {
        // NOTE: heartbeat-Err handling is added in a follow-up step; for
        // now this only covers the Ok path. See spec section "Concurrency
        // notes" for why &new_state is a correctness requirement, not an
        // optimization (LicenseSeatProvider::current_state() patches
        // Inactive→Active and would silently break deactivate-via-Ok).
        let next = self.provider.heartbeat().await?;
        self.feature_gate.update_state(&next);
        Ok(next)
    }

    pub async fn deactivate(&self) -> Result<LicenseState> {
        let next = self.provider.deactivate().await?;
        self.feature_gate.update_state(&next);
        Ok(next)
    }
```

- [ ] **Step 2.2: Run the Task 1 tests to verify they pass**

Run: `cargo test -p spur-license --features test-support --test feature_gate_freshness`

Expected: all 4 Ok-path tests PASS.

- [ ] **Step 2.3: Run the full spur-license test suite to check for regressions**

Run: `cargo test -p spur-license --features test-support`

Expected: all tests PASS, including `community_smoke`, `fake_provider`, `invariants`, `licenseseat_probe`.

- [ ] **Step 2.4: Commit the implementation**

```bash
git add crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): bd-22q.1 refresh feature_gate after Ok mutations

SpurLicense::{activate,validate,heartbeat,deactivate} now call
feature_gate.update_state(&new_state) after the provider returns
Ok, propagating fresh entitlements through the shared
Arc<FeatureGate> to all caching consumers (Orchestrator, CLI,
MCP). Heartbeat-Err arm refresh added in a follow-up commit.

Refs: bd-22q.1"
```

---

## Task 3: Test 6 — assert no spurious refresh on non-mutating Err

**Files:**
- Modify: `crates/spur-license/tests/feature_gate_freshness.rs`

- [ ] **Step 3.1: Add Test 6 with pointer-identity assertion**

Append to the bottom of `crates/spur-license/tests/feature_gate_freshness.rs`:

```rust
#[tokio::test]
async fn validate_err_keeps_gate_unchanged() {
    use spur_license::LicenseError;

    let (fake, license, _g) = build_license_with_community_seed();
    let cached = license.feature_gate();
    // Clone the Arc so we hold a strong reference to the OLD allocation
    // across the mutation. Arc::ptr_eq compares by allocation identity,
    // catching any spurious update_state call (value equality would
    // succeed even on a no-op refresh).
    let pre_arc = Arc::clone(&*cached.snapshot());

    fake.push_validate_result(Err(LicenseError::Provider(
        "transient network failure".into(),
    )));
    let result = license.validate().await;

    assert!(result.is_err(), "validate must propagate the provider Err");
    let post_arc = Arc::clone(&*cached.snapshot());
    assert!(
        Arc::ptr_eq(&pre_arc, &post_arc),
        "non-mutating validate-Err must NOT trigger update_state \
         (Arc::ptr_eq proves no store happened)",
    );
}
```

- [ ] **Step 3.2: Run the test**

Run: `cargo test -p spur-license --features test-support --test feature_gate_freshness validate_err_keeps_gate_unchanged`

Expected: PASS. The implementation in Task 2 uses `?` to short-circuit on Err, so `update_state` is never called on the Err path. This test locks in that invariant.

- [ ] **Step 3.3: Commit**

```bash
git add crates/spur-license/tests/feature_gate_freshness.rs
git commit -m "test(spur-license): bd-22q.1 lock in no-spurious-refresh on validate-Err

Pointer-identity assertion on FeatureGate::snapshot() catches any
future regression where a non-mutating Err arm accidentally calls
update_state with the same value (value equality would silently
pass). Anchors the spec's 'do not refresh non-mutating Err arms'
contract.

Refs: bd-22q.1"
```

---

## Task 4: Extend FakeProvider with `push_heartbeat_degraded_err`

**Files:**
- Modify: `crates/spur-license/src/test_support.rs`

- [ ] **Step 4.1: Add the new script entry variant**

Open `crates/spur-license/src/test_support.rs`. Find the `Script` struct (currently at lines 30-36):

```rust
#[derive(Default)]
struct Script {
    validate: VecDeque<Result<LicenseState>>,
    heartbeat: VecDeque<Result<LicenseState>>,
    activate: VecDeque<Result<LicenseState>>,
    deactivate: VecDeque<Result<LicenseState>>,
}
```

Replace the `heartbeat` field type with an enum that distinguishes plain Ok/Err from the new "commit-then-Err" shape. Add an enum just above the `Script` struct:

```rust
/// Outcome shape for a scripted `heartbeat` call.
///
/// Plain `Ok`/`Err` results match the same semantics as the other
/// mutating methods. `DegradedThenErr` mirrors
/// `LicenseSeatProvider::heartbeat`'s degrade-on-failure path: it
/// commits a degraded state to the provider's internal state and
/// broadcasts before returning the error. Used by bd-22q.1's
/// regression test to drive the heartbeat-Err refresh contract.
enum ScriptedHeartbeat {
    Ok(LicenseState),
    Err(LicenseError),
    DegradedThenErr {
        state: LicenseState,
        err: LicenseError,
    },
}

#[derive(Default)]
struct Script {
    validate: VecDeque<Result<LicenseState>>,
    heartbeat: VecDeque<ScriptedHeartbeat>,
    activate: VecDeque<Result<LicenseState>>,
    deactivate: VecDeque<Result<LicenseState>>,
}
```

- [ ] **Step 4.2: Update `push_heartbeat_result` to wrap into the new enum**

Find the existing method (currently around line 68):

```rust
    pub fn push_heartbeat_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().heartbeat.push_back(r);
    }
```

Replace with:

```rust
    pub fn push_heartbeat_result(&self, r: Result<LicenseState>) {
        let entry = match r {
            Ok(s) => ScriptedHeartbeat::Ok(s),
            Err(e) => ScriptedHeartbeat::Err(e),
        };
        self.script.lock().unwrap().heartbeat.push_back(entry);
    }

    /// Enqueue a scripted heartbeat outcome that commits `state` to
    /// the provider's internal state (and broadcasts a HeartbeatFailed
    /// event) before returning `Err(err)`. Mirrors
    /// `LicenseSeatProvider::heartbeat` degrade-on-failure (`degrade_current`
    /// + `replace_state` + return Err). Required by bd-22q.1's
    /// regression test for the heartbeat-Err refresh contract.
    pub fn push_heartbeat_degraded_err(&self, state: LicenseState, err: LicenseError) {
        self.script
            .lock()
            .unwrap()
            .heartbeat
            .push_back(ScriptedHeartbeat::DegradedThenErr { state, err });
    }
```

- [ ] **Step 4.3: Update the `heartbeat` impl on `FakeProvider` to consume the new enum**

Find the existing impl (currently around lines 163-171):

```rust
    async fn heartbeat(&self) -> Result<LicenseState> {
        self.heartbeat_calls.fetch_add(1, Ordering::Relaxed);
        let scripted = self.script.lock().unwrap().heartbeat.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::HeartbeatOk)),
            Some(Err(e)) => Err(e),
            None => Ok(self.snapshot()),
        }
    }
```

Replace with:

```rust
    async fn heartbeat(&self) -> Result<LicenseState> {
        self.heartbeat_calls.fetch_add(1, Ordering::Relaxed);
        let scripted = self.script.lock().unwrap().heartbeat.pop_front();
        match scripted {
            Some(ScriptedHeartbeat::Ok(next)) => {
                Ok(self.commit(next, LicenseEventKind::HeartbeatOk))
            }
            Some(ScriptedHeartbeat::Err(e)) => Err(e),
            Some(ScriptedHeartbeat::DegradedThenErr { state, err }) => {
                // Commit the degraded state (mutating internal state and
                // broadcasting), then propagate the error. Mirrors
                // LicenseSeatProvider::heartbeat's degrade_current path.
                self.commit(state, LicenseEventKind::HeartbeatFailed);
                Err(err)
            }
            None => Ok(self.snapshot()),
        }
    }
```

- [ ] **Step 4.4: Verify compilation and that existing tests still pass**

Run: `cargo test -p spur-license --features test-support`

Expected: all tests PASS. The `push_heartbeat_result` API is unchanged externally (still takes `Result<LicenseState>`); the rewrap is internal. Existing callers in `tests/fake_provider.rs` and `tests/invariants.rs` continue to work without modification.

- [ ] **Step 4.5: Commit**

```bash
git add crates/spur-license/src/test_support.rs
git commit -m "test(spur-license): bd-22q.1 add FakeProvider::push_heartbeat_degraded_err

Adds a script entry shape (ScriptedHeartbeat::DegradedThenErr) that
mirrors LicenseSeatProvider::heartbeat's degrade-on-failure path:
commits a degraded state to internal state and broadcasts before
returning Err. Required by bd-22q.1's regression test for the
heartbeat-Err refresh contract. Existing push_heartbeat_result
callers are unaffected.

Refs: bd-22q.1"
```

---

## Task 5: Test 5 + heartbeat-Err refresh implementation

**Files:**
- Modify: `crates/spur-license/tests/feature_gate_freshness.rs`
- Modify: `crates/spur-license/src/lib.rs` (heartbeat method)

- [ ] **Step 5.1: Write the failing test for heartbeat-Err refresh**

Append to the bottom of `crates/spur-license/tests/feature_gate_freshness.rs`:

```rust
#[tokio::test]
async fn heartbeat_err_with_degrade_refreshes_cached_gate_to_provider_state() {
    use spur_license::{LicenseError, LicenseStatus};

    let (fake, license, _g) = build_license_with_community_seed();
    // Activate Pro first so heartbeat has degraded-Pro state to commit.
    fake.push_activate_result(Ok(pro_state()));
    license.activate("PRO_KEY").await.unwrap();
    let cached = license.feature_gate();
    assert!(cached.has(FeatureKey::BLOB_PRO_NAMESPACE_DELETION));
    // Clone the OLD Arc so we hold a strong reference across the
    // mutation; Arc::ptr_eq below proves a store happened.
    let pre_arc = Arc::clone(&*cached.snapshot());

    // Build a degraded-Pro state: Pro plan with degraded status.
    let mut degraded_pro = pro_state();
    degraded_pro.status = LicenseStatus::Degraded;
    degraded_pro.status_text = "heartbeat failed".into();

    fake.push_heartbeat_degraded_err(
        degraded_pro.clone(),
        LicenseError::Provider("heartbeat failed: simulated".into()),
    );
    let result = license.heartbeat().await;
    assert!(result.is_err(), "heartbeat must propagate the provider Err");

    // 1. A store happened (Arc allocation identity changed).
    let post_arc = Arc::clone(&*cached.snapshot());
    assert!(
        !Arc::ptr_eq(&pre_arc, &post_arc),
        "heartbeat-Err must trigger update_state on the cached gate",
    );

    // 2. The new snapshot was sourced from provider.current_state(),
    //    which after FakeProvider's commit holds `degraded_pro`. Prove
    //    this by checking the snapshot's source plan is Pro (the
    //    degraded state's plan), NOT a synthesized fallback.
    assert_eq!(
        post_arc.source.plan,
        Plan::Pro,
        "gate's snapshot.source.plan must reflect the degraded-Pro state \
         (proves refresh source was provider.current_state(), not a hardcoded fallback)",
    );

    // 3. Sanity check: license.current_state() also reports degraded.
    let live = license.current_state();
    assert!(
        matches!(live.status, LicenseStatus::Degraded),
        "license.current_state() must report Degraded after heartbeat-Err-with-degrade",
    );
}
```

- [ ] **Step 5.2: Run the test to verify it fails**

Run: `cargo test -p spur-license --features test-support --test feature_gate_freshness heartbeat_err_with_degrade`

Expected: FAIL on the pointer-inequality assertion (`pre_snapshot_ptr` equals `post_snapshot_ptr` because the current `heartbeat` impl uses `?` to short-circuit on Err and never calls `update_state`).

- [ ] **Step 5.3: Implement the heartbeat-Err refresh**

Open `crates/spur-license/src/lib.rs`. Find the `heartbeat` method (currently using `?` short-circuit):

```rust
    pub async fn heartbeat(&self) -> Result<LicenseState> {
        // NOTE: heartbeat-Err handling is added in a follow-up step; for
        // now this only covers the Ok path. See spec section "Concurrency
        // notes" for why &new_state is a correctness requirement, not an
        // optimization (LicenseSeatProvider::current_state() patches
        // Inactive→Active and would silently break deactivate-via-Ok).
        let next = self.provider.heartbeat().await?;
        self.feature_gate.update_state(&next);
        Ok(next)
    }
```

Replace with:

```rust
    pub async fn heartbeat(&self) -> Result<LicenseState> {
        // Ok-path: refresh from `&next` directly (the value just delivered
        // from the provider's commit). Err-path: refresh from
        // `provider.current_state()` because LicenseSeatProvider's
        // degrade_current writes to provider state BEFORE returning Err,
        // so current_state() is the canonical post-mutation snapshot.
        //
        // SAFETY of current_state() here: degrade_current sets status to
        // Degraded, which LicenseSeatProvider::current_state()'s
        // Inactive|ConfigError→Active patching does NOT touch
        // (licenseseat.rs:148-161). For the Ok-path we use `&next`
        // because deactivate-via-Ok would otherwise be silently mis-
        // patched back to Active. See spec "Concurrency notes" for the
        // full hazard analysis.
        match self.provider.heartbeat().await {
            Ok(next) => {
                self.feature_gate.update_state(&next);
                Ok(next)
            }
            Err(err) => {
                self.feature_gate
                    .update_state(&self.provider.current_state());
                Err(err)
            }
        }
    }
```

- [ ] **Step 5.4: Run the heartbeat-Err test to verify it passes**

Run: `cargo test -p spur-license --features test-support --test feature_gate_freshness heartbeat_err_with_degrade`

Expected: PASS.

- [ ] **Step 5.5: Run the full freshness test suite**

Run: `cargo test -p spur-license --features test-support --test feature_gate_freshness`

Expected: all 6 tests PASS.

- [ ] **Step 5.6: Commit**

```bash
git add crates/spur-license/tests/feature_gate_freshness.rs crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): bd-22q.1 refresh feature_gate on heartbeat-Err degrade

LicenseSeatProvider::heartbeat degrades provider state internally
(via degrade_current → replace_state) before propagating Err. The
facade now mirrors that contract by refreshing the cached
Arc<FeatureGate> from provider.current_state() in the Err arm.

Test asserts (a) pointer-identity change on the snapshot to prove
update_state was called, and (b) snapshot.source.plan reflects the
specific degraded state (not a synthesized fallback) to prove
provider.current_state() was the refresh source.

Refs: bd-22q.1"
```

---

## Task 6: Doc-comments + final verification

**Files:**
- Modify: `crates/spur-license/src/lib.rs` (add doc-comment above `pub struct SpurLicense`)
- Modify: `crates/spur-license/src/provider.rs` (add doc-comment above `pub trait LicenseProvider`)

- [ ] **Step 6.1: Add the SpurLicense doc-comment**

Open `crates/spur-license/src/lib.rs`. Find the `SpurLicense` struct (currently at line 212-216):

```rust
#[derive(Clone)]
pub struct SpurLicense {
    provider: Arc<dyn LicenseProvider>,
    feature_gate: Arc<FeatureGate>,
}
```

Replace the lines from `#[derive(Clone)]` down to and including the closing `}` with:

```rust
/// Facade over a [`LicenseProvider`] paired with a shared
/// `Arc<FeatureGate>`.
///
/// # Freshness contract
///
/// The `Arc<FeatureGate>` returned by [`feature_gate`] is a shared
/// entitlement snapshot. Successful mutating calls ([`activate`],
/// [`validate`], [`heartbeat`], [`deactivate`]) refresh the snapshot
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
///   `LicenseEvent`. Tracked: bd-22q.14.
/// - Concurrent mutations of this facade are serialized only at
///   the `replace_state` granularity inside the provider, not
///   across the full SDK round-trip. A `validate`/`deactivate`
///   race can produce a transient over-permissioning window.
///   Tracked: bd-22q.15.
///
/// [`feature_gate`]: SpurLicense::feature_gate
/// [`activate`]: SpurLicense::activate
/// [`validate`]: SpurLicense::validate
/// [`heartbeat`]: SpurLicense::heartbeat
/// [`deactivate`]: SpurLicense::deactivate
/// [`subscribe`]: SpurLicense::subscribe
/// [`current_state`]: SpurLicense::current_state
#[derive(Clone)]
pub struct SpurLicense {
    provider: Arc<dyn LicenseProvider>,
    feature_gate: Arc<FeatureGate>,
}
```

- [ ] **Step 6.2: Add the LicenseProvider trait doc-comment**

Open `crates/spur-license/src/provider.rs`. Find the trait definition (currently at line 22):

```rust
#[async_trait]
pub trait LicenseProvider: Send + Sync {
```

Insert above `#[async_trait]`:

```rust
/// Trait for license backend implementations.
///
/// # Err-arm state mutation contract
///
/// Implementations are free to mutate internal state on `Err`
/// returns (e.g., `LicenseSeatProvider::heartbeat` degrades state
/// to `Degraded` before returning the error). However, any such
/// Err-mutating arm MUST be paired with a corresponding refresh
/// inside the matching method on [`crate::SpurLicense`]; the
/// facade today only refreshes on `heartbeat`-Err. Adding a new
/// Err-mutating path without updating `SpurLicense` will silently
/// leave consumers' cached `Arc<FeatureGate>` stale. See
/// `docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md`
/// for the full freshness contract.
#[async_trait]
pub trait LicenseProvider: Send + Sync {
```

- [ ] **Step 6.3: Run cargo fmt to normalize formatting**

Run: `cargo fmt --all`

Expected: no errors. (May silently rewrite long doc-comment lines, which is fine.)

- [ ] **Step 6.4: Run clippy with denials**

Run: `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings`

Expected: no warnings, no errors. If `cargo clippy` flags an unused-import warning in the new test file (e.g., for `LicenseStatus` if Test 5 didn't end up using it via match-via-status), remove the unused import. If clippy flags `dead_code` on `ScriptedHeartbeat` variants because not all are used in tests, suppress with `#[allow(dead_code)]` on the enum or accept the lint (the variants are part of the test surface).

- [ ] **Step 6.5: Run the full spur-license test suite**

Run: `cargo test -p spur-license --features test-support`

Expected: all tests PASS, including the new 6 in `feature_gate_freshness`.

- [ ] **Step 6.6: Run the cross-crate consumer test**

Run: `cargo test -p spur-mcp --test feature_gate_plumbing`

Expected: all tests PASS. This crate consumes `license.feature_gate()` in its plumbing tests; we must not regress.

- [ ] **Step 6.7: Run cargo fmt --check to confirm clean formatting**

Run: `cargo fmt --all -- --check`

Expected: silent (exit code 0).

- [ ] **Step 6.8: Commit**

```bash
git add crates/spur-license/src/lib.rs crates/spur-license/src/provider.rs
git commit -m "docs(spur-license): bd-22q.1 freshness contract on SpurLicense + LicenseProvider

Adds the Freshness contract doc-comment to SpurLicense (synchronous
refresh on Ok-path mutations and on heartbeat-Err degrade), with
explicit pointers to follow-up issues bd-22q.14 (autonomous-event
pump) and bd-22q.15 (cross-method serialization).

Also adds an Err-arm state mutation advisory on the
LicenseProvider trait warning future implementers that any
new Err-mutating arm must be paired with a matching SpurLicense
update.

Refs: bd-22q.1"
```

---

## Self-review checklist (run before handing off)

- [ ] All six tests in `feature_gate_freshness.rs` pass.
- [ ] `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings` clean.
- [ ] `cargo fmt --all -- --check` clean.
- [ ] `cargo test -p spur-license --features test-support` all pass.
- [ ] `cargo test -p spur-mcp --test feature_gate_plumbing` all pass.
- [ ] `cargo test -p spur-cli --tests` all pass (CLI consumes `license.feature_gate()` at multiple sites; no regression expected since we only ADD refreshes, not remove behavior).
- [ ] No code in `crates/spur-tui` was modified (TUI gate refresh is shipped under M1; this work is non-TUI only).
- [ ] No new public API surface added to `SpurLicense` or `FeatureGate`. Only added doc-comments and a new `FakeProvider` test-only method.
- [ ] Beads issue `bd-22q.1` ready to close. `bd-22q.14` and `bd-22q.15` confirmed remain Open.

## Handoff

After all 6 tasks complete, the implementation is done. Run the spec's full acceptance criteria (mapped to `## Acceptance criteria` in the spec) one last time, then mark `bd-22q.1` closed.
