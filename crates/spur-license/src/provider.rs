use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::broadcast;

use crate::{LicenseEvent, LicenseState, Result};

#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    pub validate_interval: Duration,
    pub heartbeat_interval: Duration,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            validate_interval: Duration::from_secs(3600),
            heartbeat_interval: Duration::from_secs(300),
        }
    }
}

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
///
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
#[async_trait]
pub trait LicenseProvider: Send + Sync {
    fn current_state(&self) -> LicenseState;
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent>;
    fn refresh_policy(&self) -> RefreshPolicy;
    fn has_entitlement(&self, feature: &str) -> bool;

    /// Whether the runtime should periodically heartbeat for this subject.
    /// Default `false`. Override in adapters when the provider's lease
    /// model mandates it. (As of Phase-0 audit, `licenseseat` has no
    /// per-mode gate; `LicenseSeatProvider` uses the default.)
    fn requires_heartbeat(&self) -> bool {
        false
    }

    async fn activate(&self, key: &str) -> Result<LicenseState>;
    async fn validate(&self) -> Result<LicenseState>;
    async fn heartbeat(&self) -> Result<LicenseState>;
    async fn deactivate(&self) -> Result<LicenseState>;
}
