# SPUR Licensing Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** harden the licensing subsystem landed in commit `caeeccc` by verifying suspected defects empirically, adding a FakeProvider test seam, fixing confirmed defects with regression tests, and closing the Task 6 rollout gap from the original plan.

**Architecture:** four sequential phases with a decision gate after Phase 0. Phase 0 reads the `licenseseat 0.5.3` crate and runs a one-shot tracing test to confirm or retract the two suspected 🔴 defects (duplicate emission, heartbeat over-trigger). Phase 1 lands `SpurLicense::from_provider` + `FakeProvider` + proptest invariants. Phase 2 applies confirmed fixes and the unconditional G2 cold-start plan hydration, with regression tests. Phase 3 lands low-risk polish items and closes the original plan's Task 6 verification.

**Tech Stack:** Rust 2021, `tokio`, `licenseseat = 0.5.3`, `async-trait`, `proptest`, `tracing` / `tracing-subscriber`, existing workspace dev-deps.

**Source spec:** [`docs/superpowers/specs/2026-04-19-licensing-hardening-design.md`](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-19-licensing-hardening-design.md:1)

**Source plan (prior):** [`docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md`](/Volumes/Projects/spur/docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md:1)

---

## Phase 0 Decision Gates

Phase 0 produces `docs/rca/2026-04-19-licenseseat-emission-audit.md`. That document must answer three questions; Phase 2 scope depends on the answers.

| Question | If answer is… | Then in Phase 2… |
|---|---|---|
| Does `LicenseSeat::{activate, validate, heartbeat, deactivate}` emit through `subscribe()` during the explicit call? | **Yes** | Execute Task 7 (C9 dedup). |
| Same as above | **No** | Skip Task 7; add a rollout note explaining the bridge-only autonomous emission model. |
| Per `licenseseat 0.5.3`, which `BindingMode` values require heartbeat? | **Subset of modes** | Execute Task 8 (heartbeat gating). |
| Same as above | **All active modes** | Skip Task 8; keep current gate; add rollout note. |
| Does `LicenseSeat::current_license()` expose `plan_key` and entitlements without a network call? | **Yes** | Execute Task 6 G2 hydration exactly as written. |
| Same as above | **No** | Execute Task 6 as a no-op marker and add a rollout note that cold-start plan requires a successful validate. |

---

## File Map

| File | Responsibility | Task(s) |
|---|---|---|
| `docs/rca/2026-04-19-licenseseat-emission-audit.md` (new) | Phase 0 findings | Task 1 |
| `crates/spur-license/tests/emission_audit.rs` (new) | tracing test counting emissions per handler | Task 2 |
| `crates/spur-license/src/lib.rs` | add `SpurLicense::from_provider`; G2 hydrate | Tasks 3, 6 |
| `crates/spur-license/src/test_support.rs` (new) | `FakeProvider` | Task 4 |
| `crates/spur-license/Cargo.toml` | dev-dep + optional `test-support` feature | Tasks 4, 5 |
| `crates/spur-license/tests/invariants.rs` (new) | proptest: no Active→Invalid on network errors | Task 5 |
| `crates/spur-license/src/licenseseat.rs` | C9 dedup (gated), hydrate cached plan | Tasks 6, 7 |
| `crates/spur-license/src/provider.rs` | add `requires_heartbeat()` (gated) | Task 8 |
| `crates/spur-core/src/license_runtime.rs` | gate on `requires_heartbeat`; D5; D3 | Tasks 8, 12, 13 |
| `crates/spur-core/tests/license_runtime_fake_provider.rs` (new) | runtime state-transition tests | Task 9 |
| `crates/spur-tui/tests/license_status_render.rs` | add Active-cached + Active→Invalid cases | Task 10 |
| `crates/spur-cli/tests/auth_cli.rs` | add configured-happy-path tests | Task 11 |
| `crates/spur-cli/src/commands/auth.rs` | `--format json` | Task 14 |
| `docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md` | mark Task 6 checkboxes | Task 15 |
| `docs/superpowers/plans/2026-04-19-licensing-hardening-notes.md` (new) | rollout notes | Task 15 |

---

## Phase 0 — Fact-Finding Spike

### Task 1: Read `licenseseat 0.5.3` and write the emission audit RCA

**Intent:** settle the three decision-gate questions with citations to real upstream source.

**Files:**
- Create: `docs/rca/2026-04-19-licenseseat-emission-audit.md`

- [ ] **Step 1: Locate the upstream source**

Run:

```bash
cargo doc -p licenseseat --no-deps --open 2>&1 | head -20
```

If docs are sparse, fall back to the crate source:

```bash
find ~/.cargo/registry/src -type d -name 'licenseseat-0.5.3' | head -1
```

Note the path; every citation in the RCA must reference a file under that directory.

- [ ] **Step 2: Answer Gate Question 1 — SDK emission on explicit calls**

Grep the upstream source for `subscribe` and the four handler method names:

```bash
SRC=$(find ~/.cargo/registry/src -type d -name 'licenseseat-0.5.3' | head -1)
rg -n 'fn (activate|validate|heartbeat|deactivate)' "$SRC"
rg -n 'subscribe|Sender|emit|broadcast' "$SRC"
```

Trace whether each handler calls through to the event channel synchronously. Record one of:
- **CONFIRMED — SDK emits on explicit call** (cite file:line for the emission site inside each handler).
- **RETRACTED — SDK only emits on autonomous timers** (cite file:line for the timer loop and the absence of emission in explicit handlers).

- [ ] **Step 3: Answer Gate Question 2 — heartbeat policy per binding mode**

Grep for binding-mode types and heartbeat policy:

```bash
rg -n 'NodeLocked|FloatingCi|Organization|binding' "$SRC"
rg -n 'heartbeat|keep_alive|seat_lease' "$SRC"
```

Record which modes mandate a heartbeat. Quote the authoritative upstream doc/comment.

- [ ] **Step 4: Answer Gate Question 3 — cached `current_license()` payload**

```bash
rg -n 'fn current_license|plan_key|active_entitlements' "$SRC"
```

Determine whether `current_license()` returns a structure that already contains `plan_key` and `active_entitlements` (or equivalent) before any network call.

- [ ] **Step 5: Write the RCA document**

Create `docs/rca/2026-04-19-licenseseat-emission-audit.md` with this structure:

```markdown
# LicenseSeat 0.5.3 Emission & Policy Audit

**Pinned crate source:** `<absolute-path-to-licenseseat-0.5.3>`

## Gate 1 — Does the SDK emit on explicit handler calls?

**Answer:** CONFIRMED | RETRACTED

Evidence:
- <file:line>: <quoted code snippet>
- …

Implication for Phase 2 Task 7 (C9 dedup): <EXECUTE | SKIP with rollout note>

## Gate 2 — Which binding modes require heartbeat?

**Answer:** NodeLocked=<yes|no>, FloatingCi=<yes|no>, Organization=<yes|no>

Evidence:
- <file:line>: <quoted>

Implication for Phase 2 Task 8 (D1 gating): <EXECUTE | SKIP with rollout note>

## Gate 3 — Is cached plan_key + entitlements available pre-network?

**Answer:** YES | NO

Evidence:
- <file:line>: <quoted>

Implication for Phase 2 Task 6 (G2 hydration): <EXECUTE as-written | EXECUTE as marker-only>

## Summary decision vector

- Task 6: EXECUTE | MARKER-ONLY
- Task 7: EXECUTE | SKIP
- Task 8: EXECUTE | SKIP
```

- [ ] **Step 6: Commit**

```bash
git add docs/rca/2026-04-19-licenseseat-emission-audit.md
git commit -m "docs(licensing): licenseseat 0.5.3 emission & policy audit"
```

### Task 2: Tracing-based emission-count test

**Intent:** back the RCA with a mechanical check that can run in CI (ignored by default) and serve as the regression oracle for any C9 fix.

**Files:**
- Create: `crates/spur-license/tests/emission_audit.rs`
- Modify: `crates/spur-license/Cargo.toml`

- [ ] **Step 1: Add dev-deps for tracing capture**

Edit `crates/spur-license/Cargo.toml` to add (append under existing `[dependencies]`; if no `[dev-dependencies]` section exists, add it):

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["full", "test-util"] }
tracing = { workspace = true }
tracing-subscriber = { workspace = true, features = ["fmt", "env-filter"] }
```

If `tracing`/`tracing-subscriber` are not in `[workspace.dependencies]`, add them to the root `Cargo.toml`'s `[workspace.dependencies]` with `tracing = "0.1"` and `tracing-subscriber = "0.3"`.

- [ ] **Step 2: Write the ignored emission-count test**

Create `crates/spur-license/tests/emission_audit.rs`:

```rust
//! Emission-count audit test. Runs only when live LicenseSeat credentials
//! are present. Records the number of `LicenseEvent`s observed on the
//! facade's subscribe() channel for a single explicit handler cycle.
//!
//! Expected counts after Phase 2:
//! - activate: 1
//! - validate (ok): 1
//! - heartbeat (ok): 1
//! - deactivate: 1
//!
//! Any count > 1 for a successful explicit handler call indicates the C9
//! duplicate-emission defect.

use std::time::Duration;

use spur_license::SpurLicense;

#[tokio::test]
#[ignore = "requires live LicenseSeat credentials and a test key"]
async fn explicit_handlers_emit_exactly_once() {
    let license = SpurLicense::from_env().expect("env configured");
    let mut rx = license.subscribe();

    // Drain any initial snapshot.
    let _ = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;

    let test_key = std::env::var("SPUR_LICENSESEAT_TEST_KEY")
        .expect("set SPUR_LICENSESEAT_TEST_KEY to a throwaway key");

    let count = count_emissions_during(&mut rx, || async {
        license.activate(&test_key).await.expect("activate");
    })
    .await;
    assert_eq!(count, 1, "activate emitted {count} events, expected 1");

    let count = count_emissions_during(&mut rx, || async {
        license.validate().await.expect("validate");
    })
    .await;
    assert_eq!(count, 1, "validate emitted {count} events, expected 1");

    let count = count_emissions_during(&mut rx, || async {
        license.heartbeat().await.expect("heartbeat");
    })
    .await;
    assert_eq!(count, 1, "heartbeat emitted {count} events, expected 1");

    let count = count_emissions_during(&mut rx, || async {
        license.deactivate().await.expect("deactivate");
    })
    .await;
    assert_eq!(count, 1, "deactivate emitted {count} events, expected 1");
}

async fn count_emissions_during<F, Fut>(
    rx: &mut tokio::sync::broadcast::Receiver<spur_license::LicenseEvent>,
    op: F,
) -> usize
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    op().await;
    // Collect everything that arrives within 250ms after the handler returns.
    let mut count = 0usize;
    loop {
        match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
            Ok(Ok(_)) => count += 1,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    count
}
```

- [ ] **Step 3: Verify compilation**

Run:

```bash
cargo check -p spur-license --tests
```

Expected: OK. The `#[ignore]` attribute keeps the test out of the default suite.

- [ ] **Step 4: Record the initial observation (for the RCA)**

Only run this if you have live credentials. Otherwise, record in the RCA that the test is pending operator execution.

```bash
SPUR_LICENSESEAT_API_KEY=… SPUR_LICENSESEAT_PRODUCT_SLUG=… SPUR_LICENSESEAT_TEST_KEY=… \
  cargo test -p spur-license --test emission_audit -- --ignored --nocapture
```

Paste the assertion-failure output (or pass output) into the RCA's Gate-1 Evidence section.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/tests/emission_audit.rs crates/spur-license/Cargo.toml Cargo.toml
git commit -m "test(spur-license): add ignored emission-count audit test"
```

---

## Phase 1 — Test Infrastructure

### Task 3: Add `SpurLicense::from_provider` constructor

**Intent:** open a public seam so tests can inject a `FakeProvider` without an env dance.

**Files:**
- Modify: `crates/spur-license/src/lib.rs`

- [ ] **Step 1: Write a failing test for the constructor**

Add to `crates/spur-license/tests/licenseseat_probe.rs` (append at end):

```rust
#[test]
fn from_provider_returns_a_usable_facade() {
    use std::sync::Arc;
    use spur_license::provider::LicenseProvider;

    // A minimal inline provider so this test doesn't depend on FakeProvider
    // (which lands in Task 4).
    struct Noop;
    #[async_trait::async_trait]
    impl LicenseProvider for Noop {
        fn current_state(&self) -> spur_license::LicenseState {
            spur_license::LicenseState::inactive("noop")
        }
        fn subscribe(
            &self,
        ) -> tokio::sync::broadcast::Receiver<spur_license::LicenseEvent> {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            std::mem::forget(tx);
            rx
        }
        fn refresh_policy(&self) -> spur_license::provider::RefreshPolicy {
            spur_license::provider::RefreshPolicy::default()
        }
        fn has_entitlement(&self, _: &str) -> bool {
            false
        }
        async fn activate(&self, _: &str) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
        async fn validate(&self) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
        async fn heartbeat(&self) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
        async fn deactivate(&self) -> spur_license::Result<spur_license::LicenseState> {
            Ok(spur_license::LicenseState::inactive("noop"))
        }
    }

    let license = SpurLicense::from_provider(Arc::new(Noop));
    assert!(matches!(
        license.current_state().status,
        spur_license::LicenseStatus::Inactive
    ));
}
```

Also add `async-trait` to `spur-license/Cargo.toml`'s `[dev-dependencies]` (it's already a regular dep at `:11`, but tests need it in scope too — reuse the crate dep; no change needed).

- [ ] **Step 2: Run the test to confirm it fails**

Run:

```bash
cargo test -p spur-license --test licenseseat_probe from_provider_returns_a_usable_facade
```

Expected: compile error `no function or associated item named 'from_provider' found for struct 'SpurLicense'`.

- [ ] **Step 3: Implement the constructor**

In `crates/spur-license/src/lib.rs`, add under the existing `impl SpurLicense` block (above `from_env` at `:183`):

```rust
impl SpurLicense {
    /// Construct a facade backed by an arbitrary provider. Primary use is
    /// test injection via `FakeProvider`; production paths should prefer
    /// `from_env` / `from_env_or_disabled`.
    pub fn from_provider(provider: std::sync::Arc<dyn crate::provider::LicenseProvider>) -> Self {
        Self { provider }
    }
}
```

(If you prefer to add inside the existing `impl` block rather than a new one, do that — either is fine. The method must be `pub`.)

- [ ] **Step 4: Run the test to confirm it passes**

Run:

```bash
cargo test -p spur-license --test licenseseat_probe from_provider_returns_a_usable_facade
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/lib.rs crates/spur-license/tests/licenseseat_probe.rs
git commit -m "feat(spur-license): pub SpurLicense::from_provider constructor"
```

### Task 4: Build `FakeProvider` scaffolding

**Intent:** a script-driven provider for tests across runtime, CLI, and TUI crates.

**Files:**
- Create: `crates/spur-license/src/test_support.rs`
- Modify: `crates/spur-license/src/lib.rs`
- Modify: `crates/spur-license/Cargo.toml`

- [ ] **Step 1: Add the `test-support` feature gate**

Edit `crates/spur-license/Cargo.toml`:

```toml
[features]
test-support = []
```

- [ ] **Step 2: Write a failing FakeProvider smoke test**

Create `crates/spur-license/tests/fake_provider.rs`:

```rust
#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::Duration;

use spur_license::test_support::FakeProvider;
use spur_license::{LicenseState, LicenseStatus, Plan, SpurLicense};

#[tokio::test]
async fn fake_provider_scripted_validate_transitions() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone());
    let mut rx = license.subscribe();

    // Script a validate that downgrades to Invalid.
    fake.push_validate_result(Ok({
        let mut s = LicenseState::active_validated(Plan::Pro, Default::default());
        s.status = LicenseStatus::Invalid;
        s.status_text = "revoked".into();
        s
    }));

    let out = license.validate().await.expect("validate ok-with-invalid");
    assert!(matches!(out.status, LicenseStatus::Invalid));

    let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .expect("event within 200ms")
        .expect("broadcast ok");
    assert!(matches!(ev.state.status, LicenseStatus::Invalid));
}

#[tokio::test]
async fn fake_provider_simulated_network_error_preserves_active_state() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone());

    fake.push_validate_result(Err(spur_license::LicenseError::Provider(
        "network unreachable".into(),
    )));

    let res = license.validate().await;
    assert!(res.is_err());
    // Cached state must remain Active.
    assert!(matches!(
        license.current_state().status,
        LicenseStatus::Active
    ));
}
```

- [ ] **Step 3: Run to confirm the test fails**

Run:

```bash
cargo test -p spur-license --features test-support --test fake_provider
```

Expected: compile error `no module named 'test_support'`.

- [ ] **Step 4: Implement `FakeProvider`**

Create `crates/spur-license/src/test_support.rs`:

```rust
//! Test-only `LicenseProvider` for exercising cross-crate licensing paths.
//! Enabled via the `test-support` feature.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::provider::{LicenseProvider, RefreshPolicy};
use crate::{LicenseError, LicenseEvent, LicenseEventKind, LicenseState, Result};

/// Scripted fake. Each `push_*_result` enqueues the outcome of the next
/// matching handler call. Unqueued calls reflect the current snapshot and
/// succeed.
pub struct FakeProvider {
    state: Mutex<LicenseState>,
    events_tx: broadcast::Sender<LicenseEvent>,
    script: Mutex<Script>,
    refresh_policy: RefreshPolicy,
    requires_heartbeat: bool,
}

#[derive(Default)]
struct Script {
    validate: VecDeque<Result<LicenseState>>,
    heartbeat: VecDeque<Result<LicenseState>>,
    activate: VecDeque<Result<LicenseState>>,
    deactivate: VecDeque<Result<LicenseState>>,
}

impl FakeProvider {
    pub fn new(initial: LicenseState) -> Self {
        let (events_tx, _) = broadcast::channel(64);
        Self {
            state: Mutex::new(initial),
            events_tx,
            script: Mutex::new(Script::default()),
            refresh_policy: RefreshPolicy::default(),
            requires_heartbeat: false,
        }
    }

    pub fn with_refresh_policy(mut self, policy: RefreshPolicy) -> Self {
        self.refresh_policy = policy;
        self
    }

    pub fn with_requires_heartbeat(mut self, needs: bool) -> Self {
        self.requires_heartbeat = needs;
        self
    }

    pub fn push_validate_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().validate.push_back(r);
    }

    pub fn push_heartbeat_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().heartbeat.push_back(r);
    }

    pub fn push_activate_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().activate.push_back(r);
    }

    pub fn push_deactivate_result(&self, r: Result<LicenseState>) {
        self.script.lock().unwrap().deactivate.push_back(r);
    }

    /// Inject a raw event into the subscribe channel without mutating state.
    /// Models autonomous SDK subscription updates.
    pub fn inject_event(&self, kind: LicenseEventKind, state: LicenseState) {
        let _ = self.events_tx.send(LicenseEvent {
            kind,
            state,
            message: None,
        });
    }

    fn snapshot(&self) -> LicenseState {
        self.state.lock().unwrap().clone()
    }

    fn commit(&self, next: LicenseState, kind: LicenseEventKind) -> LicenseState {
        *self.state.lock().unwrap() = next.clone();
        let _ = self.events_tx.send(LicenseEvent {
            kind,
            state: next.clone(),
            message: None,
        });
        next
    }
}

#[async_trait]
impl LicenseProvider for FakeProvider {
    fn current_state(&self) -> LicenseState {
        self.snapshot()
    }

    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        self.events_tx.subscribe()
    }

    fn refresh_policy(&self) -> RefreshPolicy {
        self.refresh_policy
    }

    fn has_entitlement(&self, feature: &str) -> bool {
        self.snapshot().features.contains(feature)
    }

    fn requires_heartbeat(&self) -> bool {
        self.requires_heartbeat
    }

    async fn activate(&self, _key: &str) -> Result<LicenseState> {
        let scripted = self.script.lock().unwrap().activate.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::Activated)),
            Some(Err(e)) => Err(e),
            None => Err(LicenseError::Provider("no scripted activate".into())),
        }
    }

    async fn validate(&self) -> Result<LicenseState> {
        let scripted = self.script.lock().unwrap().validate.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::Validated)),
            Some(Err(e)) => Err(e),
            None => Ok(self.snapshot()),
        }
    }

    async fn heartbeat(&self) -> Result<LicenseState> {
        let scripted = self.script.lock().unwrap().heartbeat.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::HeartbeatOk)),
            Some(Err(e)) => Err(e),
            None => Ok(self.snapshot()),
        }
    }

    async fn deactivate(&self) -> Result<LicenseState> {
        let scripted = self.script.lock().unwrap().deactivate.pop_front();
        match scripted {
            Some(Ok(next)) => Ok(self.commit(next, LicenseEventKind::Deactivated)),
            Some(Err(e)) => Err(e),
            None => Ok(self.commit(
                LicenseState::inactive("deactivated"),
                LicenseEventKind::Deactivated,
            )),
        }
    }
}
```

- [ ] **Step 5: Expose the module under the feature gate**

Edit `crates/spur-license/src/lib.rs`. After the existing `pub mod provider;` line (around `:2`) add:

```rust
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
```

Also add `requires_heartbeat()` to the `LicenseProvider` trait so `FakeProvider::requires_heartbeat` compiles. Edit `crates/spur-license/src/provider.rs`:

```rust
#[async_trait]
pub trait LicenseProvider: Send + Sync {
    fn current_state(&self) -> LicenseState;
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent>;
    fn refresh_policy(&self) -> RefreshPolicy;
    fn has_entitlement(&self, feature: &str) -> bool;

    /// Whether the runtime should periodically heartbeat for this subject.
    /// Default `false` — override in the adapter when the provider's lease
    /// model mandates it (see Phase 0 Gate 2 outcome).
    fn requires_heartbeat(&self) -> bool {
        false
    }

    async fn activate(&self, key: &str) -> Result<LicenseState>;
    async fn validate(&self) -> Result<LicenseState>;
    async fn heartbeat(&self) -> Result<LicenseState>;
    async fn deactivate(&self) -> Result<LicenseState>;
}
```

(The default is intentionally `false`. Task 8 overrides it for `LicenseSeatProvider` only if Phase 0 Gate 2 requires executing it.)

- [ ] **Step 6: Run the fake-provider tests to confirm they pass**

Run:

```bash
cargo test -p spur-license --features test-support --test fake_provider
```

Expected: both tests PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-license/src/test_support.rs \
        crates/spur-license/src/lib.rs \
        crates/spur-license/src/provider.rs \
        crates/spur-license/Cargo.toml \
        crates/spur-license/tests/fake_provider.rs
git commit -m "feat(spur-license): FakeProvider test seam + requires_heartbeat hook"
```

### Task 5: Proptest invariant — no Active→Invalid on network errors

**Intent:** encode as a property test the central trust invariant: transient network errors must never cause a license to be marked Invalid.

**Files:**
- Create: `crates/spur-license/tests/invariants.rs`
- Modify: `crates/spur-license/Cargo.toml`

- [ ] **Step 1: Add proptest dev-dep**

Edit `crates/spur-license/Cargo.toml`, under `[dev-dependencies]`:

```toml
proptest = "1"
```

- [ ] **Step 2: Write the proptest**

Create `crates/spur-license/tests/invariants.rs`:

```rust
#![cfg(feature = "test-support")]

use std::sync::Arc;

use proptest::prelude::*;
use spur_license::test_support::FakeProvider;
use spur_license::{LicenseError, LicenseState, LicenseStatus, SpurLicense};

#[derive(Debug, Clone, Copy)]
enum Step {
    ValidateNetworkErr,
    HeartbeatNetworkErr,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        Just(Step::ValidateNetworkErr),
        Just(Step::HeartbeatNetworkErr),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn network_errors_never_produce_invalid_from_active(
        script in prop::collection::vec(step_strategy(), 1..32),
    ) {
        // Sync-driver because proptest runs synchronous predicates.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async move {
            let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
            let license = SpurLicense::from_provider(fake.clone());

            for step in &script {
                match step {
                    Step::ValidateNetworkErr => {
                        fake.push_validate_result(Err(LicenseError::Provider(
                            "network".into(),
                        )));
                        let _ = license.validate().await;
                    }
                    Step::HeartbeatNetworkErr => {
                        fake.push_heartbeat_result(Err(LicenseError::Provider(
                            "network".into(),
                        )));
                        let _ = license.heartbeat().await;
                    }
                }
            }

            prop_assert!(
                !matches!(license.current_state().status, LicenseStatus::Invalid),
                "network-only failures must not mark license Invalid"
            );
            Ok(())
        }).unwrap();
    }
}
```

- [ ] **Step 3: Run the proptest**

Run:

```bash
cargo test -p spur-license --features test-support --test invariants
```

Expected: PASS (64 cases). The `FakeProvider` never mutates state on `Err` return — so the invariant holds by construction today. The test locks that behavior in.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/tests/invariants.rs crates/spur-license/Cargo.toml
git commit -m "test(spur-license): proptest — no Active→Invalid from network errors"
```

---

## Phase 2 — Confirmed Fixes + High-Confidence Gaps

### Task 6: G2 — Hydrate initial Plan and features from cached license

**Intent:** cold-start `Active` state should carry the real plan label, not `Plan::Unknown`.

**Executes as written** if Phase 0 Gate 3 answered YES. If NO, skip to Step 5 and add the rollout note only.

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs`
- Modify: `crates/spur-license/src/lib.rs`

- [ ] **Step 1: Write a failing test**

Append to `crates/spur-license/tests/licenseseat_probe.rs`:

```rust
#[test]
#[ignore = "requires a cached live activation"]
fn cached_active_startup_surfaces_real_plan() {
    // Precondition: an earlier run called activate() with a Pro key.
    let license = SpurLicense::from_env().expect("env configured");
    let state = license.current_state();
    assert!(matches!(
        state.status,
        spur_license::LicenseStatus::Active
    ));
    assert!(
        !matches!(state.plan, spur_license::Plan::Unknown),
        "expected cached plan to hydrate, got Unknown"
    );
}
```

- [ ] **Step 2: Implement the hydration**

In `crates/spur-license/src/licenseseat.rs`, replace the block at `:68-72` inside `LicenseSeatProvider::new`:

```rust
        let initial_state = match sdk.current_license() {
            Some(cached) => hydrate_from_cached(&cached),
            None => LicenseState::inactive("No active license"),
        };
```

Add the helper below the `LicenseSeatProvider` impl (around `:133` after `degrade_current`):

```rust
fn hydrate_from_cached(cached: &licenseseat::License) -> LicenseState {
    // Phase 0 Gate 3 confirmed `current_license()` returns plan_key + entitlements
    // before any network call. If those fields are absent on future SDK versions,
    // `Plan::from_key` already maps to `Plan::Unknown` and `features` falls back
    // to an empty set — safe fallback without a separate feature flag.
    let plan = Plan::from_key(&cached.plan_key);
    let features: BTreeSet<String> = cached
        .active_entitlements
        .iter()
        .map(|e| e.key.clone())
        .collect();
    LicenseState {
        status: LicenseStatus::Active,
        subject_kind: SubjectKind::User,
        plan,
        features,
        expires_at: cached.expires_at,
        binding_mode: BindingMode::NodeLocked,
        offline_ok: true,
        status_text: "Cached license available".into(),
    }
}
```

(If Phase 0 Gate 3 field names differ — e.g., `plan` instead of `plan_key`, or `entitlements` instead of `active_entitlements` — adjust to match; the structure stays the same.)

- [ ] **Step 3: Run the happy-path unit test**

Run:

```bash
cargo test -p spur-license
```

Expected: PASS. The ignored test does not run unless explicit `--ignored` is passed with live creds.

- [ ] **Step 4: Add a FakeProvider-based unit test that proves hydration preserves plan**

Append to `crates/spur-license/tests/fake_provider.rs`:

```rust
#[tokio::test]
async fn initial_active_state_carries_non_unknown_plan_when_seeded() {
    let mut seed = LicenseState::active_validated(Plan::Pro, Default::default());
    seed.status_text = "Cached Pro".into();
    let fake = Arc::new(FakeProvider::new(seed));
    let license = SpurLicense::from_provider(fake);
    assert!(matches!(license.current_state().plan, Plan::Pro));
}
```

Run:

```bash
cargo test -p spur-license --features test-support --test fake_provider
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/licenseseat.rs \
        crates/spur-license/tests/licenseseat_probe.rs \
        crates/spur-license/tests/fake_provider.rs
git commit -m "fix(spur-license): hydrate cached plan + entitlements on cold start (G2)"
```

**If Phase 0 Gate 3 returned NO:** skip Steps 2–4; only add a paragraph to the rollout notes (Task 15) explaining that cached plan requires a validate round-trip because the upstream crate does not expose it on `current_license()`.

### Task 7: C9 — Dedup duplicate emission from `LicenseSeatProvider`

**Intent:** every successful explicit handler call produces exactly one `LicenseEvent` on the facade's subscribe channel.

**Executes only if Phase 0 Gate 1 = CONFIRMED.** If RETRACTED, skip and add a rollout note.

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs`

- [ ] **Step 1: Write a failing FakeProvider-agnostic regression test**

This test cannot use `FakeProvider` because C9 is specific to `LicenseSeatProvider`. It uses the emission-audit test skeleton but with a stubbed `LicenseSeat` that simulates the SDK bridge firing. Actually, because we cannot stub the real SDK easily, express the invariant as a local test against `LicenseSeatProvider` with a no-network env. Append to `crates/spur-license/tests/emission_audit.rs`:

```rust
// Regression test for C9. The facade must emit exactly one LicenseEvent
// per explicit handler call. Uses the `DisabledProvider` path — which has
// no bridge and no SDK — as the upper bound of allowed emissions.
#[tokio::test]
async fn disabled_provider_emits_no_events_on_explicit_calls() {
    let license = spur_license::SpurLicense::from_env_or_disabled();
    let mut rx = license.subscribe();

    // DisabledProvider returns Ok(snapshot) but does not broadcast.
    let _ = license.validate().await;
    let _ = license.heartbeat().await;

    let got = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(
        got.is_err(),
        "DisabledProvider must not broadcast on explicit calls"
    );
}
```

Run:

```bash
cargo test -p spur-license --test emission_audit disabled_provider_emits_no_events_on_explicit_calls
```

Expected: PASS (this is a baseline — no fix needed for DisabledProvider). The live-cred test in Task 2 is the true C9 regression oracle.

- [ ] **Step 2: Apply the dedup**

Per Phase 0 Gate 1 CONFIRMED, the SDK bridge and `replace_state` both broadcast on every explicit handler call. Preferred direction: **drop the bridge's re-broadcast for event kinds that the explicit handler already broadcasts.**

Edit `crates/spur-license/src/licenseseat.rs` in `spawn_sdk_event_bridge` (around `:96-117`):

```rust
    fn spawn_sdk_event_bridge(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut rx = self.sdk.subscribe();
        let state = Arc::clone(&self.state);
        let tx = self.events_tx.clone();
        handle.spawn(async move {
            while let Ok(event) = rx.recv().await {
                let kind = map_event_kind(event.kind);
                // C9 dedup: event kinds that originate from explicit handlers
                // (activate/validate/heartbeat/deactivate) are re-broadcast by
                // `replace_state`. Skip them here to avoid double emission.
                // Kinds that only fire from autonomous SDK timers / server
                // pushes (e.g., LicenseRevoked) are still forwarded.
                if matches!(
                    kind,
                    LicenseEventKind::Activated
                        | LicenseEventKind::Validated
                        | LicenseEventKind::HeartbeatOk
                        | LicenseEventKind::Deactivated
                ) {
                    continue;
                }
                let snapshot = state
                    .read()
                    .map(|state| state.clone())
                    .unwrap_or_else(|_| LicenseState::inactive("License state unavailable"));
                let _ = tx.send(LicenseEvent {
                    kind,
                    state: snapshot,
                    message: None,
                });
            }
        });
    }
```

- [ ] **Step 3: Re-run the emission-audit test**

If live creds are available:

```bash
SPUR_LICENSESEAT_API_KEY=… SPUR_LICENSESEAT_PRODUCT_SLUG=… SPUR_LICENSESEAT_TEST_KEY=… \
  cargo test -p spur-license --test emission_audit -- --ignored --nocapture
```

Expected: all counts = 1.

If no live creds, mark the post-fix count as pending-operator-run in the rollout notes and rely on the DisabledProvider baseline + code review.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/licenseseat.rs crates/spur-license/tests/emission_audit.rs
git commit -m "fix(spur-license): dedup SDK bridge vs replace_state emissions (C9)"
```

**If Phase 0 Gate 1 = RETRACTED:** skip Steps 2–3. Add a paragraph to Task 15 rollout notes stating that C9 was a theoretical concern retracted by upstream audit; bridge and handlers do not overlap.

### Task 8: D1 — Per-binding-mode heartbeat gating

**Intent:** runtime heartbeat fires only when the provider's lease model requires it.

**Executes only if Phase 0 Gate 2 = EXECUTE.** If SKIP, drop Steps 2–3 and update rollout notes.

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs`
- Modify: `crates/spur-core/src/license_runtime.rs`
- Create/append: `crates/spur-core/tests/license_runtime_fake_provider.rs`

- [ ] **Step 1: Add call-counters to FakeProvider first**

Making the behavioral assertion watertight requires a side-channel counter. Edit `crates/spur-license/src/test_support.rs`:

At the top, add:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
```

Inside `FakeProvider` struct, add fields:

```rust
    heartbeat_calls: AtomicUsize,
    validate_calls: AtomicUsize,
```

Initialize both to `AtomicUsize::new(0)` in `new`.

At the start of `validate()` add `self.validate_calls.fetch_add(1, Ordering::Relaxed);`. Same at the start of `heartbeat()`.

Add accessors:

```rust
    pub fn heartbeat_call_count(&self) -> usize {
        self.heartbeat_calls.load(Ordering::Relaxed)
    }
    pub fn validate_call_count(&self) -> usize {
        self.validate_calls.load(Ordering::Relaxed)
    }
```

- [ ] **Step 2: Write a failing runtime test**

Create `crates/spur-core/tests/license_runtime_fake_provider.rs`:

```rust
#![cfg(feature = "test-support")]

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_core::event_funnel::spawn_funnel;
use spur_core::license_runtime::spawn_license_runtime;
use spur_license::test_support::FakeProvider;
use spur_license::{LicenseState, Plan, SpurLicense};
use tokio::sync::broadcast;

#[tokio::test]
async fn runtime_does_not_heartbeat_when_provider_declines() {
    let mut seed = LicenseState::active_validated(Plan::Pro, Default::default());
    seed.binding_mode = spur_license::BindingMode::NodeLocked;

    let fake = Arc::new(
        FakeProvider::new(seed)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_secs(3600),
                heartbeat_interval: Duration::from_millis(50),
            })
            .with_requires_heartbeat(false),
    );
    let probe = fake.clone();
    let license = SpurLicense::from_provider(fake);

    let (bcast_tx, _bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    // Give the runtime plenty of heartbeat windows (50ms interval × ~5 ticks).
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.abort();

    assert_eq!(
        probe.heartbeat_call_count(),
        0,
        "runtime must not heartbeat when requires_heartbeat=false"
    );
}
```

Add `spur-license = { workspace = true, features = ["test-support"] }` to `crates/spur-core/Cargo.toml` `[dev-dependencies]` so the feature is enabled for this test crate.

- [ ] **Step 3: Run the test and observe failure**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider \
    runtime_does_not_heartbeat_when_provider_declines
```

Expected: FAIL with `heartbeat_call_count = N > 0`, because current `should_heartbeat` gates on `binding_mode != Unknown`, which is true for our `NodeLocked` seed.

- [ ] **Step 4: Apply the fix**

In `crates/spur-license/src/licenseseat.rs`, override `requires_heartbeat` per Phase 0 Gate 2 findings. Example implementation (adjust mode predicates to match Gate 2 evidence):

```rust
#[async_trait]
impl LicenseProvider for LicenseSeatProvider {
    // ... existing methods ...

    fn requires_heartbeat(&self) -> bool {
        // Phase 0 Gate 2 evidence: only FloatingCi leases need periodic
        // heartbeats. NodeLocked and Organization do not.
        matches!(self.current_state().binding_mode, BindingMode::FloatingCi)
    }
}
```

In `crates/spur-core/src/license_runtime.rs`, replace `should_heartbeat` (at `:79-81`) with:

```rust
fn should_heartbeat(license: &SpurLicense) -> bool {
    license.current_state().is_active() && license.requires_heartbeat()
}
```

And expose `requires_heartbeat()` on `SpurLicense` by editing `crates/spur-license/src/lib.rs` inside the `impl SpurLicense`:

```rust
    pub fn requires_heartbeat(&self) -> bool {
        self.provider.requires_heartbeat()
    }
```

Update the `select!` guard at `license_runtime.rs:48` to call the new form:

```rust
                _ = &mut heartbeat_sleep, if should_heartbeat(&license) => {
```

- [ ] **Step 5: Run the test to confirm PASS**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-license/src/licenseseat.rs \
        crates/spur-license/src/lib.rs \
        crates/spur-core/src/license_runtime.rs \
        crates/spur-core/tests/license_runtime_fake_provider.rs \
        crates/spur-license/src/test_support.rs \
        crates/spur-core/Cargo.toml
git commit -m "fix(spur-core): gate heartbeat on provider.requires_heartbeat (D1)"
```

**If Phase 0 Gate 2 = SKIP:** drop Steps 3–5. Keep `requires_heartbeat` default `false` on the trait and do not override it on `LicenseSeatProvider`. Update rollout note: "provider declares heartbeat-universal — current runtime gate retained."

### Task 9: Runtime state-transition tests with FakeProvider

**Intent:** cover Active→Degraded→Active, Active→Invalid via revocation, and autonomous-subscription relay.

**Files:**
- Modify: `crates/spur-core/tests/license_runtime_fake_provider.rs`
- Modify: `crates/spur-core/Cargo.toml` (dev-dep if not already added in Task 8)

- [ ] **Step 1: Add Active→Degraded→Active test**

Append to `crates/spur-core/tests/license_runtime_fake_provider.rs`:

```rust
#[tokio::test]
async fn active_to_degraded_and_back_via_validate() {
    use spur_acp::LicenseStatusEvent;
    use spur_license::{LicenseError, LicenseState, Plan};

    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(
        FakeProvider::new(seed)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_millis(40),
                heartbeat_interval: Duration::from_secs(3600),
            }),
    );
    fake.push_validate_result(Err(LicenseError::Provider("network".into())));
    fake.push_validate_result(Ok(LicenseState::active_validated(Plan::Pro, Default::default())));

    let license = SpurLicense::from_provider(fake.clone());
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    let mut statuses = Vec::<LicenseStatusEvent>::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        if let Ok(ev) = tokio::time::timeout(Duration::from_millis(60), bcast_rx.recv()).await
        {
            if let Ok(ev) = ev {
                if let SpurEventBody::LicenseUpdated { state } = ev.body {
                    statuses.push(state.status);
                }
            }
        }
    }
    handle.abort();

    assert!(statuses.contains(&LicenseStatusEvent::Active), "got: {statuses:?}");
    assert!(statuses.contains(&LicenseStatusEvent::Degraded), "got: {statuses:?}");
    // Back to Active after the scripted Ok result.
    let last = statuses.last().copied();
    assert_eq!(last, Some(LicenseStatusEvent::Active), "trailing status should recover");
}
```

- [ ] **Step 2: Add Active→Invalid revocation test**

Append:

```rust
#[tokio::test]
async fn authoritative_invalid_propagates_to_funnel() {
    use spur_acp::LicenseStatusEvent;
    use spur_license::{LicenseState, LicenseStatus, Plan};

    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(
        FakeProvider::new(seed)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_millis(20),
                heartbeat_interval: Duration::from_secs(3600),
            }),
    );
    let mut invalid = LicenseState::inactive("revoked");
    invalid.status = LicenseStatus::Invalid;
    fake.push_validate_result(Ok(invalid));

    let license = SpurLicense::from_provider(fake);
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    let mut saw_invalid = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    while tokio::time::Instant::now() < deadline && !saw_invalid {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(40), bcast_rx.recv()).await
        {
            if let SpurEventBody::LicenseUpdated { state } = ev.body {
                if matches!(state.status, LicenseStatusEvent::Invalid) {
                    saw_invalid = true;
                }
            }
        }
    }
    handle.abort();
    assert!(saw_invalid, "runtime must propagate Invalid to the funnel");
}
```

- [ ] **Step 3: Add autonomous-subscription relay test**

Append:

```rust
#[tokio::test]
async fn injected_subscription_event_reaches_funnel() {
    use spur_acp::LicenseStatusEvent;
    use spur_license::{LicenseEventKind, LicenseState, LicenseStatus, Plan};

    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(FakeProvider::new(seed));
    let license = SpurLicense::from_provider(fake.clone());
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    // Drain the initial snapshot.
    let _ = tokio::time::timeout(Duration::from_millis(50), bcast_rx.recv()).await;

    let mut degraded = LicenseState::active_validated(Plan::Pro, Default::default());
    degraded.status = LicenseStatus::Degraded;
    degraded.status_text = "simulated SDK degrade".into();
    fake.inject_event(LicenseEventKind::ValidationFailed, degraded.clone());

    let ev = tokio::time::timeout(Duration::from_millis(200), bcast_rx.recv())
        .await
        .expect("event")
        .expect("recv");
    handle.abort();

    match ev.body {
        SpurEventBody::LicenseUpdated { state } => {
            assert!(matches!(state.status, LicenseStatusEvent::Degraded));
        }
        other => panic!("expected LicenseUpdated, got {other:?}"),
    }
}
```

- [ ] **Step 4: Run all runtime tests**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider
```

Expected: PASS on all three new tests (plus Task 8's test).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/tests/license_runtime_fake_provider.rs
git commit -m "test(spur-core): fake-provider coverage for runtime transitions"
```

### Task 10: TUI — Active-cached first render + Active→Invalid transition

**Intent:** close the gap in `license_status_render.rs` that leaves these paths untested.

**Files:**
- Modify: `crates/spur-tui/tests/license_status_render.rs`

- [ ] **Step 1: Add Active-cached first render test**

Append to `crates/spur-tui/tests/license_status_render.rs`:

```rust
#[test]
fn app_starts_with_active_plan_badge_when_seeded() {
    use spur_acp::{LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent, LicenseSubjectKind};
    use std::sync::Arc;

    let cfg = Arc::new(spur_acp::SpurConfig::default());
    let state = LicenseStateEvent {
        status: LicenseStatusEvent::Active,
        subject_kind: LicenseSubjectKind::User,
        plan: LicensePlan::Pro,
        features: Default::default(),
        expires_at: None,
        binding_mode: LicenseBindingMode::NodeLocked,
        offline_ok: true,
        status_text: "Cached license available".into(),
    };
    let app = spur_tui::app::App::new_with_license(None, false, cfg, state);

    let badge = app
        .license_badge_for_test()
        .expect("badge present on active seed");
    assert!(badge.label.contains("pro"));
}
```

- [ ] **Step 2: Add Active→Invalid transition test**

Append:

```rust
#[test]
fn active_to_invalid_transition_flips_badge_to_danger_tone() {
    use spur_acp::{
        LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent,
        LicenseSubjectKind, SpurEvent, SpurEventBody,
    };
    use spur_tui::components::status_bar::LicenseBadgeTone;
    use std::sync::Arc;

    let cfg = Arc::new(spur_acp::SpurConfig::default());
    let active = LicenseStateEvent {
        status: LicenseStatusEvent::Active,
        subject_kind: LicenseSubjectKind::User,
        plan: LicensePlan::Pro,
        features: Default::default(),
        expires_at: None,
        binding_mode: LicenseBindingMode::NodeLocked,
        offline_ok: true,
        status_text: "Cached".into(),
    };
    let mut app = spur_tui::app::App::new_with_license(None, false, cfg, active);

    let invalid = LicenseStateEvent {
        status: LicenseStatusEvent::Invalid,
        subject_kind: LicenseSubjectKind::User,
        plan: LicensePlan::Pro,
        features: Default::default(),
        expires_at: None,
        binding_mode: LicenseBindingMode::NodeLocked,
        offline_ok: false,
        status_text: "revoked".into(),
    };
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::LicenseUpdated { state: invalid }),
    );

    let badge = app
        .license_badge_for_test()
        .expect("badge present after transition");
    assert_eq!(badge.tone, LicenseBadgeTone::Danger);
    assert!(badge.label.contains("invalid"));
}
```

(If `LicenseBadgeTone` is not `pub`, promote it to `pub` in `crates/spur-tui/src/components/status_bar.rs:19` — it already is per the reviewed source.)

- [ ] **Step 3: Run the TUI tests**

Run:

```bash
cargo test -p spur-tui --test license_status_render
```

Expected: PASS on all four tests (two existing + two new).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/tests/license_status_render.rs
git commit -m "test(spur-tui): cover active-cached seed + active→invalid transition"
```

### Task 11: CLI — configured happy-path coverage via FakeProvider

**Intent:** prove `auth login/status/refresh/logout` against a scripted provider without live creds.

**Files:**
- Modify: `crates/spur-cli/src/commands/auth.rs` (introduce a pub test seam)
- Modify: `crates/spur-cli/src/commands/mod.rs` (re-export if needed)
- Create: `crates/spur-cli/tests/auth_fake_provider.rs`
- Modify: `crates/spur-cli/Cargo.toml`

- [ ] **Step 1: Add a pub test seam to `auth.rs`**

In `crates/spur-cli/src/commands/auth.rs`, refactor `run` to take an optional facade:

```rust
pub async fn run(command: AuthCommands) -> Result<()> {
    run_with_license(command, SpurLicense::from_env_or_disabled()).await
}

pub async fn run_with_license(command: AuthCommands, license: SpurLicense) -> Result<()> {
    match command {
        AuthCommands::Login { key } => login(&license, &key).await,
        AuthCommands::Status => {
            print_state(&license.current_state());
            Ok(())
        }
        AuthCommands::Refresh => refresh(&license).await,
        AuthCommands::Logout => logout(&license).await,
    }
}

async fn login(license: &SpurLicense, key: &str) -> Result<()> {
    ensure_configured(license)?;
    let state = license.activate(key).await?;
    print_state(&state);
    Ok(())
}

async fn refresh(license: &SpurLicense) -> Result<()> {
    ensure_configured(license)?;
    let state = license.validate().await?;
    print_state(&state);
    Ok(())
}

async fn logout(license: &SpurLicense) -> Result<()> {
    ensure_configured(license)?;
    let state = license.deactivate().await?;
    print_state(&state);
    Ok(())
}

fn ensure_configured(license: &SpurLicense) -> Result<()> {
    if matches!(license.current_state().status, LicenseStatus::ConfigError) {
        return Err(anyhow!(
            "license provider is not configured; set SPUR_LICENSESEAT_API_KEY and SPUR_LICENSESEAT_PRODUCT_SLUG"
        ));
    }
    Ok(())
}
```

Remove the old `configured_license()` helper (dead after this refactor). Update imports: `use spur_license::{LicenseState, LicenseStatus, SpurLicense};`.

- [ ] **Step 2: Wire the dev-dep feature**

Edit `crates/spur-cli/Cargo.toml` `[dev-dependencies]`:

```toml
spur-license = { workspace = true, features = ["test-support"] }
```

(Use `workspace = true` only if already declared; otherwise add the path directly.)

- [ ] **Step 3: Write the failing happy-path CLI tests**

Create `crates/spur-cli/tests/auth_fake_provider.rs`:

```rust
use std::sync::Arc;

use spur_cli::commands::auth::{run_with_license, AuthCommands};
use spur_license::test_support::FakeProvider;
use spur_license::{LicenseState, Plan, SpurLicense};

#[tokio::test]
async fn login_happy_path_activates_and_prints() {
    let fake = Arc::new(FakeProvider::new(LicenseState::inactive("fresh")));
    fake.push_activate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));
    let license = SpurLicense::from_provider(fake.clone());

    run_with_license(
        AuthCommands::Login {
            key: "test-key".into(),
        },
        license,
    )
    .await
    .expect("login happy path");
    assert_eq!(fake.validate_call_count(), 0);
}

#[tokio::test]
async fn refresh_invokes_validate_exactly_once() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    fake.push_validate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));
    let license = SpurLicense::from_provider(fake.clone());

    run_with_license(AuthCommands::Refresh, license)
        .await
        .expect("refresh");
    assert_eq!(fake.validate_call_count(), 1);
}

#[tokio::test]
async fn logout_invokes_deactivate_and_transitions_to_inactive() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone());

    run_with_license(AuthCommands::Logout, license.clone())
        .await
        .expect("logout");
    assert!(matches!(
        license.current_state().status,
        spur_license::LicenseStatus::Inactive
    ));
}
```

Expose `pub mod commands` and `pub mod auth` from the CLI crate if not already. If `spur_cli` is a binary-only crate (`[[bin]]` with no `[lib]`), add a minimal `src/lib.rs` re-export so integration tests can import it:

In `crates/spur-cli/Cargo.toml`:

```toml
[lib]
path = "src/lib.rs"
```

And `crates/spur-cli/src/lib.rs` (new):

```rust
pub mod commands {
    pub mod auth {
        pub use crate::commands::auth::{run, run_with_license, AuthCommands};
    }
}
```

(If the existing `src/main.rs` already declares `mod commands;`, convert it to `pub mod commands;` inside `lib.rs` instead — simpler.)

- [ ] **Step 4: Run the tests**

Run:

```bash
cargo test -p spur-cli --test auth_fake_provider
```

Expected: all three PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/auth.rs \
        crates/spur-cli/src/lib.rs \
        crates/spur-cli/Cargo.toml \
        crates/spur-cli/tests/auth_fake_provider.rs
git commit -m "test(spur-cli): happy-path auth coverage via FakeProvider"
```

---

## Phase 3 — Polish + Rollout

### Task 12: D5 — Preserve prior `Invalid` status_text on transient failures

**Files:**
- Modify: `crates/spur-core/src/license_runtime.rs`

- [ ] **Step 1: Write a failing test**

Append to `crates/spur-core/tests/license_runtime_fake_provider.rs`:

```rust
#[tokio::test]
async fn degraded_from_preserves_invalid_status_text() {
    use spur_license::{LicenseState, LicenseStatus};

    // degraded_from is private; we exercise via a seed that's already Invalid
    // and a validate error, then observe the funnel snapshot.
    let mut invalid = LicenseState::inactive("revoked");
    invalid.status = LicenseStatus::Invalid;
    let fake = Arc::new(
        FakeProvider::new(invalid)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_millis(20),
                heartbeat_interval: Duration::from_secs(3600),
            }),
    );
    fake.push_validate_result(Err(spur_license::LicenseError::Provider(
        "transient".into(),
    )));

    let license = SpurLicense::from_provider(fake);
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    // Find the first non-initial funnel emission after the validate fires.
    let mut last_text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(40), bcast_rx.recv()).await
        {
            if let SpurEventBody::LicenseUpdated { state } = ev.body {
                last_text = state.status_text;
            }
        }
    }
    handle.abort();
    assert_eq!(
        last_text, "revoked",
        "transient validate error must not overwrite prior Invalid text"
    );
}
```

- [ ] **Step 2: Run to observe failure**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider degraded_from_preserves_invalid_status_text
```

Expected: FAIL — `last_text` is `"Validation failed: transient"`.

- [ ] **Step 3: Apply the fix**

Edit `crates/spur-core/src/license_runtime.rs` `degraded_from` at `:83-89`:

```rust
fn degraded_from(mut state: LicenseState, message: String) -> LicenseState {
    match state.status {
        LicenseStatus::Active => {
            state.status = LicenseStatus::Degraded;
            state.status_text = message;
        }
        LicenseStatus::Degraded => {
            // already degraded; refresh the reason
            state.status_text = message;
        }
        LicenseStatus::Inactive | LicenseStatus::Invalid | LicenseStatus::ConfigError => {
            // keep authoritative text — a transient error does not overwrite
            // a prior hard-fail reason.
        }
    }
    state
}
```

- [ ] **Step 4: Run test to confirm PASS**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/license_runtime.rs \
        crates/spur-core/tests/license_runtime_fake_provider.rs
git commit -m "fix(spur-core): preserve Invalid/Inactive status_text on transient errors (D5)"
```

### Task 13: D3 — Initial validate after 30s jitter

**Files:**
- Modify: `crates/spur-core/src/license_runtime.rs`

- [ ] **Step 1: Write a failing test**

Append to `crates/spur-core/tests/license_runtime_fake_provider.rs`:

```rust
#[tokio::test]
async fn runtime_validates_within_first_minute_after_boot() {
    use spur_license::{LicenseState, Plan};

    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(
        FakeProvider::new(seed)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_secs(3600),
                heartbeat_interval: Duration::from_secs(3600),
            }),
    );
    fake.push_validate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));

    let license = SpurLicense::from_provider(fake.clone());
    let (bcast_tx, _rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.abort();

    assert_eq!(
        fake.validate_call_count(),
        1,
        "runtime must perform an initial validate shortly after startup \
         even when validate_interval is long"
    );
}
```

Note: in CI, real time is used. The 200ms timeout will fail the assertion unless we shorten the initial validate. Use `tokio::time::pause()` + `advance()` for determinism. Rewrite:

```rust
#[tokio::test(start_paused = true)]
async fn runtime_validates_within_first_minute_after_boot() {
    // ... (setup same as above) ...
    let license = SpurLicense::from_provider(fake.clone());
    let (bcast_tx, _rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    handle.abort();

    assert_eq!(fake.validate_call_count(), 1);
}
```

- [ ] **Step 2: Run to observe failure**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider runtime_validates_within_first_minute_after_boot
```

Expected: FAIL — `validate_call_count = 0` because the runtime sleeps for the full 3600s validate_interval before first tick.

- [ ] **Step 3: Apply the fix**

Edit `crates/spur-core/src/license_runtime.rs` at `:22`:

```rust
    // D3: initial validate fires after a short interval regardless of configured
    // cadence, so stale cached state doesn't persist up to `validate_interval`.
    let initial_delay = std::cmp::min(current_validate_delay, std::time::Duration::from_secs(30));
    let mut validate_sleep = Box::pin(tokio::time::sleep(initial_delay));
```

- [ ] **Step 4: Run to confirm PASS**

Run:

```bash
cargo test -p spur-core --test license_runtime_fake_provider runtime_validates_within_first_minute_after_boot
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/license_runtime.rs \
        crates/spur-core/tests/license_runtime_fake_provider.rs
git commit -m "fix(spur-core): initial validate within 30s of boot (D3)"
```

### Task 14: H5 — `spur auth status --format json`

**Files:**
- Modify: `crates/spur-cli/src/commands/auth.rs`
- Modify: `crates/spur-cli/tests/auth_cli.rs`

- [ ] **Step 1: Write a failing integration test**

Append to `crates/spur-cli/tests/auth_cli.rs`:

```rust
#[test]
fn auth_status_json_emits_parseable_object() {
    let _guard = LOCK.lock().unwrap();
    let output = spur()
        .args(["auth", "status", "--format", "json"])
        .output()
        .expect("spawn spur auth status --format json");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("valid JSON on stdout");
    assert!(value.get("status").is_some(), "missing status field: {stdout}");
    assert!(value.get("plan").is_some(), "missing plan field: {stdout}");
}
```

Add `serde_json` to `[dev-dependencies]` in `crates/spur-cli/Cargo.toml` if not already present.

- [ ] **Step 2: Run to observe failure**

Run:

```bash
cargo test -p spur-cli --test auth_cli auth_status_json_emits_parseable_object
```

Expected: FAIL — `--format` is an unknown argument.

- [ ] **Step 3: Add the `--format` option**

Edit `crates/spur-cli/src/commands/auth.rs`:

```rust
use clap::{Subcommand, ValueEnum};

#[derive(Copy, Clone, Debug, ValueEnum, Default)]
pub enum OutputFormat {
    #[default]
    Plain,
    Json,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommands {
    Login {
        #[arg(long)]
        key: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
    Status {
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
    Refresh {
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
    Logout {
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
}
```

Update `run_with_license` to route by format:

```rust
pub async fn run_with_license(command: AuthCommands, license: SpurLicense) -> Result<()> {
    match command {
        AuthCommands::Login { key, format } => {
            let state = login_inner(&license, &key).await?;
            print_by_format(&state, format);
            Ok(())
        }
        AuthCommands::Status { format } => {
            let state = license.current_state();
            print_by_format(&state, format);
            Ok(())
        }
        AuthCommands::Refresh { format } => {
            let state = refresh_inner(&license).await?;
            print_by_format(&state, format);
            Ok(())
        }
        AuthCommands::Logout { format } => {
            let state = logout_inner(&license).await?;
            print_by_format(&state, format);
            Ok(())
        }
    }
}

async fn login_inner(license: &SpurLicense, key: &str) -> Result<LicenseState> {
    ensure_configured(license)?;
    Ok(license.activate(key).await?)
}

async fn refresh_inner(license: &SpurLicense) -> Result<LicenseState> {
    ensure_configured(license)?;
    Ok(license.validate().await?)
}

async fn logout_inner(license: &SpurLicense) -> Result<LicenseState> {
    ensure_configured(license)?;
    Ok(license.deactivate().await?)
}

fn print_by_format(state: &LicenseState, format: OutputFormat) {
    match format {
        OutputFormat::Plain => print_state(state),
        OutputFormat::Json => {
            // Serialize via the ACP mirror type for a stable external schema.
            let event = spur_core::license_runtime::to_event_state(state.clone());
            match serde_json::to_string(&event) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("{{\"error\":\"serialization failed: {e}\"}}"),
            }
        }
    }
}
```

Add `serde_json` to `[dependencies]` in `crates/spur-cli/Cargo.toml` (if not already) and `spur-core = { workspace = true }` (should already be present).

- [ ] **Step 4: Run to confirm PASS**

Run:

```bash
cargo test -p spur-cli --test auth_cli
```

Expected: all PASS (existing + new).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/auth.rs \
        crates/spur-cli/tests/auth_cli.rs \
        crates/spur-cli/Cargo.toml
git commit -m "feat(spur-cli): spur auth {status,login,refresh,logout} --format json"
```

### Task 15: Close original plan's Task 6 + rollout notes

**Files:**
- Modify: `docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md`
- Create: `docs/superpowers/plans/2026-04-19-licensing-hardening-notes.md`

- [ ] **Step 1: Run all verification commands and capture output**

Run each and paste the last 5 lines into a scratch file:

```bash
cargo test -p spur-license 2>&1 | tail -5
cargo test -p spur-license --features test-support 2>&1 | tail -5
cargo test -p spur-acp license_events_roundtrip 2>&1 | tail -5
cargo test -p spur-tui license_status_render 2>&1 | tail -5
cargo test -p spur-cli auth_cli auth_fake_provider 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -10
cargo clippy --workspace -- -D warnings 2>&1 | tail -10
cargo fmt --all --check 2>&1 | tail -5
```

- [ ] **Step 2: Mark Task 6 checkboxes**

Edit `docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md` Task 6 section (lines 309–342). Change each `- [ ]` to `- [x]` for steps that were executed successfully. For any that failed, leave as `- [ ]` and open a follow-up.

- [ ] **Step 3: Write rollout notes**

Create `docs/superpowers/plans/2026-04-19-licensing-hardening-notes.md`:

```markdown
# Licensing Hardening Rollout Notes

## Phase 0 outcomes

- **Gate 1 (C9 duplicate emission):** <CONFIRMED | RETRACTED> per `docs/rca/2026-04-19-licenseseat-emission-audit.md`.
- **Gate 2 (D1 heartbeat gating):** <EXECUTE | SKIP> per RCA.
- **Gate 3 (G2 cached plan):** <EXECUTE | MARKER-ONLY> per RCA.

## Cache-path configurability

LicenseSeat 0.5.3 stores cached license state in `<document the path from the RCA>`. SPUR does not override this today; a future spec may allow operators to relocate it for air-gapped or read-only environments.

## Background refresh

- **TUI (`spur watch`)**: the orchestrator's `spawn_license_runtime` runs background validate and (when applicable) heartbeat loops for the life of the session.
- **Non-TUI commands (`spur auth ...`, `spur run ...`)**: no background refresh. Operators must run `spur auth refresh` to force a validate.

## Air-gapped activation

- `spur auth login --key <KEY>` performs a network activate; offline activation is not yet supported.
- Open follow-up: provider's machine-file activation flow for air-gapped hosts.

## Known deferred items

- B3 — split `SpurLicense` construction from background-start.
- C2 — migrate `LicenseSeatProvider` state lock from `std::sync::RwLock` to `parking_lot::RwLock`.
- H1 — sanitize upstream error strings before surfacing in `auth status`.
- Typed state-machine refactor of `LicenseState`.
- Second provider (self-hosted / enterprise).
- Trial UX.
```

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/plans/2026-04-18-spur-licensing-architecture.md \
        docs/superpowers/plans/2026-04-19-licensing-hardening-notes.md
git commit -m "docs(licensing): close Task 6 checkboxes + rollout notes"
```

---

## Exit Criteria

- `docs/rca/2026-04-19-licenseseat-emission-audit.md` committed with decisive Gate answers.
- `SpurLicense::from_provider` + `FakeProvider` + proptest invariant merged.
- Every confirmed 🔴 (C9, D1) has a regression test passing and a committed fix.
- G2 cold-start hydration landed (or marker + rollout note if Gate 3 = NO).
- Runtime, TUI, and CLI have FakeProvider-backed tests for the previously uncovered paths.
- `spur auth status --format json` emits a stable `LicenseStateEvent` JSON payload.
- Original plan's Task 6 checkboxes filled; rollout notes committed.
- `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all --check` all pass.
