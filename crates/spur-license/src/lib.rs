mod community;
mod gate;
mod install_id;
mod licenseseat;
pub mod policy;
pub mod provider;
mod quota;
mod snapshot;
mod tier;
pub mod upgrade_cta;

pub use community::CommunityProvider;
pub use gate::{require_feature, FeatureGate, FeatureGateError};

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::install_id::InstallId;
pub use crate::licenseseat::{
    classify_binding_mode, classify_subject, from_env, from_env_or_disabled,
};
pub use crate::policy::FlagEvaluator;
pub use crate::policy::{FeatureKey, FlagKey};
pub use crate::provider::{LicenseProvider, RefreshPolicy};
pub use crate::quota::{QuotaKey, QuotaValue};
pub use crate::snapshot::EntitlementSnapshot;
pub use crate::tier::Tier;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseStatus {
    Inactive,
    Active,
    Degraded,
    Invalid,
    ConfigError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubjectKind {
    User,
    Organization,
    Ci,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingMode {
    NodeLocked,
    FloatingCi,
    Organization,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan {
    Community,
    StarterLtd,
    BuilderLtd,
    FounderLtd,
    Pro,
    Team,
    Enterprise,
    Unknown,
}

impl Plan {
    pub fn from_key(key: &str) -> Self {
        match key {
            "community" => Self::Community,
            "starter_ltd" | "starter-ltd" => Self::StarterLtd,
            "builder_ltd" | "builder-ltd" => Self::BuilderLtd,
            "founder_ltd" | "founder-ltd" => Self::FounderLtd,
            "pro" => Self::Pro,
            "team" => Self::Team,
            "enterprise" => Self::Enterprise,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Community => "Community",
            Self::StarterLtd => "Starter LTD",
            Self::BuilderLtd => "Builder LTD",
            Self::FounderLtd => "Founder LTD",
            Self::Pro => "Pro",
            Self::Team => "Team",
            Self::Enterprise => "Enterprise",
            Self::Unknown => "Licensed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseState {
    pub status: LicenseStatus,
    pub subject_kind: SubjectKind,
    pub plan: Plan,
    pub features: BTreeSet<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub binding_mode: BindingMode,
    pub offline_ok: bool,
    pub status_text: String,
}

impl LicenseState {
    pub fn inactive(message: impl Into<String>) -> Self {
        Self {
            status: LicenseStatus::Inactive,
            subject_kind: SubjectKind::Unknown,
            plan: Plan::Community,
            features: BTreeSet::new(),
            expires_at: None,
            binding_mode: BindingMode::Unknown,
            offline_ok: false,
            status_text: message.into(),
        }
    }

    pub fn config_error(message: impl Into<String>) -> Self {
        let mut state = Self::inactive(message);
        state.status = LicenseStatus::ConfigError;
        state.status_text = state.status_text.trim().to_string();
        state
    }

    pub fn active_cached() -> Self {
        Self {
            status: LicenseStatus::Active,
            subject_kind: SubjectKind::User,
            plan: Plan::Unknown,
            features: BTreeSet::new(),
            expires_at: None,
            binding_mode: BindingMode::NodeLocked,
            offline_ok: true,
            status_text: "Cached license available".into(),
        }
    }

    pub fn active_validated(plan: Plan, features: BTreeSet<String>) -> Self {
        Self {
            status: LicenseStatus::Active,
            subject_kind: SubjectKind::User,
            plan,
            features,
            expires_at: None,
            binding_mode: BindingMode::NodeLocked,
            offline_ok: true,
            status_text: "License validated".into(),
        }
    }

    pub fn active_community(features: BTreeSet<String>) -> Self {
        Self {
            status: LicenseStatus::Active,
            subject_kind: SubjectKind::User,
            plan: Plan::Community,
            features,
            expires_at: None,
            binding_mode: BindingMode::Unknown,
            offline_ok: true,
            status_text: "Community tier".into(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, LicenseStatus::Active | LicenseStatus::Degraded)
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self.status, LicenseStatus::Degraded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseEvent {
    pub kind: LicenseEventKind,
    pub state: LicenseState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseEventKind {
    Activated,
    ActivationFailed,
    Validated,
    ValidationFailed,
    Deactivated,
    DeactivationFailed,
    HeartbeatOk,
    HeartbeatFailed,
}

#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    #[error("{0}")]
    NotConfigured(String),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    PolicyMalformed(String),
}

pub type Result<T> = std::result::Result<T, LicenseError>;

#[derive(Clone)]
pub struct SpurLicense {
    provider: Arc<dyn LicenseProvider>,
    feature_gate: Arc<FeatureGate>,
}

impl SpurLicense {
    /// Construct a facade backed by an arbitrary provider. Primary use is
    /// test injection via `FakeProvider`; production paths should prefer
    /// `from_env` / `from_env_or_disabled`.
    pub fn from_provider(
        provider: std::sync::Arc<dyn crate::provider::LicenseProvider>,
        feature_gate: Arc<FeatureGate>,
    ) -> Self {
        Self {
            provider,
            feature_gate,
        }
    }

    pub fn from_env() -> Result<Self> {
        let provider = Arc::new(crate::licenseseat::from_env()?);
        let policy = crate::policy::PolicyResolver::with_default_overlay();
        let feature_gate = Arc::new(FeatureGate::new(policy));
        feature_gate.update_state(&provider.current_state());
        Ok(Self {
            provider,
            feature_gate,
        })
    }

    pub fn from_env_or_disabled() -> Self {
        let provider = crate::licenseseat::from_env_or_disabled();
        let policy = crate::policy::PolicyResolver::with_default_overlay();
        let feature_gate = Arc::new(FeatureGate::new(policy));
        feature_gate.update_state(&provider.current_state());
        Self {
            provider,
            feature_gate,
        }
    }

    pub fn feature_gate(&self) -> Arc<FeatureGate> {
        Arc::clone(&self.feature_gate)
    }

    pub fn current_state(&self) -> LicenseState {
        self.provider.current_state()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<LicenseEvent> {
        self.provider.subscribe()
    }

    pub fn refresh_policy(&self) -> RefreshPolicy {
        self.provider.refresh_policy()
    }

    pub fn requires_heartbeat(&self) -> bool {
        self.provider.requires_heartbeat()
    }

    pub fn has_entitlement(&self, feature: &str) -> bool {
        self.provider.has_entitlement(feature)
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_plan_maps_to_unknown() {
        assert_eq!(Plan::from_key("mystery"), Plan::Unknown);
    }

    #[test]
    fn inactive_state_is_not_active() {
        assert!(!LicenseState::inactive("nope").is_active());
    }

    #[test]
    fn active_community_state_has_correct_shape() {
        use std::collections::BTreeSet;
        let mut features = BTreeSet::new();
        features.insert("chat".to_string());
        features.insert("watch_loop".to_string());
        let state = LicenseState::active_community(features.clone());
        assert!(matches!(state.status, LicenseStatus::Active));
        assert!(matches!(state.plan, Plan::Community));
        assert!(matches!(state.subject_kind, SubjectKind::User));
        assert!(matches!(state.binding_mode, BindingMode::Unknown));
        assert!(state.offline_ok);
        assert_eq!(state.features, features);
        assert!(state.is_active(), "Community must be is_active() == true");
    }
}
