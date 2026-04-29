# bd-22q.15 — LicenseSeatProvider Cross-Method Serialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `tokio::sync::Mutex<()>` operation-level serialization to `LicenseSeatProvider` so that two of `{activate, validate, heartbeat, deactivate}` cannot interleave their SDK calls and `replace_state` commits in the wrong order. Closes the durable over-permissioning race that `bd-22q.1`'s gate refresh exposed.

**Architecture:** Add `operation_lock: Arc<tokio::sync::Mutex<()>>` to `LicenseSeatProvider`. Each of the four mutating methods acquires the lock at entry, holds it across the SDK call AND the subsequent `replace_state`, releases on return/panic. tokio's documented FIFO acquisition gives "commit-order = acquisition-order = SDK-call-start-order" transitively. Read paths (`current_state`, `subscribe`, `has_entitlement`, `current_snapshot`, the autonomous bridge) do NOT acquire the lock — they observe a best-effort snapshot, documented as eventually consistent.

**Tech Stack:** Rust, tokio (`sync::Mutex`, `time::pause`, `time::sleep`, `task::yield_now`, `time::advance`), `licenseseat` 0.5.3 SDK.

**Spec:** `docs/superpowers/specs/2026-04-29-bd-22q-15-licenseseat-cross-method-serialization-design.md` (commit `e0b93fef`).

**Out of scope:**
- Bridge hydration + autonomous-event subscription pump (`bd-22q.14`).
- `current_state()` SDK-cache vs. provider-state mixing (subsumed under `bd-22q.14`).
- `std::sync::RwLock` poison-swallowing in `replace_state` — file as `bd-22q.16` in Task 7.
- SDK trait abstraction for direct race-reproduction tests — file as `bd-22q.17` in Task 7.
- TUI-side observability of the new ordering guarantee.

---

## File map

- **Modify**: `crates/spur-license/src/licenseseat.rs`
  - Add `operation_lock: Arc<tokio::sync::Mutex<()>>` to struct (line 48–54).
  - Initialize in `new()` (line 57–82).
  - Acquire `operation_lock` at entry of all four mutating methods (lines 184–285).
  - Add `#[cfg(test)]` `pub(crate) operation_lock_handle()` accessor.
  - Add struct-level doc-comment block (per spec § "Doc-comment contract — `LicenseSeatProvider`").
  - Add `#[cfg(test)] mod cross_method_race { ... }` containing all 6 tests.
- **Modify**: `crates/spur-license/src/lib.rs`
  - Edit existing `SpurLicense` doc-comment (line 212–245): delete only the `bd-22q.15` bullet from `# Out-of-scope (tracked separately)`, append a new `# Concurrency` section.
- **Modify**: `crates/spur-license/src/provider.rs`
  - Append the new `# Cross-method serialization (advisory)` paragraph to the existing trait advisory (line 22–35).

No new top-level files. No new public API. The `pub(crate)` accessor is `#[cfg(test)]`-only and never visible to dependent crates.

---

## Task 1: Failing primitive sanity test + stub for accessor (RED)

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs`

- [ ] **Step 1.1: Add the `cross_method_race` test module skeleton**

At the BOTTOM of `crates/spur-license/src/licenseseat.rs` (after the existing `#[cfg(test)] mod dedup_tests`), add:

```rust
#[cfg(test)]
mod cross_method_race {
    //! Lock-discipline canaries for bd-22q.15. These tests verify that
    //! `LicenseSeatProvider`'s mutating methods participate in the
    //! `operation_lock` discipline. They do NOT directly drive the
    //! validate-vs-deactivate race scenario — that requires SDK-mock
    //! infrastructure deferred to bd-22q.17. Instead, they prove:
    //!   1. The lock primitive serializes (sanity).
    //!   2. Each of the four mutating methods blocks on an externally-
    //!      held `operation_lock`, proving they acquire it.
    //!   3. Under tokio's documented FIFO semantics, lock-acquisition
    //!      order matches request order.
    //!
    //! Spec: docs/superpowers/specs/2026-04-29-bd-22q-15-licenseseat-cross-method-serialization-design.md
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Test 1: tokio::sync::Mutex primitive sanity. Decoupled from
    /// LicenseSeatProvider; verifies that two clones of an
    /// `Arc<Mutex<()>>` serialize.
    #[tokio::test(start_paused = true)]
    async fn mutex_serializes_concurrent_acquirers() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let a = lock.clone().lock_owned().await;

        let lock_clone = lock.clone();
        let task_b = tokio::spawn(async move {
            let _g = lock_clone.lock().await;
            tokio::time::Instant::now()
        });

        // While A holds, B is queued. Advance virtual time and confirm
        // B has not yet acquired by polling-once: the spawned task
        // must remain pending.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(50)).await;
        // (No way to directly assert pending without try-join; the
        //  drop sequence below is the test.)

        let a_release = tokio::time::Instant::now();
        drop(a);
        let b_acquire = task_b.await.unwrap();
        assert!(
            b_acquire >= a_release,
            "B should acquire only after A released"
        );
    }

    /// Test 2 (STUB — fails to compile until Task 2 adds the accessor).
    #[tokio::test]
    async fn activate_blocks_on_externally_held_operation_lock() {
        let provider = LicenseSeatProvider::new(
            "test-key".to_string(),
            "test-product".to_string(),
        );
        let _external_lock = provider.operation_lock_handle().lock_owned().await;
        // Body intentionally left as a stub for Task 1 RED. Task 3 fills it in.
    }
}
```

- [ ] **Step 1.2: Verify RED**

Run: `cargo build -p spur-license --tests 2>&1 | tail -20`

Expected: compile error like `no method named 'operation_lock_handle' found for struct 'LicenseSeatProvider'`. This is the RED state.

- [ ] **Step 1.3: Commit RED**

```
git add crates/spur-license/src/licenseseat.rs
git commit -m "test(spur-license): bd-22q.15 add cross_method_race module + Test 1 sanity (RED)"
```

The compile failure on Test 2's stub is the RED-marker.

---

## Task 2: Add `operation_lock` field + `pub(crate)` accessor (GREEN)

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs`

- [ ] **Step 2.1: Extend the struct with `operation_lock`**

In `crates/spur-license/src/licenseseat.rs`, locate the `LicenseSeatProvider` struct definition (line 48–54):

```rust
#[derive(Clone)]
pub struct LicenseSeatProvider {
    sdk: LicenseSeat,
    state: Arc<RwLock<LicenseState>>,
    events_tx: broadcast::Sender<LicenseEvent>,
    refresh_policy: RefreshPolicy,
}
```

Replace with:

```rust
#[derive(Clone)]
pub struct LicenseSeatProvider {
    sdk: LicenseSeat,
    state: Arc<RwLock<LicenseState>>,
    /// Cross-method operation serialization (bd-22q.15). Acquired at
    /// entry of every mutating method; held across SDK + replace_state.
    /// Reads do NOT acquire this lock.
    operation_lock: Arc<tokio::sync::Mutex<()>>,
    events_tx: broadcast::Sender<LicenseEvent>,
    refresh_policy: RefreshPolicy,
}
```

- [ ] **Step 2.2: Initialize in `new()`**

Locate `pub fn new(...)` (line 57). Inside, find:

```rust
let provider = Self {
    sdk,
    state: Arc::new(RwLock::new(initial_state)),
    events_tx,
    refresh_policy,
};
```

Replace with:

```rust
let provider = Self {
    sdk,
    state: Arc::new(RwLock::new(initial_state)),
    operation_lock: Arc::new(tokio::sync::Mutex::new(())),
    events_tx,
    refresh_policy,
};
```

- [ ] **Step 2.3: Add the `#[cfg(test)] pub(crate)` accessor**

Insert after the existing `impl LicenseSeatProvider { ... }` block (after line 144, where `degrade_current` ends):

```rust
#[cfg(test)]
impl LicenseSeatProvider {
    /// In-crate-test-only handle to the operation lock for bd-22q.15
    /// cross_method_race tests. NEVER expose `pub`: external crates
    /// could acquire the lock and stall production mutations.
    pub(crate) fn operation_lock_handle(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.operation_lock)
    }
}
```

- [ ] **Step 2.4: Verify GREEN**

Run: `cargo test -p spur-license --lib cross_method_race::mutex_serializes_concurrent_acquirers 2>&1 | tail -20`

Expected: 1 test passes (the primitive sanity). `activate_blocks_on_externally_held_operation_lock` is still a no-assertion stub; it should also pass trivially.

Run: `cargo test -p spur-license --lib 2>&1 | tail -10`

Expected: all existing in-crate tests still pass. No regression.

- [ ] **Step 2.5: Commit GREEN**

```
git add crates/spur-license/src/licenseseat.rs
git commit -m "feat(spur-license): bd-22q.15 add operation_lock field + #[cfg(test)] accessor"
```

---

## Task 3: Failing per-method blocking tests (RED)

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs` (extend `cross_method_race` module)

- [ ] **Step 3.1: Replace the Test 2 stub + add Tests 3, 4, 5**

In the `cross_method_race` module, replace the stub body of `activate_blocks_on_externally_held_operation_lock` AND add three more tests:

```rust
/// Test 2: activate() acquires operation_lock at entry.
#[tokio::test(start_paused = true)]
async fn activate_blocks_on_externally_held_operation_lock() {
    let provider = LicenseSeatProvider::new(
        "test-key".to_string(),
        "test-product".to_string(),
    );
    let external_lock = provider.operation_lock_handle().lock_owned().await;

    let provider_clone = provider.clone();
    let activate_task = tokio::spawn(async move {
        provider_clone.activate("X").await
    });

    // Yield so activate_task is polled and queues on the lock.
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    // Task should still be pending — operation_lock is held externally.
    assert!(
        !activate_task.is_finished(),
        "activate() must block on externally-held operation_lock"
    );

    // Release; activate proceeds. We don't care about the outcome
    // (SDK error is fine); we care that the task UNBLOCKS.
    drop(external_lock);
    let _ = activate_task.await;
}

/// Test 3: validate() acquires operation_lock at entry.
#[tokio::test(start_paused = true)]
async fn validate_blocks_on_externally_held_operation_lock() {
    let provider = LicenseSeatProvider::new(
        "test-key".to_string(),
        "test-product".to_string(),
    );
    let external_lock = provider.operation_lock_handle().lock_owned().await;

    let provider_clone = provider.clone();
    let task = tokio::spawn(async move { provider_clone.validate().await });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    assert!(
        !task.is_finished(),
        "validate() must block on externally-held operation_lock"
    );

    drop(external_lock);
    let _ = task.await;
}

/// Test 4: heartbeat() acquires operation_lock at entry.
#[tokio::test(start_paused = true)]
async fn heartbeat_blocks_on_externally_held_operation_lock() {
    let provider = LicenseSeatProvider::new(
        "test-key".to_string(),
        "test-product".to_string(),
    );
    let external_lock = provider.operation_lock_handle().lock_owned().await;

    let provider_clone = provider.clone();
    let task = tokio::spawn(async move { provider_clone.heartbeat().await });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    assert!(
        !task.is_finished(),
        "heartbeat() must block on externally-held operation_lock"
    );

    drop(external_lock);
    let _ = task.await;
}

/// Test 5: deactivate() acquires operation_lock at entry.
#[tokio::test(start_paused = true)]
async fn deactivate_blocks_on_externally_held_operation_lock() {
    let provider = LicenseSeatProvider::new(
        "test-key".to_string(),
        "test-product".to_string(),
    );
    let external_lock = provider.operation_lock_handle().lock_owned().await;

    let provider_clone = provider.clone();
    let task = tokio::spawn(async move { provider_clone.deactivate().await });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    assert!(
        !task.is_finished(),
        "deactivate() must block on externally-held operation_lock"
    );

    drop(external_lock);
    let _ = task.await;
}
```

- [ ] **Step 3.2: Verify RED**

Run: `cargo test -p spur-license --lib cross_method_race 2>&1 | tail -30`

Expected: `mutex_serializes_concurrent_acquirers` passes. The four `*_blocks_on_externally_held_operation_lock` tests **fail** with assertion `"... must block on externally-held operation_lock"` because the mutating methods don't yet acquire the lock — they bypass it and run the SDK call immediately, finishing the spawned task while the external lock is still held.

If a method's SDK call hangs (e.g., real network attempt) instead of fast-failing, the task may stay running past the 100ms virtual advance — that's still "not finished" and the test would falsely PASS in that path. Mitigation: the spec covers this in Task 3's design — we expect the SDK call to fast-fail synchronously OR fail quickly because the test API key is non-routable; either way, without the operation_lock acquisition, the task should finish (Err) within the 100ms virtual advance. If you observe flakiness, switch to checking that the task FINISHES (with an SDK error) DURING the held lock — that's the inverse assertion proving lock absence.

- [ ] **Step 3.3: Commit RED**

```
git add crates/spur-license/src/licenseseat.rs
git commit -m "test(spur-license): bd-22q.15 add per-method blocking tests (RED)"
```

---

## Task 4: Acquire lock at mutating-method entry (GREEN)

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs`

- [ ] **Step 4.1: Add `let _guard = self.operation_lock.lock().await;` to all four mutating methods**

Locate each of the four methods (lines 184, 202, 256, 277). Add the guard line as the **first statement** in each method body:

```rust
async fn activate(&self, key: &str) -> Result<LicenseState> {
    let _guard = self.operation_lock.lock().await;
    let response = self
        .sdk
        .activate(key)
        // ... existing body unchanged ...
}

async fn validate(&self) -> Result<LicenseState> {
    let _guard = self.operation_lock.lock().await;
    let result = self
        .sdk
        .validate()
        // ... existing body unchanged ...
}

async fn heartbeat(&self) -> Result<LicenseState> {
    let _guard = self.operation_lock.lock().await;
    match self.sdk.heartbeat().await {
        // ... existing body unchanged ...
    }
}

async fn deactivate(&self) -> Result<LicenseState> {
    let _guard = self.operation_lock.lock().await;
    self.sdk
        .deactivate()
        // ... existing body unchanged ...
}
```

The `_guard` must be `let _guard = ...` (named binding), NOT `let _ = ...` — `let _ =` drops the guard immediately, defeating the lock. **This is a critical correctness invariant.** A code review checklist item.

- [ ] **Step 4.2: Verify GREEN**

Run: `cargo test -p spur-license --lib cross_method_race 2>&1 | tail -10`

Expected: all 5 tests pass. The four blocking tests now correctly observe that the spawned tasks remain pending while the external lock is held.

- [ ] **Step 4.3: Run full crate tests + clippy**

Run:
```
cargo test -p spur-license --features test-support 2>&1 | tail -20
cargo clippy -p spur-license --all-targets --features test-support -- -D warnings 2>&1 | tail -20
```

Expected: all green. No regression in `tests/community_smoke.rs`, `tests/fake_provider.rs`, `tests/invariants.rs`, `tests/licenseseat_probe.rs`, `tests/feature_gate_freshness.rs`, `tests/emission_audit.rs`.

- [ ] **Step 4.4: Commit GREEN**

```
git add crates/spur-license/src/licenseseat.rs
git commit -m "feat(spur-license): bd-22q.15 acquire operation_lock at mutating-method entry"
```

---

## Task 5: FIFO test using virtual-time cascade (regression canary)

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs` (extend `cross_method_race` module)

- [ ] **Step 5.1: Add the FIFO regression canary**

In the `cross_method_race` module, append:

```rust
/// Test 6: tokio FIFO discipline regression canary. Uses virtual
/// time staggering (NOT tokio::sync::Barrier — barrier release is
/// non-deterministic in waker-queue order) to ensure three tasks
/// queue on the lock in a known order, then asserts the lock
/// releases in that order under FIFO.
#[tokio::test(start_paused = true)]
async fn fifo_ordering_via_virtual_time_cascade() {
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let order = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let holder = lock.clone().lock_owned().await;

    let handles: Vec<_> = (0..3)
        .map(|i| {
            let lock = lock.clone();
            let order = order.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis((i as u64 + 1) * 10)).await;
                let _g = lock.lock().await;
                order.lock().unwrap().push(i);
            })
        })
        .collect();

    // Yield so all three tasks reach their sleep call.
    for _ in 0..6 {
        tokio::task::yield_now().await;
    }
    // Advance past the longest sleep so all three tasks are queued.
    tokio::time::advance(Duration::from_millis(100)).await;
    for _ in 0..6 {
        tokio::task::yield_now().await;
    }
    // Release; FIFO acquisition begins.
    drop(holder);

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
}
```

- [ ] **Step 5.2: Verify GREEN**

Run: `cargo test -p spur-license --lib cross_method_race::fifo_ordering_via_virtual_time_cascade 2>&1 | tail -10`

Expected: passes. Asserts `[0, 1, 2]` matches the deterministic queue order under tokio FIFO.

- [ ] **Step 5.3: Commit GREEN**

```
git add crates/spur-license/src/licenseseat.rs
git commit -m "test(spur-license): bd-22q.15 add FIFO regression canary (virtual-time cascade)"
```

---

## Task 6: Doc-comment updates + full-suite check

**Files:**
- Modify: `crates/spur-license/src/lib.rs`
- Modify: `crates/spur-license/src/provider.rs`
- Modify: `crates/spur-license/src/licenseseat.rs`

- [ ] **Step 6.1: Update `SpurLicense` doc-comment in `lib.rs`**

In `crates/spur-license/src/lib.rs`, locate the existing `SpurLicense` doc-comment (line 212–245). Inside the `# Out-of-scope (tracked separately)` section, **delete only the `bd-22q.15` bullet** (currently lines 233–237):

```rust
/// - Concurrent mutations of this facade are serialized only at
///   the `replace_state` granularity inside the provider, not
///   across the full SDK round-trip. A `validate`/`deactivate`
///   race can produce a transient over-permissioning window.
///   Tracked: bd-22q.15.
```

(Leave the `bd-22q.14` bullet intact.) Then **append a new `# Concurrency` section** below the `# Out-of-scope` block, before the `[`feature_gate`]` link references:

```rust
/// # Concurrency
///
/// Mutating calls (`activate`, `validate`, `heartbeat`,
/// `deactivate`) are serialized end-to-end inside the
/// underlying `LicenseSeatProvider` via an internal
/// `tokio::sync::Mutex`. Two concurrent calls commit in the
/// order they acquire the mutex (FIFO under tokio); the cached
/// `Arc<FeatureGate>` reflects the LATER-committed operation's
/// state.
///
/// Reads (`current_state`, `subscribe`, `has_entitlement`,
/// `feature_gate`) are unsynchronized with mutations and return
/// a best-effort snapshot. In particular, `current_state()` may
/// observe a mix of SDK-cache state (post-mutation) and provider
/// RwLock state (pre-mutation) during an in-flight mutation
/// because the SDK mutates its own cache before SPUR's
/// `replace_state` runs. The mismatch closes on commit. Callers
/// that require strict coherence should subscribe to
/// `LicenseEvent`s and react on commit.
```

- [ ] **Step 6.2: Add the `LicenseSeatProvider` struct doc-comment in `licenseseat.rs`**

In `crates/spur-license/src/licenseseat.rs`, just BEFORE the `#[derive(Clone)]` on the struct (line 48), insert:

```rust
/// Production `LicenseProvider` backed by the `licenseseat` SDK.
///
/// # Concurrency
///
/// All mutating methods (`activate`, `validate`, `heartbeat`,
/// `deactivate`) are serialized via `operation_lock`, a fair
/// (FIFO) `tokio::sync::Mutex` held across the SDK round-trip
/// AND the subsequent `replace_state`. Two callers commit in the
/// order they acquire the mutex.
///
/// Readers (`current_state`, `current_snapshot`, `subscribe`,
/// `has_entitlement`) proceed without acquiring this lock and
/// observe a best-effort snapshot:
///
/// - `current_state()` reads `sdk.current_license()` BEFORE the
///   provider RwLock, so during an in-flight mutator it can
///   observe SDK-cache-post-mutation mixed with provider-state-
///   pre-mutation. Eventually consistent on commit.
/// - `has_entitlement(feature)` reads the SDK cache directly;
///   not synchronized with provider state.
/// - The autonomous SDK event bridge (`spawn_sdk_event_bridge`)
///   reads `state` independently and can forward stale snapshots
///   on autonomous events. Tracked: `bd-22q.14`.
///
/// **Future-implementer advisory**: any new state-mutating path
/// added to this provider (including bridge hydration in
/// `bd-22q.14`) MUST acquire `operation_lock` to preserve the
/// cross-method commit-order guarantee.
///
/// The internal `state: Arc<RwLock<LicenseState>>` continues to
/// protect against torn writes at the snapshot level. Note: the
/// current `replace_state` silently ignores `RwLock` poisoning
/// (`if let Ok(...)`), which is a pre-existing correctness bomb
/// tracked separately. See `bd-22q.16`.
```

- [ ] **Step 6.3: Update `LicenseProvider` trait advisory in `provider.rs`**

In `crates/spur-license/src/provider.rs`, after the existing `# Err-arm state mutation contract` paragraph (line 22–35), append:

```rust
/// # Cross-method serialization (advisory)
///
/// `LicenseSeatProvider` (the production implementation)
/// serializes its mutating methods (`activate`, `validate`,
/// `heartbeat`, `deactivate`) end-to-end via an internal
/// `tokio::sync::Mutex` to prevent durable over-permissioning
/// from concurrent SDK calls committing in the wrong order. This
/// trait does NOT mandate equivalent serialization — implementers
/// whose backends naturally serialize (e.g., a single in-memory
/// state guarded by a `RwLock` write that's held across the
/// equivalent of an SDK round-trip) need no extra mechanism.
/// However, any production `LicenseProvider` that performs an
/// asynchronous side-effecting call (network round-trip, IPC,
/// process spawn) AND mutates its own state on the result MUST
/// consider whether interleaving with another mutating method
/// could produce stale-allow over-permissioning, and serialize
/// accordingly. See
/// `docs/superpowers/specs/2026-04-29-bd-22q-15-licenseseat-cross-method-serialization-design.md`
/// for the LicenseSeatProvider design.
```

- [ ] **Step 6.4: Run the full check suite**

Run, in order:

```
cargo build -p spur-license --tests 2>&1 | tail -10
cargo test -p spur-license --features test-support 2>&1 | tail -30
cargo test -p spur-mcp 2>&1 | tail -20
cargo clippy -p spur-license --all-targets --features test-support -- -D warnings 2>&1 | tail -20
cargo fmt --all -- --check 2>&1 | tail -10
```

Expected: all clean.

- [ ] **Step 6.5: If `cargo fmt --all -- --check` fails, run `cargo fmt --all` and commit ONLY the formatting changes as a separate commit**

Precedent: bd-22q.1 Task 6. Workspace fmt sweep is acceptable per prior brain-side approval.

```
cargo fmt --all
git diff --stat   # confirm only formatting diffs
git add -A
git commit -m "style(workspace): cargo fmt sweep after bd-22q.15"
```

- [ ] **Step 6.6: Commit doc-comment updates**

```
git add crates/spur-license/src/lib.rs crates/spur-license/src/provider.rs crates/spur-license/src/licenseseat.rs
git commit -m "docs(spur-license): bd-22q.15 freshness/concurrency contract on SpurLicense + LicenseSeatProvider + trait"
```

---

## Task 7: File follow-up issues (`bd-22q.16`, `bd-22q.17`)

**Files:** None (beads issue creation only).

- [ ] **Step 7.1: File `bd-22q.16` — RwLock poison-swallowing remediation**

Use `mcp__spur-mcp__create_issue` with:

- **title**: `M1.x.3 — LicenseSeatProvider replace_state RwLock poison handling`
- **priority**: 3
- **labels**: `["concurrency", "deferred-from-bd-22q-15", "follow-up", "spur-license", "tier-revamp"]`
- **blocked_by**: `["bd-22q.15", "bd-22q"]`
- **body**: explain the pre-existing hazard at `crates/spur-license/src/licenseseat.rs:84–88` where `replace_state` uses `if let Ok(mut state) = self.state.write() { ... }` and silently drops writes on poison. Two viable remediations: (a) migrate `state` from `std::sync::RwLock` to `tokio::sync::RwLock` (no poison concept); (b) explicitly recover via `into_inner()` on poison and log. Reference this spec for context.

- [ ] **Step 7.2: File `bd-22q.17` — SDK trait abstraction for race-reproduction tests**

Use `mcp__spur-mcp__create_issue` with:

- **title**: `M1.x.4 — LicenseSeat SDK trait abstraction for race-reproduction tests`
- **priority**: 3
- **labels**: `["testing", "deferred-from-bd-22q-15", "follow-up", "spur-license", "tier-revamp"]`
- **blocked_by**: `["bd-22q.15", "bd-22q"]`
- **body**: explain that `bd-22q.15` ships lock-discipline canary tests but does NOT directly reproduce the validate-vs-deactivate race because that requires mocking slow SDK calls. The canaries prove lock acquisition; a direct race test would need `sdk: LicenseSeat` abstracted behind a trait (e.g., `pub trait LicenseSeatSdk: Send + Sync`) so a test fake can inject controllable delays. Out of scope for `bd-22q.15` (large refactor) but useful coverage. Reference this spec for context.

- [ ] **Step 7.3: Verify both issues are visible**

Use `mcp__spur-mcp__get_issue` on `bd-22q.16` and `bd-22q.17` to confirm creation succeeded.

---

## Acceptance criteria (final)

All criteria from the spec's "Acceptance criteria" section must hold. Specifically:

- [ ] All four mutating methods on `LicenseSeatProvider` acquire `operation_lock` at entry.
- [ ] `#[cfg(test)]` `pub(crate)` accessor exists, NOT `feature = "test-support"` gated.
- [ ] All 6 tests in `cross_method_race` mod pass: `mutex_serializes_concurrent_acquirers`, `activate/validate/heartbeat/deactivate_blocks_on_externally_held_operation_lock`, `fifo_ordering_via_virtual_time_cascade`.
- [ ] `SpurLicense` doc-comment updated (delete `bd-22q.15` bullet, add `# Concurrency` section).
- [ ] `LicenseSeatProvider` struct doc-comment added.
- [ ] `LicenseProvider` trait advisory extended with the cross-method-serialization paragraph.
- [ ] No regression in: `tests/community_smoke.rs`, `tests/fake_provider.rs`, `tests/invariants.rs`, `tests/licenseseat_probe.rs`, `tests/feature_gate_freshness.rs`, `tests/emission_audit.rs`, `crates/spur-mcp/tests/feature_gate_plumbing.rs`.
- [ ] `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings` clean.
- [ ] `cargo fmt --all -- --check` clean (or fmt sweep committed separately).
- [ ] `bd-22q.16` and `bd-22q.17` filed in beads.
- [ ] `bd-22q.15` itself can be closed in beads.

---

## Risk hot-spots for implementer review

1. **`let _guard = ...` vs `let _ = ...`.** A code review item. `let _ = lock.lock().await` drops the guard immediately and serializes nothing. The implementer MUST use a named binding.

2. **Real-network slowness in Task 3 tests.** If a test machine actually establishes a TCP connection to the licenseseat server (even with a fake API key), the SDK call may take seconds to fail, and the spawned task would still be pending at the 100ms virtual-time advance — a FALSE PASS. If you observe flakiness, change the assertion shape: instead of "task is not finished", capture an `Instant` before drop and assert that the task's completion is AFTER the drop. Or set environment variable `LICENSESEAT_API_KEY=test` and `LICENSESEAT_PRODUCT_SLUG=test` if the SDK has a no-network test mode. The spec acknowledges this gap explicitly.

3. **`tokio::test(start_paused = true)`** is the right macro for Tasks 1, 3, 5. Plain `#[tokio::test]` would real-clock-time the assertions — flaky.

4. **`LicenseSeatProvider: Clone`.** Cloning the provider clones `Arc<tokio::sync::Mutex<()>>`, sharing the SAME mutex across clones. Verify by ensuring `.clone()` correctness in Test 2–5 — they use `provider.clone()` and rely on the shared mutex. If a future refactor accidentally replaces `Arc<Mutex<()>>` with a plain `Mutex<()>` (not `Clone`), the field would not compile under `derive(Clone)` — compile-time guard.

5. **Bridge-task interaction.** `spawn_sdk_event_bridge` (line 95) reads `state` without `operation_lock`. This is correct for `bd-22q.15`'s scope. `bd-22q.14` will need to address this. The struct doc-comment flags this for future readers.

---

## References

- Spec: `docs/superpowers/specs/2026-04-29-bd-22q-15-licenseseat-cross-method-serialization-design.md` (commit `e0b93fef`)
- Parent (bd-22q.1) spec: `docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md`
- Parent (bd-22q.1) plan (style match): `docs/superpowers/plans/2026-04-29-bd-22q-1-spurlicense-gate-refresh.md`
- Codex first-principles design review: `spur://continuation/db064024-1b6b-4d0c-9079-ab92cc725ffd`
- Kimi adversarial review: `spur://continuation/c224e5de-dc17-4e35-995c-fff3c7fc487d`
- Gemini clarity review: `spur://continuation/8f29dcb3-67c7-4f94-a371-cadb953c815e`
- Tokio Mutex FIFO: `tokio-1.51.1/src/sync/mutex.rs:20`
- Beads issue: `bd-22q.15`
