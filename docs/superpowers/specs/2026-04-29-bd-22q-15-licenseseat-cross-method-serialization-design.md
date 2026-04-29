# bd-22q.15 — `LicenseSeatProvider` Cross-Method Operation Serialization

**Status:** Approved 2026-04-29 (codex/kimi reviews folded in;
gemini clarity review pending against committed copy).
**Beads:** `bd-22q.15` (P2, 6-8h estimate, parent epic `bd-22q`).
**Origin:** Filed from kimi adversarial review of `bd-22q.1` spec
(`spur://continuation/2aef1302-678a-42c5-bf79-c17430c7e648`). Closes
the over-permissioning race that `bd-22q.1`'s gate refresh made
observable across all caching consumers.
**Depends on:** `bd-22q.1` (shipped 2026-04-29 commit `f6a585a3`).
**Independent of:** `bd-22q.14` (bridge hydration). Both can land in
either order.

## Problem

`LicenseSeatProvider` does NOT serialize cross-method operations.
The `Arc<RwLock<LicenseState>>` at `licenseseat.rs:51` only protects
the brief `replace_state` write (lines 84–93); the SDK calls in
`activate`/`validate`/`heartbeat`/`deactivate` are awaited WITHOUT
any lock held (`licenseseat.rs:184–285`).

### The race

```
Thread A: license.validate().await         // slow SDK network
Thread B: license.deactivate().await       // completes first
  → sdk.deactivate() returns Ok
  → replace_state(Inactive)
  → bd-22q.1: feature_gate refreshed to Inactive
Thread A: sdk.validate() returns Ok(Pro)   // server hadn't seen deactivate yet
  → replace_state(Pro)                     // overwrites Inactive
  → bd-22q.1: feature_gate refreshed to Pro
```

**Result:** the gate shows Pro entitlements for a license the user
just deactivated. Worse: a subsequent successful `heartbeat` reads
`current_snapshot()` at `licenseseat.rs:259`, which is now Pro, and
**reinforces** the bad state rather than resyncing it. Window
persists until the next `validate()` (typically 1 hour).

### Asymmetric impact

- **Stale-deny** = user inconvenience (waits for next refresh).
- **Stale-allow** = entitlement leak (unauthorized features for up
  to one validate interval).

`bd-22q.15` closes the stale-allow leak.

### Pre-existing nature

The race exists today, but only manifests in provider-internal state
(callers querying `provider.current_state()` after the fact see Pro
when they should see Inactive). `bd-22q.1`'s gate refresh propagates
the wrong state to every caching consumer (Orchestrator, CLI, MCP),
making the hazard observable across the system. The fix belongs at
the provider layer, not at the facade.

## Verified facts (file:line)

- `crates/spur-license/src/licenseseat.rs:48–54` — `LicenseSeatProvider`
  struct. Currently holds `sdk`, `state: Arc<RwLock<LicenseState>>`,
  `events_tx`, `refresh_policy`. **No mutex serializes mutating methods.**
- `crates/spur-license/src/licenseseat.rs:84–93` — `replace_state`
  acquires the RwLock briefly, writes, releases, then broadcasts.
  This is the ONLY lock-protected window today.
- `crates/spur-license/src/licenseseat.rs:184–200` — `activate`:
  awaits `sdk.activate(key)` with no lock held; on Ok, builds new
  state, calls `replace_state`. Err path returns immediately without
  mutating provider state.
- `crates/spur-license/src/licenseseat.rs:202–254` — `validate`:
  awaits `sdk.validate()` with no lock held; on Ok, builds new state
  (active or invalid), calls `replace_state`. Err path returns
  immediately without mutating.
- `crates/spur-license/src/licenseseat.rs:256–275` — `heartbeat`:
  awaits `sdk.heartbeat()` with no lock held; on Ok, reads
  `current_snapshot()`, possibly clears Degraded, calls
  `replace_state`. On Err, calls `degrade_current` which mutates
  state to `Degraded` via `replace_state` then returns Err.
- `crates/spur-license/src/licenseseat.rs:277–285` — `deactivate`:
  awaits `sdk.deactivate()` with no lock held; on Ok, calls
  `replace_state(Inactive)`. Err path returns immediately without
  mutating.
- `crates/spur-license/src/licenseseat.rs:95–128` — `spawn_sdk_event_bridge`:
  reads `state` via `state.read()`, broadcasts events with the
  observed snapshot. **The bridge does NOT mutate `state`.** It is
  a pure event forwarder. Therefore the bridge does NOT race with
  mutating methods at the state-write level.
- `crates/spur-license/src/licenseseat.rs:130–135` — `current_snapshot`:
  read-only access to `state` lock.
- `crates/spur-license/src/licenseseat.rs:148–162` — `current_state`:
  reads `state` lock, optionally patches `Inactive | ConfigError →
  Active` when `sdk.current_license().is_some()`. Read-only with
  respect to provider state.
- `crates/spur-license/src/lib.rs:312–340` (post `bd-22q.1`) — facade
  methods on `SpurLicense` already serialize gate refresh after
  provider returns; they rely on the provider's commit being the
  authoritative final state. The fix here ensures the provider's
  commit IS the last write.

## Design

### Approach 1 — operation-scope `tokio::sync::Mutex`

Add an `operation_lock: Arc<tokio::sync::Mutex<()>>` field to
`LicenseSeatProvider`. Each mutating method acquires the lock at
**entry**, before the SDK call, and holds it across the entire
SDK round-trip and the subsequent `replace_state`.

```rust
// crates/spur-license/src/licenseseat.rs

#[derive(Clone)]
pub struct LicenseSeatProvider {
    sdk: LicenseSeat,
    state: Arc<RwLock<LicenseState>>,
    operation_lock: Arc<tokio::sync::Mutex<()>>,   // NEW
    events_tx: broadcast::Sender<LicenseEvent>,
    refresh_policy: RefreshPolicy,
}

impl LicenseSeatProvider {
    pub fn new(api_key: String, product_slug: String) -> Self {
        // ... existing config setup ...
        let provider = Self {
            sdk,
            state: Arc::new(RwLock::new(initial_state)),
            operation_lock: Arc::new(tokio::sync::Mutex::new(())),  // NEW
            events_tx,
            refresh_policy,
        };
        provider.spawn_sdk_event_bridge();
        provider
    }
}

#[async_trait]
impl LicenseProvider for LicenseSeatProvider {
    async fn activate(&self, key: &str) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        // ... existing body unchanged ...
    }

    async fn validate(&self) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        // ... existing body unchanged ...
    }

    async fn heartbeat(&self) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        // ... existing body unchanged ...
    }

    async fn deactivate(&self) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        // ... existing body unchanged ...
    }
}
```

### Why `tokio::sync::Mutex` (not `std::sync::Mutex`)

The lock guard is held across `await` points (the SDK call). A
`std::sync::MutexGuard` is `!Send` across await — would not compile.
`tokio::sync::Mutex` is designed for this case.

### Why the lock is fair (FIFO)

`tokio::sync::Mutex` documents fair (FIFO) acquisition semantics
(`tokio-1.51.1/src/sync/mutex.rs:20`). Workspace `Cargo.toml:29`
declares `tokio = { version = "1", ... }`; `Cargo.lock:5362`
pins the resolved version to `1.51.1`. This gives us the precise
guarantee the `bd-22q.15` issue calls for: "any two of
`{activate, validate, heartbeat, deactivate}` are guaranteed to
commit their `replace_state` calls in the same order they began
their SDK calls." Mutex acquisition order = SDK-call start order =
`replace_state` commit order, all transitively equal under FIFO.

Note: tokio's FIFO-ness is API-level documented behavior. If a
future tokio release relaxes it (unlikely; would be a breaking
SemVer bump for behavior contract), the `fifo_ordering_via_explicit_barriers`
test (see Test plan) is the regression detector.

### Why entry (not around just `replace_state`)

The race is in the SDK call itself, not in `replace_state`. Holding
the lock only around `replace_state` does not prevent the
"validate-Ok-overwrites-deactivate-Inactive" interleaving — both
final writes still hit `replace_state` serially, but the inputs
to those writes are derived from concurrent server responses that
have already raced. The lock MUST be held across the SDK call to
serialize the operations end-to-end.

### Read paths — unchanged, NOT serialized with mutators

Reads do NOT acquire `operation_lock`. Crucially, this means
read-path observers may see a **best-effort snapshot** that mixes
SDK-cache state (post-mutation) with provider RwLock state
(pre-mutation), because the SDK mutates its internal cache inside
`activate`/`validate`/`deactivate` BEFORE SPUR's `replace_state`
runs. The mutex closes the *cross-method commit-order* race; it
does NOT make reads atomic with respect to in-flight mutations.

- `current_state()` (lines 148–162): consults
  `sdk.current_license()` FIRST, then reads the `state` RwLock and
  optionally patches `Inactive | ConfigError → Active`. Mid-flight
  in `activate`: the SDK cache may already report a license while
  provider RwLock still holds `Inactive`; `current_state()` returns
  the patched `Active` value before `replace_state` commits.
  Mid-flight in `deactivate`: the SDK cache may have already
  cleared while provider RwLock still holds `Active`; the patch
  no longer triggers, but the un-patched read still returns the
  stale `Active` snapshot until `replace_state(Inactive)` commits.
  This is **eventually consistent** — the gap closes when the
  in-flight mutator releases the mutex and `replace_state` lands.
- `has_entitlement(feature)`: forwards to `sdk.has_entitlement(...)`,
  reading the SDK cache directly. Reflects SDK-cache state at the
  moment of the call; not synchronized with provider RwLock or
  `operation_lock`.
- `subscribe()`: returns a broadcast receiver; no lock interaction.
- `current_snapshot()` (private): read-only access to provider
  RwLock. Called from `heartbeat` Ok-arm (line 259) and
  `degrade_current` (line 138). Both call sites already hold
  `operation_lock`; no additional acquisition needed.
- `degrade_current` (private): called from `heartbeat` Err-arm
  under `operation_lock`. No additional acquisition.

The eventually-consistent semantics are acceptable for `bd-22q.15`'s
stated goal (closing the durable over-permissioning race in
provider state). The "Residual hazards" section below documents the
remaining gaps.

### Residual hazards (explicitly out of scope)

These are real but pre-existing issues that `bd-22q.15` does NOT
attempt to fix. Documented honestly so future readers understand
the lock's exact scope.

1. **`current_state()` SDK-cache vs. provider-state mixing** (lines
   148–162). Caused by the SDK mutating its cache before
   `replace_state` runs. Mitigation: callers that need a strictly
   coherent snapshot should subscribe to `LicenseEvent`s and react
   on commit. **Not filed separately** — this is part of the
   `bd-22q.14` bridge-hydration work. The cleanest end-state is
   that the bridge becomes the source of truth for autonomous SDK
   events and SPUR derives all state from one path.

2. **Bridge stale-snapshot forwarding** (lines 95–128). The bridge
   reads `state.read()` (line 117) without `operation_lock`, so an
   autonomous SDK event firing during a mid-flight mutator gets
   forwarded with the pre-mutation snapshot. Tracked: `bd-22q.14`.
   `bd-22q.15` adds a doc-comment on the struct warning future
   `bd-22q.14` work that any added bridge-side state mutation MUST
   participate in `operation_lock`.

3. **Hanging-validate UX SLA**. With the mutex in place, a slow
   `validate()` (e.g., 30s SDK timeout on flaky connectivity)
   blocks user-initiated `deactivate` for the full duration. This
   is acceptable for `bd-22q.15`: under realistic refresh policies
   (validate=1h, heartbeat=5m) contention is rare, and the alternative
   (timeout-wrap each SDK call) introduces premature abort behavior
   that complicates server-truth reconciliation. **Future hardening:**
   if user reports surface this UX issue, add `tokio::time::timeout`
   around the SDK call inside each mutating method and document the
   abort semantics in a follow-up issue.

4. **`std::sync::RwLock` poison-swallowing in `replace_state`**
   (lines 84–88). The current code uses `if let Ok(mut state) =
   self.state.write() { ... }`, silently dropping writes if the
   lock is poisoned (i.e., a previous holder panicked). After
   poisoning, `current_snapshot()` returns
   `"License state unavailable"` indefinitely while broadcasts
   continue with stale state. This is a **pre-existing correctness
   bomb** unrelated to the cross-method race. Two viable
   remediations: (a) migrate `state` to `tokio::sync::RwLock`
   (no poisoning concept), (b) explicitly recover via
   `into_inner()` on poison and log. **File as a separate
   follow-up issue (`bd-22q.16`)** when implementing `bd-22q.15`;
   do NOT bundle it into this scope.

5. **`current_state()` multi-call composition by callers**.
   No callsite today composes two `current_state()` calls and
   assumes coherence (audited: `auth.rs:54,94`,
   `license_runtime.rs:86–91` — single calls only). Future
   callers must be advised via the trait advisory doc-comment.

### Bridge interaction — no changes required

`spawn_sdk_event_bridge` (lines 95–128) only **reads** `state` and
broadcasts events. It does NOT mutate provider state. Therefore the
bridge cannot race with mutating methods at the state-write level
and does NOT need to acquire `operation_lock`.

The bridge MAY observe a pre-mutation snapshot while a mutating
method holds the lock and is mid-SDK-call, then broadcast an
autonomous event with that pre-mutation snapshot. This produces an
event ordering like: `bridge.LicenseLoaded(pre)` →
`explicit.Validated(post)`. Subscribers see two events; last write
wins for cached state. This is identical to today's behavior and
is independently tracked under `bd-22q.14` (bridge hydration).

### Construction-time hydration — no changes required

`new()` runs `spawn_sdk_event_bridge` AFTER seeding the initial
state. No mutating method can run before construction completes.
First mutating-method call acquires the lock cleanly.

### Cancellation safety

`tokio::sync::Mutex::lock().await` returns a `MutexGuard` whose
drop releases the lock. If the calling future is cancelled
(timeout, `JoinHandle::abort`), the guard is dropped and the lock
is released. The next queued waiter proceeds. This is correct
behavior; no special handling required.

**Cancellation-induced state divergence:** If a `validate` future
is cancelled mid-SDK-call, the SDK may have already committed
server-side, but the client's `replace_state` never runs. Client
state is stale until the next refresh. This is a **pre-existing**
hazard at the SDK level; `bd-22q.15` does not introduce it and does
not attempt to fix it. Documenting only.

## Alternatives considered

### Approach 2 — version stamps with retry/discard

Each mutating method captures a version on entry; `replace_state`
only commits if the version matches the current head. On mismatch,
the operation aborts (or retries the SDK call).

- Pro: allows true concurrent SDK calls.
- Con: significantly more machinery; harder to reason about. Policy
  question ("which operation wins?") becomes the design's hot
  surface (deactivate-wins? latest-wins?). Prone to subtle bugs.
- Con: SDK round-trips are not free even when discarded — wasted
  bandwidth and server-side rate-limit pressure on the discard path.
- Verdict: rejected. Approach 1 is simpler, correct, and the
  performance cost is negligible for our call frequency.

### Approach 3 — `tokio::sync::RwLock` write-across-await

Use a `RwLock`, acquire the write half across the SDK call. Reads
proceed concurrently when no mutator holds write.

- Pro: marginally more concurrency than Mutex (parallel reads).
- Con: `current_state()` already reads `state: Arc<RwLock<...>>`
  without contention; adding a second RwLock layer for the
  mutating-method gate gives no measurable benefit at our read
  frequency.
- Con: write across await blocks ALL readers for the SDK call
  duration — strictly worse than Approach 1, which leaves readers
  unaffected.
- Verdict: rejected.

### Approach 4 — actor/channel

Refactor mutating methods to enqueue into an `mpsc::channel`
processed by a single task that owns the SDK and state.

- Pro: serialization is structural, not lock-based.
- Con: large refactor; backpressure semantics; cancellation
  semantics; introduces an extra task per provider; complicates
  `Drop` shutdown.
- Verdict: rejected as out of proportion to the bug.

## Performance impact

Mutating methods now serialize globally. Worst case: a slow
`validate` (network round-trip, typically <1s, can spike to 10s
under degraded connectivity) blocks `deactivate` for the duration.

Frequencies:
- `validate`: scheduled at `RefreshPolicy::validate_interval`
  (default 3600s = 1 hour, `provider.rs:16`).
- `heartbeat`: scheduled at `RefreshPolicy::heartbeat_interval`
  (default 300s = 5 minutes, `provider.rs:17`).
- `activate`: user-initiated (rare, typically once per device).
- `deactivate`: user-initiated (rare, end-of-lifecycle).

Realistic contention probability: vanishingly small. A user-driven
`deactivate` overlapping with a scheduled `validate` is the only
realistic interleaving, and the worst-case wait is the validate's
SDK timeout (typically <10s).

**No `cargo bench` regression expected.** No bench targets exist
for this provider today (`grep -r 'criterion' crates/spur-license/`
returns nothing); we do not introduce one for this change.

## Concurrency notes

- **What's serialized:** the four mutating methods execute one at
  a time, in mutex-acquisition order (FIFO under tokio).
- **What's NOT serialized:** reads (`current_state`, `subscribe`,
  `has_entitlement`, `current_snapshot`); the autonomous bridge
  loop (`spawn_sdk_event_bridge`).
- **Eventual-consistency hedge dropped:** the `SpurLicense`
  doc-comment in `lib.rs:217–237` currently states "Concurrent
  mutations of this facade are serialized only at the
  `replace_state` granularity inside the provider, not across the
  full SDK round-trip. A `validate`/`deactivate` race can produce a
  transient over-permissioning window. Tracked: `bd-22q.15`." After
  `bd-22q.15` ships, this paragraph is replaced with: "Concurrent
  mutations are serialized end-to-end at the provider layer
  (`LicenseSeatProvider::operation_lock`); the gate refresh in this
  facade rides on top of that serialization, so the cached
  `Arc<FeatureGate>` reflects the LATER-COMMITTED operation's
  state."
- **`current_state()` Inactive→Active patch (lines 148–161)**: this
  patching logic at `LicenseSeatProvider::current_state()` runs
  outside `operation_lock`. It is unaffected by serialization. The
  patch's correctness was audited under `bd-22q.1` and remains
  correct: callers using `current_state()` see the in-RwLock value
  with the patch applied; mutating methods commit to the RwLock
  via `replace_state`, and the patch logic still applies on the
  next `current_state()` read regardless of whether a mutating
  method is in flight.

## Doc-comment updates

### `SpurLicense` (lib.rs:212–245)

Replace the `bd-22q.15` paragraph in `# Out-of-scope (tracked
separately)` with a `# Concurrency` section:

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

### `LicenseSeatProvider` (licenseseat.rs:48)

Add a struct-level doc comment documenting the serialization
guarantee:

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

## Test plan

### New unit tests in `crates/spur-license/src/licenseseat.rs` (in-crate test module)

**These tests are explicitly LOCK-DISCIPLINE CANARIES, not
direct race-reproduction.** The acceptance criteria's "concurrent
`validate` (mocked slow SDK) and `deactivate`" test would require
abstracting `sdk: LicenseSeat` behind a trait — a refactor outside
this issue's scope. The tests below prove that (a) the lock exists
on each mutating method, (b) lock acquisition order matches commit
order under tokio FIFO, and (c) bypass of the lock via the
internal-only test accessor is detectable. The full SDK-mock test
is filed as a deferred follow-up (`bd-22q.17`) so future
implementers can add it when the SDK trait abstraction lands.

Tests live INSIDE `crates/spur-license/src/licenseseat.rs` under a
`#[cfg(test)] mod cross_method_race { ... }` block — NOT under
`tests/` — because:

- The lock accessor must be `pub(crate)`, never exposed via the
  `test-support` feature flag (kimi finding 5: any external crate
  with `test-support` could acquire the lock and stall production
  operations). In-crate test modules see private fields; integration
  tests would need `test-support`-gated `pub` exposure.
- All four mutating methods need targeted coverage, easier to
  parameterize from inside the module.

```rust
// crates/spur-license/src/licenseseat.rs (NEW: test-only accessor)
#[cfg(test)]
impl LicenseSeatProvider {
    /// In-crate-test-only handle to the operation lock. Used by
    /// the `cross_method_race` test module to assert lock-acquisition
    /// discipline. NEVER expose this `pub`: external crates could
    /// acquire the lock and stall production mutations.
    pub(crate) fn operation_lock_handle(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.operation_lock)
    }
}
```

Tests:

1. **`mutex_serializes_concurrent_acquirers`** — primitive sanity.
   Two `Arc<tokio::sync::Mutex<()>>` clones; task A holds + sleeps
   100ms with `tokio::time::pause()` for determinism; task B
   acquires; assert B's acquisition happens-after A's release via
   instrumented `Instant`. Regression canary for tokio FIFO.

2. **`activate_blocks_on_externally_held_operation_lock`** — proves
   `activate` acquires the lock at entry. Construct `LicenseSeatProvider`
   with a non-routable API key (SDK calls fast-fail). Acquire the
   lock via `operation_lock_handle()`. Spawn `provider.activate("X")`.
   Use `tokio::time::timeout(100ms, ...)` to assert pending. Release
   the lock. Assert the task completes (with whatever SDK error;
   irrelevant — assertion is on the lock-blocking timing).

3. **`validate_blocks_on_externally_held_operation_lock`** — same
   shape for `validate`.

4. **`heartbeat_blocks_on_externally_held_operation_lock`** — same
   shape for `heartbeat`.

5. **`deactivate_blocks_on_externally_held_operation_lock`** — same
   shape for `deactivate`. Together with tests 2–4, proves all four
   mutating methods participate in the lock discipline. Future
   regression: a fifth method added without lock acquisition would
   not be caught here, but the doc-comment on the struct + the
   trait-level advisory cover the soft-enforcement story.

6. **`fifo_ordering_via_explicit_barriers`** — barrier-coordinated
   FIFO check (replaces the timing-only test that kimi flagged as
   coincidence-prone). Acquire the lock externally. Spawn three
   tasks in a known order: each first awaits a `tokio::sync::Barrier`
   (with N=4 including the test itself), then attempts to acquire
   the lock. Drop the barrier so all three tasks race to the lock
   simultaneously after the holder releases. Record acquisition
   order via shared `Mutex<Vec<usize>>`. Assert: under tokio's
   documented FIFO, recorded order matches spawn order. **Without
   the barrier**, the spawn-order assumption is invalid (spawn order
   ≠ acquisition-attempt order under the tokio scheduler).

### Existing tests — must not regress

- `crates/spur-license/tests/community_smoke.rs` — already-passing
  smoke against the facade.
- `crates/spur-license/tests/fake_provider.rs` — `FakeProvider`
  unchanged; tests unchanged.
- `crates/spur-license/tests/invariants.rs` — license-event invariants.
- `crates/spur-license/tests/licenseseat_probe.rs` — provider probe
  test. Verify it still passes; the lock is acquired-and-released
  on each method call, no externally-observable change.
- `crates/spur-license/tests/feature_gate_freshness.rs` (bd-22q.1) —
  6 freshness tests. Lock is invisible to facade-level testing
  through `FakeProvider`; these tests remain green.
- `crates/spur-license/tests/emission_audit.rs` —
  `is_handler_originated` dedup table.
- `crates/spur-mcp/tests/feature_gate_plumbing.rs` — cross-crate
  consumer test.

**Note**: there is NO `tests/cross_method_race.rs` integration file.
The lock-discipline canaries live INSIDE `src/licenseseat.rs` per
the rationale above.

### Test-feature-flag dependence

The `operation_lock_handle` accessor is gated behind `#[cfg(test)]`
ONLY (NOT `feature = "test-support"`) and uses `pub(crate)`
visibility. This deliberately blocks any external-crate acquisition
of the lock: only this crate's own `#[cfg(test)] mod cross_method_race`
can call it. `test-support`-gated FakeProvider work is unaffected.

## Implementation tasks (TDD)

The implementation plan (writing-plans output) will decompose this
into six tasks, in order:

1. **Task 1 — failing primitive sanity test.** Add
   `#[cfg(test)] mod cross_method_race { ... }` inside
   `src/licenseseat.rs` containing only
   `mutex_serializes_concurrent_acquirers`. Test references
   `LicenseSeatProvider::operation_lock_handle()` which doesn't
   exist yet — **compile fails** (RED). The test BODY will pass
   once compilation succeeds (it's a primitive sanity test on
   tokio Mutex itself). Commit RED.

2. **Task 2 — add `operation_lock` field + `pub(crate)` accessor.**
   Add `operation_lock: Arc<tokio::sync::Mutex<()>>` field; add
   `pub(crate) fn operation_lock_handle(&self) -> Arc<...>` gated
   on `#[cfg(test)]`. Compile passes; Task 1 test passes. Mutating
   methods do NOT yet acquire the lock. Commit GREEN.

3. **Task 3 — failing per-method blocking tests.** Add four tests
   (`activate_blocks_on_externally_held_operation_lock`,
   `validate_blocks_on_externally_held_operation_lock`,
   `heartbeat_blocks_on_externally_held_operation_lock`,
   `deactivate_blocks_on_externally_held_operation_lock`).
   They fail because mutating methods do not yet acquire the lock —
   the spawned `provider.X().await` calls return BEFORE the
   external lock is released. Commit RED.

4. **Task 4 — acquire lock at mutating-method entry.** Add
   `let _guard = self.operation_lock.lock().await;` at the top
   of each of the four mutating methods. Tests pass. Commit GREEN.

5. **Task 5 — failing FIFO test + GREEN with already-acquired lock.**
   Add `fifo_ordering_via_explicit_barriers` using
   `tokio::sync::Barrier`. The test exercises ordering across
   all four mutating methods; with Task 4 already in place, this
   test will likely pass on first run (tokio FIFO is correct).
   The RED-GREEN cycle here is more "verify the discipline, lock
   in the canary" than "drive an implementation change". Commit
   GREEN. **Document this as a regression canary** in a comment
   atop the test.

6. **Task 6 — doc-comment updates + full-suite check.** Update
   doc-comments per the "Doc-comment updates" section
   (`SpurLicense`, `LicenseSeatProvider` struct,
   `LicenseProvider` trait advisory). Run
   `cargo test -p spur-license --features test-support`,
   `cargo test -p spur-mcp`,
   `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings`,
   `cargo fmt --all -- --check`. All clean. Commit a fmt-only
   sweep if needed (precedent: `bd-22q.1` Task 6).

7. **Task 7 — file `bd-22q.16` (poison-swallowing) and `bd-22q.17`
   (SDK trait abstraction for race-reproduction tests).** Two new
   beads issues capturing the "Residual hazards" gaps. Body for
   each links back to this spec and quotes the exact line ranges.
   This task closes `bd-22q.15` cleanly without scope creep.

## Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| `tokio::sync::Mutex` is not actually FIFO and our ordering claim is wrong | Very Low | `fifo_ordering_via_explicit_barriers` test (with `tokio::sync::Barrier` coordination) serves as a regression canary if tokio's discipline changes upstream. Cargo.lock pins tokio 1.51.1 (`tokio-1.51.1/src/sync/mutex.rs:20` documents fair acquisition). |
| Lock contention causes user-visible latency (UI freeze) | Very Low | Mutex is held only across SDK round-trip. Realistic contention requires a user-initiated mutation overlapping a scheduled refresh — both rare, both already async. UI never blocks on these calls today. |
| Hanging-validate blocks user-initiated deactivate for SDK timeout duration (~30s on flaky connection) | Low | Documented in "Residual hazards" #3. UX SLA accepted for `bd-22q.15` scope. Future hardening: wrap each SDK call in `tokio::time::timeout` if user reports surface this. |
| Mutex held across panic in SDK call leaves the lock un-released | Very Low | `tokio::sync::Mutex` is panic-safe; the guard's `Drop` releases the lock even if the task panics. (Unlike `std::sync::Mutex`, there is no poisoning concept.) |
| Future refactor adds a fifth mutating method without acquiring the lock | Medium | (a) Doc-comment on the struct documents the discipline. (b) `LicenseProvider` trait advisory (extended from `bd-22q.1`) mentions the operation_lock discipline. (c) Per-method blocking tests (4 of them) catch the regression for any of the existing four; a fifth method would slip through. Soft-enforcement only. |
| Cancellation mid-SDK-call leaves client/server diverged | Low | Documented in "Cancellation safety" section. Pre-existing; not introduced by this change. Out of scope. |
| Bridge starts mutating state in a future refactor (bd-22q.14) without acquiring the lock | Medium | `bd-22q.14` will explicitly address bridge hydration; that work MUST acquire `operation_lock` if it adds state mutation. Doc-comment on the struct flags this. Track in `bd-22q.14`'s spec when drafted. |
| `operation_lock_handle` accessor leaks via `feature = "test-support"` and external crate stalls production mutations | Mitigated | Accessor gated `#[cfg(test)]` ONLY (NOT `test-support`) and uses `pub(crate)` visibility. Tests live in-crate. External crates have NO mechanism to acquire the lock. |
| `std::sync::RwLock` poison-swallowing in `replace_state` causes silent state-write loss | Low (pre-existing) | Out of scope for `bd-22q.15`. Tracked: file new `bd-22q.16` during Task 7. Two viable remediations: migrate to `tokio::sync::RwLock` or explicit `into_inner()` on poison. |
| `current_state()` SDK-cache + provider-state mixing leaks Active reads during in-flight activate | Low (pre-existing) | Out of scope for `bd-22q.15`. Tracked: subsumed under `bd-22q.14` (bridge hydration). Documented in "Residual hazards" #1. |

## Acceptance criteria

- [ ] `LicenseSeatProvider` gains an `operation_lock:
  Arc<tokio::sync::Mutex<()>>` field, initialized in `new()`.
- [ ] All four mutating methods (`activate`, `validate`,
  `heartbeat`, `deactivate`) acquire `operation_lock` at entry,
  hold it across SDK call + `replace_state`, and release on
  return/panic.
- [ ] `#[cfg(test)]`-gated, `pub(crate)`-visibility accessor
  `operation_lock_handle()` returns `Arc<tokio::sync::Mutex<()>>`.
  **NOT** gated on `feature = "test-support"`.
- [ ] `#[cfg(test)] mod cross_method_race` inside
  `crates/spur-license/src/licenseseat.rs` contains six tests:
  `mutex_serializes_concurrent_acquirers`,
  `activate_blocks_on_externally_held_operation_lock`,
  `validate_blocks_on_externally_held_operation_lock`,
  `heartbeat_blocks_on_externally_held_operation_lock`,
  `deactivate_blocks_on_externally_held_operation_lock`,
  `fifo_ordering_via_explicit_barriers`. All pass.
- [ ] Doc-comment on `SpurLicense` (`lib.rs:212–245`) is updated:
  the `bd-22q.15` paragraph in `# Out-of-scope` is replaced with
  the new `# Concurrency` section.
- [ ] Doc-comment on `LicenseSeatProvider` struct documents the
  serialization guarantee AND the residual hazards (read-path
  best-effort semantics, bridge bd-22q.14 advisory).
- [ ] `LicenseProvider` trait advisory updated to mention the
  operation_lock discipline (light touch — the trait does not
  mandate the lock; only documents that `LicenseSeatProvider` uses
  one and that future implementers should consider analogous
  serialization).
- [ ] No regression in: `tests/community_smoke.rs`,
  `tests/fake_provider.rs`, `tests/invariants.rs`,
  `tests/licenseseat_probe.rs`,
  `tests/feature_gate_freshness.rs`,
  `tests/emission_audit.rs`,
  `crates/spur-mcp/tests/feature_gate_plumbing.rs`.
- [ ] `cargo clippy -p spur-license --all-targets --features test-support -- -D warnings` clean.
- [ ] `cargo fmt --all -- --check` clean.
- [ ] Two new beads issues filed: `bd-22q.16` (poison-swallowing
  remediation), `bd-22q.17` (SDK trait abstraction for full
  race-reproduction coverage). Both link to this spec.
- [ ] Beads issue `bd-22q.15` closed; cross-references to
  `bd-22q.1` (verified-shipped), `bd-22q.14` (still open),
  `bd-22q.16` (newly filed), `bd-22q.17` (newly filed)
  remain accurate.

## Out of scope (filed elsewhere)

- Bridge hydration + autonomous-event subscription pump:
  `bd-22q.14`. The bridge currently attaches stale snapshots to
  forwarded autonomous events; `bd-22q.14` will fix this. The lock
  added here does NOT need to be acquired by the bridge in its
  current read-only form, but `bd-22q.14`'s implementer MUST
  acquire `operation_lock` if they add state mutation to the
  bridge. The `current_state()` SDK-cache mixing is also subsumed
  here.
- `validate()` Err-on-revocation leak: covered under `bd-22q.14`.
- TUI-side observability of the new ordering guarantee: not
  required; the freshness contract on `SpurLicense` is sufficient
  for caching consumers.
- SDK trait abstraction (would enable directly mocking slow SDK
  calls in tests): filed as **`bd-22q.17`** during Task 7. The
  `operation_lock_handle` accessor approach gives lock-discipline
  coverage without the refactor; the SDK-mock test would give
  direct race-reproduction coverage.
- `std::sync::RwLock` poison-swallowing in `replace_state` (lines
  84–88): filed as **`bd-22q.16`** during Task 7. Two viable fixes
  documented in "Residual hazards" #4.

## References

- Origin: kimi adversarial review of `bd-22q.1`,
  `spur://continuation/2aef1302-678a-42c5-bf79-c17430c7e648`
- Parent design: `docs/superpowers/specs/2026-04-29-bd-22q-1-spurlicense-gate-refresh-design.md`
- Provider source: `crates/spur-license/src/licenseseat.rs:48–289`
- Facade source (post `bd-22q.1`): `crates/spur-license/src/lib.rs:212–340`
- `LicenseProvider` trait: `crates/spur-license/src/provider.rs`
- `tokio::sync::Mutex` fairness:
  `tokio-1.51.1/src/sync/mutex.rs:20` documents fair (FIFO)
  acquisition. Workspace pin: `Cargo.toml:29` (`version = "1"`),
  resolved in `Cargo.lock:5362` (`1.51.1`).
- `bd-22q.1` ship commit: `f6a585a3`
- Plan D dependency chain: `bd-22q.1 → bd-22q.14 → bd-22q.11 → bd-22q.12`;
  `bd-22q.15` parallels this chain (independent of `bd-22q.14`).
- Codex first-principles design review: `spur://continuation/db064024-1b6b-4d0c-9079-ab92cc725ffd`
- Kimi adversarial review (covers same kimi that surfaced the
  original race): `spur://continuation/c224e5de-dc17-4e35-995c-fff3c7fc487d`
