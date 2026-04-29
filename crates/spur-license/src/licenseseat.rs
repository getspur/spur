use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use licenseseat::{Config, EventKind, LicenseSeat};
use tokio::sync::broadcast;

use crate::provider::{LicenseProvider, RefreshPolicy};
use crate::{
    BindingMode, LicenseError, LicenseEvent, LicenseEventKind, LicenseState, LicenseStatus, Plan,
    Result, SubjectKind,
};

const LICENSESEAT_API_KEY_ENV: &str = "SPUR_LICENSESEAT_API_KEY";
const LICENSESEAT_PRODUCT_SLUG_ENV: &str = "SPUR_LICENSESEAT_PRODUCT_SLUG";

pub fn from_env() -> Result<LicenseSeatProvider> {
    let api_key = std::env::var(LICENSESEAT_API_KEY_ENV).map_err(|_| {
        LicenseError::NotConfigured(format!(
            "missing environment variable {LICENSESEAT_API_KEY_ENV}"
        ))
    })?;
    let product_slug = std::env::var(LICENSESEAT_PRODUCT_SLUG_ENV).map_err(|_| {
        LicenseError::NotConfigured(format!(
            "missing environment variable {LICENSESEAT_PRODUCT_SLUG_ENV}"
        ))
    })?;
    Ok(LicenseSeatProvider::new(api_key, product_slug))
}

pub fn from_env_or_disabled() -> Arc<dyn LicenseProvider> {
    // Provider selection (Option A from 2026-04-19 plan, Task 14b):
    //
    //   1. Runtime env vars override (developer / CI override path).
    //   2. Build-time baked credentials present + cached license on disk:
    //      use LicenseSeatProvider directly (already-activated user).
    //   3. Build-time baked credentials present + NO cached license:
    //      present as Community via CommunityProviderWithUpgrade, which
    //      delegates `activate()` to a baked LicenseSeatProvider so
    //      `spur auth login --key …` works on a fresh install with zero
    //      env vars. After successful activation the SDK persists a
    //      cached license, and the next process launch promotes to
    //      LicenseSeatProvider directly via branch (2).
    //   4. Neither runtime env vars nor baked credentials: pure Community
    //      (no upgrade path \u2014 unsupported build configuration).
    match (
        std::env::var(LICENSESEAT_API_KEY_ENV),
        std::env::var(LICENSESEAT_PRODUCT_SLUG_ENV),
    ) {
        // Branch 1: runtime override.
        (Ok(api_key), Ok(product_slug)) => {
            return Arc::new(LicenseSeatProvider::new(api_key, product_slug));
        }
        // Both unset: fall through to baked-credentials check.
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {}
        // Partial env: loud config error (developer mistake).
        _ => {
            return Arc::new(DisabledProvider::new(
                "incomplete licensing environment configuration",
            ));
        }
    }

    // Branches 2 + 3: baked credentials path.
    if let Some((api_key, product_slug)) = crate::build_constants::baked_credentials() {
        let seat = LicenseSeatProvider::new(api_key.into(), product_slug.into());
        if seat.has_cached_license() {
            // Branch 2: already activated; promote directly.
            return Arc::new(seat);
        }
        // Branch 3: no cache; present as Community but route activation
        // to the baked LicenseSeatProvider.
        return Arc::new(crate::community::CommunityProviderWithUpgrade::new(
            crate::policy::PolicyResolver::embedded(),
            Arc::new(seat),
        ));
    }

    // Branch 4: no baked credentials (developer build of an unconfigured
    // fork). Pure Community, no upgrade path.
    Arc::new(crate::CommunityProvider::new(
        crate::policy::PolicyResolver::embedded(),
    ))
}

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
/// tracked separately. See `bd-3v05` (filed as the spec's
/// "bd-22q.16" follow-up).
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

impl LicenseSeatProvider {
    pub fn new(api_key: String, product_slug: String) -> Self {
        let mut config = Config::new(api_key, product_slug);
        config.telemetry_enabled = false;
        config.app_version = Some(env!("CARGO_PKG_VERSION").into());

        let refresh_policy = RefreshPolicy {
            validate_interval: config.auto_validate_interval,
            heartbeat_interval: config.heartbeat_interval,
        };

        let sdk = LicenseSeat::new(config);
        let initial_state = match sdk.current_license() {
            Some(cached) => hydrate_from_cached(&cached),
            None => LicenseState::inactive("No active license"),
        };

        let (events_tx, _) = broadcast::channel(64);
        let provider = Self {
            sdk,
            state: Arc::new(RwLock::new(initial_state)),
            operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            events_tx,
            refresh_policy,
        };
        provider.spawn_sdk_event_bridge();
        provider
    }

    /// True iff the underlying SDK has a cached license on disk.
    /// Used by `from_env_or_disabled` to decide whether to expose
    /// `LicenseSeatProvider` directly (already-activated user) or wrap
    /// it in `CommunityProviderWithUpgrade` (fresh install).
    pub fn has_cached_license(&self) -> bool {
        self.sdk.current_license().is_some()
    }

    fn replace_state(&self, next: LicenseState, kind: LicenseEventKind, message: Option<String>) {
        if let Ok(mut state) = self.state.write() {
            *state = next.clone();
        }
        let _ = self.events_tx.send(LicenseEvent {
            kind,
            state: next,
            message,
        });
    }

    fn spawn_sdk_event_bridge(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut rx = self.sdk.subscribe();
        let state = Arc::clone(&self.state);
        let tx = self.events_tx.clone();
        handle.spawn(async move {
            while let Ok(event) = rx.recv().await {
                // C9 dedup (see docs/rca/2026-04-19-licenseseat-emission-audit.md
                // Gate 1). The explicit activate/validate/heartbeat/deactivate
                // handlers ALREADY broadcast via `replace_state` with an
                // authoritative post-mutation snapshot. The SDK also fires
                // these kinds synchronously during each explicit call, so
                // forwarding them here produces a stale-then-fresh duplicate.
                // Drop the handler-originated kinds; keep autonomous / server-
                // push kinds (revocation, offline-verification failures,
                // license-loaded).
                if is_handler_originated(&event.kind) {
                    continue;
                }
                let kind = map_event_kind(event.kind);
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

    fn current_snapshot(&self) -> LicenseState {
        self.state
            .read()
            .map(|state| state.clone())
            .unwrap_or_else(|_| LicenseState::inactive("License state unavailable"))
    }

    fn degrade_current(&self, message: impl Into<String>, kind: LicenseEventKind) -> LicenseState {
        let mut next = self.current_snapshot();
        next.status = LicenseStatus::Degraded;
        next.status_text = message.into();
        self.replace_state(next.clone(), kind, Some(next.status_text.clone()));
        next
    }
}

#[cfg(test)]
impl LicenseSeatProvider {
    /// In-crate-test-only handle to the operation lock for bd-22q.15
    /// cross_method_race tests. NEVER expose `pub`: external crates
    /// could acquire the lock and stall production mutations.
    pub(crate) fn operation_lock_handle(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.operation_lock)
    }
}

#[async_trait]
impl LicenseProvider for LicenseSeatProvider {
    fn current_state(&self) -> LicenseState {
        if self.sdk.current_license().is_some() {
            let mut state = self.current_snapshot();
            if matches!(
                state.status,
                LicenseStatus::Inactive | LicenseStatus::ConfigError
            ) {
                state.status = LicenseStatus::Active;
                state.status_text = "Cached license available".into();
            }
            state
        } else {
            self.current_snapshot()
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        self.events_tx.subscribe()
    }

    fn refresh_policy(&self) -> RefreshPolicy {
        self.refresh_policy
    }

    fn requires_heartbeat(&self) -> bool {
        // Upstream licenseseat 0.5.3 has no per-mode heartbeat policy;
        // the SPUR-layer coarse gate (state.is_active() &&
        // binding_mode != Unknown) drives suppression. See
        // docs/rca/2026-04-19-licenseseat-emission-audit.md Gate 2.
        true
    }

    fn has_entitlement(&self, feature: &str) -> bool {
        self.sdk.has_entitlement(feature)
    }

    async fn activate(&self, key: &str) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        let response = self
            .sdk
            .activate(key)
            .await
            .map_err(|err| LicenseError::Provider(err.to_string()))?;

        let mut next = LicenseState::active_cached();
        next.plan = response
            .trusted_license
            .as_ref()
            .map(|license| Plan::from_key(&license.plan_key))
            .unwrap_or(Plan::Unknown);
        next.status_text = "License activated".into();
        self.replace_state(next.clone(), LicenseEventKind::Activated, None);
        Ok(next)
    }

    async fn validate(&self) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        let result = self
            .sdk
            .validate()
            .await
            .map_err(|err| LicenseError::Provider(err.to_string()))?;

        let next = if result.valid {
            let mut state = LicenseState::active_validated(
                Plan::from_key(&result.license.plan_key),
                result
                    .license
                    .active_entitlements
                    .iter()
                    .map(|entitlement| entitlement.key.clone())
                    .collect::<BTreeSet<_>>(),
            );
            state.expires_at = result.license.expires_at;
            state.status_text = result
                .warnings
                .as_ref()
                .filter(|warnings| !warnings.is_empty())
                .map(|warnings| {
                    warnings
                        .iter()
                        .map(|warning| warning.message.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "License validated".into());
            state
        } else {
            let mut state = LicenseState::inactive(
                result
                    .code
                    .as_deref()
                    .unwrap_or("license validation failed"),
            );
            state.status = LicenseStatus::Invalid;
            state
        };

        self.replace_state(
            next.clone(),
            if next.is_active() {
                LicenseEventKind::Validated
            } else {
                LicenseEventKind::ValidationFailed
            },
            None,
        );
        Ok(next)
    }

    async fn heartbeat(&self) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        match self.sdk.heartbeat().await {
            Ok(_) => {
                let mut next = self.current_snapshot();
                if next.is_active() && matches!(next.status, LicenseStatus::Degraded) {
                    next.status = LicenseStatus::Active;
                    next.status_text = "Heartbeat restored connectivity".into();
                }
                self.replace_state(next.clone(), LicenseEventKind::HeartbeatOk, None);
                Ok(next)
            }
            Err(err) => {
                let degraded = self.degrade_current(
                    format!("Heartbeat failed: {err}"),
                    LicenseEventKind::HeartbeatFailed,
                );
                Err(LicenseError::Provider(degraded.status_text))
            }
        }
    }

    async fn deactivate(&self) -> Result<LicenseState> {
        let _guard = self.operation_lock.lock().await;
        self.sdk
            .deactivate()
            .await
            .map_err(|err| LicenseError::Provider(err.to_string()))?;
        let next = LicenseState::inactive("License deactivated");
        self.replace_state(next.clone(), LicenseEventKind::Deactivated, None);
        Ok(next)
    }
}

pub struct DisabledProvider {
    state: Arc<RwLock<LicenseState>>,
    events_tx: broadcast::Sender<LicenseEvent>,
}

impl DisabledProvider {
    pub fn new(message: impl Into<String>) -> Self {
        let state = LicenseState::config_error(message);
        let (events_tx, _) = broadcast::channel(8);
        Self {
            state: Arc::new(RwLock::new(state)),
            events_tx,
        }
    }

    fn snapshot(&self) -> LicenseState {
        self.state
            .read()
            .map(|state| state.clone())
            .unwrap_or_else(|_| LicenseState::inactive("License state unavailable"))
    }
}

#[async_trait]
impl LicenseProvider for DisabledProvider {
    fn current_state(&self) -> LicenseState {
        self.snapshot()
    }

    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        self.events_tx.subscribe()
    }

    fn refresh_policy(&self) -> RefreshPolicy {
        RefreshPolicy::default()
    }

    fn has_entitlement(&self, _feature: &str) -> bool {
        false
    }

    async fn activate(&self, _key: &str) -> Result<LicenseState> {
        Err(LicenseError::NotConfigured(self.snapshot().status_text))
    }

    async fn validate(&self) -> Result<LicenseState> {
        Ok(self.snapshot())
    }

    async fn heartbeat(&self) -> Result<LicenseState> {
        Ok(self.snapshot())
    }

    async fn deactivate(&self) -> Result<LicenseState> {
        Ok(self.snapshot())
    }
}

/// Returns `true` for `EventKind`s that are emitted synchronously inside
/// the explicit handler methods (`activate`, `validate`, `heartbeat`,
/// `deactivate`) and therefore already covered by a `replace_state` broadcast.
/// Forwarding these from the bridge would produce a stale-then-fresh duplicate
/// pair on every explicit call (C9).
///
/// Verified against upstream enum in `licenseseat-0.5.3/src/events.rs`.
/// See: docs/rca/2026-04-19-licenseseat-emission-audit.md Gate 1 dedup table.
fn is_handler_originated(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::ActivationStart
            | EventKind::ActivationSuccess
            | EventKind::ActivationError
            | EventKind::ValidationStart
            | EventKind::ValidationSuccess
            | EventKind::ValidationFailed
            | EventKind::ValidationError
            | EventKind::ValidationAuthFailed
            | EventKind::HeartbeatSuccess
            | EventKind::HeartbeatError
            | EventKind::DeactivationStart
            | EventKind::DeactivationSuccess
            | EventKind::DeactivationError
    )
}

fn map_event_kind(kind: EventKind) -> LicenseEventKind {
    match kind {
        EventKind::ActivationSuccess => LicenseEventKind::Activated,
        EventKind::ActivationError => LicenseEventKind::ActivationFailed,
        EventKind::ValidationSuccess => LicenseEventKind::Validated,
        EventKind::ValidationFailed
        | EventKind::ValidationError
        | EventKind::ValidationOfflineFailed
        | EventKind::ValidationAuthFailed
        | EventKind::ValidationAutoFailed
        | EventKind::LicenseRevoked
        | EventKind::OfflineValidationFailed
        | EventKind::OfflineTokenVerificationFailed
        | EventKind::MachineFileVerificationFailed => LicenseEventKind::ValidationFailed,
        EventKind::DeactivationSuccess => LicenseEventKind::Deactivated,
        EventKind::DeactivationError => LicenseEventKind::DeactivationFailed,
        EventKind::HeartbeatSuccess => LicenseEventKind::HeartbeatOk,
        EventKind::HeartbeatError => LicenseEventKind::HeartbeatFailed,
        _ => LicenseEventKind::Validated,
    }
}

pub fn classify_binding_mode(active: bool) -> BindingMode {
    if active {
        BindingMode::NodeLocked
    } else {
        BindingMode::Unknown
    }
}

pub fn classify_subject(active: bool) -> SubjectKind {
    if active {
        SubjectKind::User
    } else {
        SubjectKind::Unknown
    }
}

fn hydrate_from_cached(cached: &licenseseat::License) -> LicenseState {
    // Phase-0 Gate 3 confirmed `current_license()` returns a cached License
    // that wraps `trusted_license: Option<LicenseResponse>`, which carries
    // `plan_key` and `active_entitlements`. If `trusted_license` is absent
    // (e.g., cache stale or minimal), fall back to the prior behavior.
    let (plan, features, expires_at) = match cached.trusted_license.as_ref() {
        Some(resp) => (
            Plan::from_key(&resp.plan_key),
            resp.active_entitlements
                .iter()
                .map(|e| e.key.clone())
                .collect::<BTreeSet<String>>(),
            resp.expires_at,
        ),
        None => (Plan::Unknown, BTreeSet::new(), None),
    };
    LicenseState {
        status: LicenseStatus::Active,
        subject_kind: SubjectKind::User,
        plan,
        features,
        expires_at,
        binding_mode: BindingMode::NodeLocked,
        offline_ok: true,
        status_text: "Cached license available".into(),
    }
}

#[cfg(test)]
mod dedup_tests {
    use super::is_handler_originated;
    use licenseseat::EventKind;

    #[test]
    fn handler_originated_covers_all_explicit_kinds() {
        for kind in [
            EventKind::ActivationStart,
            EventKind::ActivationSuccess,
            EventKind::ActivationError,
            EventKind::ValidationStart,
            EventKind::ValidationSuccess,
            EventKind::ValidationFailed,
            EventKind::ValidationError,
            EventKind::ValidationAuthFailed,
            EventKind::HeartbeatSuccess,
            EventKind::HeartbeatError,
            EventKind::DeactivationStart,
            EventKind::DeactivationSuccess,
            EventKind::DeactivationError,
        ] {
            assert!(
                is_handler_originated(&kind),
                "expected {:?} to be classified as handler-originated",
                kind,
            );
        }
    }

    #[test]
    fn handler_originated_excludes_autonomous_kinds() {
        // Autonomous/server-push kinds must NOT be dropped by the bridge.
        // If this test fails, the bridge will silence revocations and
        // offline-validation failures — a severe regression.
        for kind in [
            EventKind::LicenseRevoked,
            EventKind::LicenseLoaded,
            EventKind::ValidationAutoFailed,
        ] {
            assert!(
                !is_handler_originated(&kind),
                "autonomous kind {:?} must NOT be classified as handler-originated",
                kind,
            );
        }
    }
}

#[cfg(test)]
mod cross_method_race {
    //! Lock-discipline canaries for bd-22q.15. These tests verify that
    //! `LicenseSeatProvider`'s mutating methods participate in the
    //! `operation_lock` discipline. They do NOT directly drive the
    //! validate-vs-deactivate race scenario — that requires SDK-mock
    //! infrastructure deferred to bd-25tg (filed as the spec's
    //! "bd-22q.17" follow-up). Instead, they prove:
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

    /// Test 2: activate() acquires operation_lock at entry.
    #[tokio::test(start_paused = true)]
    async fn activate_blocks_on_externally_held_operation_lock() {
        let provider = LicenseSeatProvider::new("test-key".to_string(), "test-product".to_string());
        let external_lock = provider.operation_lock_handle().lock_owned().await;

        let provider_clone = provider.clone();
        let activate_task = tokio::spawn(async move { provider_clone.activate("X").await });

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
        let provider = LicenseSeatProvider::new("test-key".to_string(), "test-product".to_string());
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
        let provider = LicenseSeatProvider::new("test-key".to_string(), "test-product".to_string());
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
        let provider = LicenseSeatProvider::new("test-key".to_string(), "test-product".to_string());
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
}
