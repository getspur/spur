use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use ahash::AHashSet;
use arc_swap::ArcSwap;

use crate::install_id::InstallId;
use crate::policy::flags::FlagEvaluator;
use crate::policy::{FeatureKey, FlagKey, FlagSpec, PolicyDocument, PolicyResolver};
use crate::quota::{QuotaKey, QuotaValue};
use crate::snapshot::{EntitlementSnapshot, SourceMetadata};
use crate::tier::Tier;
use crate::{LicenseState, Plan};

#[allow(dead_code)]
pub struct FeatureGate {
    snapshot: ArcSwap<EntitlementSnapshot>,
    policy: Arc<PolicyResolver>,
    install_id: InstallId,
    flag_evaluator: FlagEvaluator,
}

impl FeatureGate {
    pub fn new(policy: Arc<PolicyResolver>) -> Self {
        let install_id = InstallId::load_or_create();
        Self::new_with_install_id(policy, install_id)
    }

    pub fn new_with_install_id(policy: Arc<PolicyResolver>, install_id: InstallId) -> Self {
        let flag_evaluator = FlagEvaluator::new(install_id.clone());
        let snapshot = Self::build_community_snapshot(&policy);
        Self {
            snapshot: ArcSwap::new(Arc::new(snapshot)),
            policy,
            install_id,
            flag_evaluator,
        }
    }

    /// Wait-free feature check.
    pub fn has(&self, feature: FeatureKey) -> bool {
        self.snapshot.load().features.contains(&feature)
    }

    /// Wait-free quota read.
    pub fn quota(&self, key: QuotaKey) -> Option<QuotaValue> {
        self.snapshot.load().quotas.get(&key).copied()
    }

    pub fn tier(&self) -> Tier {
        self.snapshot.load().tier
    }

    pub fn snapshot(&self) -> arc_swap::Guard<Arc<EntitlementSnapshot>> {
        self.snapshot.load()
    }

    pub fn is_flag_enabled(&self, key: FlagKey) -> Option<bool> {
        let snap = self.snapshot.load();
        let flag = snap.flags.get(&key)?;
        Some(self.flag_evaluator.evaluate(key, flag, snap.tier))
    }

    pub fn update_state(&self, state: &LicenseState) {
        let new_snapshot = self.build_snapshot(state);
        self.snapshot.store(Arc::new(new_snapshot));
    }

    /// Replace the active snapshot with a hand-crafted one. Test-only.
    ///
    /// Used by in-process tests that need to simulate a tampered license
    /// state (e.g. an active Pro tier with a single key stripped) without
    /// going through `update_state`, which always resolves the policy's
    /// `@inherit:community` directive and would re-grant the stripped key.
    /// The binary-level analog uses `SPUR_LICENSE_TEST_STRIP_KEYS` (see
    /// `apply_test_strip_keys`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_snapshot_for_test(&self, snapshot: EntitlementSnapshot) {
        self.snapshot.store(Arc::new(snapshot));
    }

    fn build_community_snapshot(policy: &PolicyResolver) -> EntitlementSnapshot {
        let features = apply_test_strip_keys(Self::resolve_feature_keys(policy, "community"));

        let quotas = Self::merge_quotas(Tier::Community, policy.document());
        let flags = Self::extract_flags(policy.document());

        EntitlementSnapshot {
            tier: Tier::Community,
            features,
            quotas,
            flags,
            source: SourceMetadata {
                plan: Plan::Community,
                expires_at: None,
                is_offline: true,
            },
        }
    }

    fn build_snapshot(&self, state: &LicenseState) -> EntitlementSnapshot {
        if !state.is_active() {
            return EntitlementSnapshot::default();
        }

        let tier = Tier::from_plan(state.plan);
        // The signed policy is the canonical source for what each tier
        // grants — it carries the `@inherit:community` directive that lets
        // Pro/Team/Enterprise reuse the Community baseline (cli_core_tui,
        // cli_core_run, …). The license-server JWT only ships tier-specific
        // entitlements, so building a Pro snapshot from JWT alone silently
        // dropped all inherited Community features. Resolve the policy
        // tier first, then union JWT entitlements for forward-compat.
        let mut resolved: AHashSet<FeatureKey> =
            Self::resolve_feature_keys(&self.policy, &tier.label().to_lowercase());
        if tier != Tier::Community {
            resolved.extend(
                state
                    .features
                    .iter()
                    .filter_map(|s| resolve_jwt_feature_key(s)),
            );
        }
        let features = apply_test_strip_keys(resolved);

        let quotas = Self::merge_quotas(tier, self.policy.document());
        let flags = Self::extract_flags(self.policy.document());

        EntitlementSnapshot {
            tier,
            features,
            quotas,
            flags,
            source: SourceMetadata {
                plan: state.plan,
                expires_at: state.expires_at,
                is_offline: state.offline_ok,
            },
        }
    }

    /// Merge quota values for a tier.
    ///
    /// Compatibility defaults apply first as the baseline; policy quotas
    /// overlay on top, overwriting only keys explicitly declared. This
    /// guarantees baseline quotas are always present even with partial
    /// policies.
    fn merge_quotas(tier: Tier, policy_doc: Arc<PolicyDocument>) -> HashMap<QuotaKey, QuotaValue> {
        let mut quotas = HashMap::new();
        Self::apply_compatibility_quota_defaults(tier, &mut quotas);

        let tier_label = tier.label().to_lowercase();
        if let Some(tp) = policy_doc.tier_policies.get(&tier_label) {
            for (key_str, val) in &tp.quotas {
                if let Some(qk) = QuotaKey::from_known(key_str) {
                    if let Some(qv) = parse_quota_value(val) {
                        quotas.insert(qk, qv);
                    }
                }
            }
        }

        quotas
    }

    fn apply_compatibility_quota_defaults(tier: Tier, quotas: &mut HashMap<QuotaKey, QuotaValue>) {
        match tier {
            Tier::Community => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(3));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(128 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Count(1));
            }
            Tier::Pro => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(5));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(1024 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Count(3));
            }
            Tier::Team => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(10));
                quotas.insert(
                    QuotaKey::EventRetentionBytes,
                    QuotaValue::Bytes(10 * 1024 * 1024 * 1024),
                );
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Count(3));
                quotas.insert(QuotaKey::MinSeats, QuotaValue::Count(3));
            }
            Tier::Enterprise => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Unlimited);
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Unlimited);
                quotas.insert(QuotaKey::BrainFailoverChainDepth, QuotaValue::Unlimited);
            }
        }
    }

    fn resolve_feature_keys(policy: &PolicyResolver, tier: &str) -> AHashSet<FeatureKey> {
        let mut features: AHashSet<FeatureKey> = policy
            .tier_features(tier)
            .unwrap_or_else(|err| {
                tracing::warn!("policy tier {tier:?} malformed: {err}; using empty features");
                BTreeSet::new()
            })
            .into_iter()
            .filter_map(|s| FeatureKey::from_known(&s))
            .collect();

        apply_community_compatibility_feature_grants(tier, &mut features);
        features
    }

    fn extract_flags(doc: Arc<PolicyDocument>) -> HashMap<FlagKey, FlagSpec> {
        doc.flags
            .iter()
            .map(|(&key, spec)| (key, spec.clone()))
            .collect()
    }
}

fn apply_community_compatibility_feature_grants(tier: &str, features: &mut AHashSet<FeatureKey>) {
    if tier == "community" {
        // The signed 2026-04-27 policy still models this as Pro, but SPUR's
        // local beads plan substrate is part of the Community daily-driver
        // workflow. Keep the override narrow so unrelated Pro gates stay paid.
        features.insert(FeatureKey::PM_PRO_BEADS_ADVANCED);
    }
}

/// Parse a raw JSON quota value from the signed policy document into the
/// typed `QuotaValue` enum. Returns `None` for unrecognized shapes so the
/// hardcoded default is preserved.
fn parse_quota_value(val: &serde_json::Value) -> Option<QuotaValue> {
    match val {
        serde_json::Value::Number(n) => n.as_u64().map(QuotaValue::Count),
        serde_json::Value::String(s) if s == "unlimited" => Some(QuotaValue::Unlimited),
        serde_json::Value::Object(map) => map
            .get("bytes")
            .and_then(|v| v.as_u64())
            .map(QuotaValue::Bytes)
            .or_else(|| {
                map.get("count")
                    .and_then(|v| v.as_u64())
                    .map(QuotaValue::Count)
            }),
        _ => None,
    }
}

/// Strip the comma-separated feature keys listed in
/// `SPUR_LICENSE_TEST_STRIP_KEYS` from a freshly-resolved feature set.
///
/// **Debug builds only.** The body is `#[cfg(debug_assertions)]`-gated so
/// the env var has zero effect in release binaries — `cargo install spur`
/// users can never accidentally trip this hook.
///
/// Construction-time semantic: read once when the snapshot is built. A
/// mid-process `export SPUR_LICENSE_TEST_STRIP_KEYS=…` does NOT take effect
/// until a new `FeatureGate` is constructed (or `update_state` rebuilds
/// the snapshot, which is the relevant rebuild trigger). Tests that need
/// the strip must set the env var BEFORE spawning the binary.
///
/// Use only as a denial fixture for binary-level e2e tests (Plan C M0.5
/// `cli_core_gate_e2e`). Do not rely on it for product behavior.
fn apply_test_strip_keys(features: AHashSet<FeatureKey>) -> AHashSet<FeatureKey> {
    #[cfg(debug_assertions)]
    {
        if let Ok(csv) = std::env::var("SPUR_LICENSE_TEST_STRIP_KEYS") {
            let mut features = features;
            for raw in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match FeatureKey::from_known(raw) {
                    Some(key) => {
                        features.remove(&key);
                    }
                    None => tracing::debug!(
                        unknown_key = raw,
                        "SPUR_LICENSE_TEST_STRIP_KEYS contains unknown feature key; ignoring"
                    ),
                }
            }
            return features;
        }
    }
    features
}

fn resolve_jwt_feature_key(s: &str) -> Option<FeatureKey> {
    FeatureKey::from_known(s).or_else(|| {
        let mapped = legacy_to_wave9_mapping(s)?;
        tracing::debug!(
            legacy_feature = s,
            mapped_feature = mapped.as_str(),
            "mapped legacy license feature to Wave-9 entitlement"
        );
        Some(mapped)
    })
}

fn legacy_to_wave9_mapping(s: &str) -> Option<FeatureKey> {
    match s {
        "brain_session" => Some(FeatureKey::CORE_CORE_BRAIN_SESSION),
        "single_worker" => None,
        "worktree_isolation" => Some(FeatureKey::WORKTREE_CORE_ISOLATION),
        "manual_review" => Some(FeatureKey::CORE_CORE_REVIEW),
        "event_persistence" => Some(FeatureKey::CORE_CORE_EVENT_PIPELINE),
        "basic_lineage" => Some(FeatureKey::CORE_CORE_EVENT_PIPELINE),
        "tui_dashboard" => Some(FeatureKey::TUI_CORE_VIEW_DASHBOARD),
        "basic_cost_display" => Some(FeatureKey::COST_CORE_SESSION_DISPLAY),
        "basic_notifications" => Some(FeatureKey::CORE_CORE_EVENT_PIPELINE),
        "local_config" => None,
        "mcp_standard_tools" => Some(FeatureKey::MCP_CORE_SERVER_DISPATCH),
        "parallel_workers" => Some(FeatureKey::CORE_CORE_PARALLEL_WORKERS),
        "auto_review_policies" => Some(FeatureKey::CORE_PRO_REVIEW_AUTO_APPROVE),
        "session_resume" => Some(FeatureKey::CORE_CORE_SESSION_RESUME),
        "advanced_cost_analytics" => Some(FeatureKey::COST_PRO_PER_PROJECT_TRACKING),
        "custom_worktree_policies" => None,
        "custom_notifications" => None,
        "extended_retention" => None,
        "tui_session_detail" => Some(FeatureKey::TUI_CORE_VIEW_SESSION_DETAIL),
        "pm_integration" => Some(FeatureKey::PM_CORE_BROWSE),
        "shared_lineage" => None,
        "team_cost_dashboard" => None,
        "centralized_config" => None,
        "rbac" => None,
        "shared_review_queue" => None,
        "pm_webhooks" => None,
        "sso_saml" => None,
        "audit_logs" => None,
        "custom_policies" => None,
        "custom_mcp_tools" => None,
        "dedicated_support" => None,
        "sla_guarantee" => None,
        _ => None,
    }
}

/// Typed error returned by [`require_feature`] when the active
/// license tier does not entitle the requested feature.
///
/// `#[non_exhaustive]` reserves room for future denial-shape variants
/// (e.g. `BootstrapPending` when the gate has not yet received its
/// first snapshot, or future policy-mismatch conditions) without
/// breaking downstream pattern matches. Provider-layer failures like
/// expiry and revocation belong in [`crate::LicenseError`], not here:
/// `require_feature` only knows feature presence in the snapshot,
/// not denial cause.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum FeatureGateError {
    #[error("feature `{}` is not available on tier `{:?}`", key.as_str(), tier)]
    Denied { key: FeatureKey, tier: Tier },
}

/// Workspace-wide gate-check helper. Returns `Ok(())` if the active
/// snapshot grants `key`; otherwise returns
/// [`FeatureGateError::Denied`] tagged with the key and active tier
/// for downstream pattern-matching (e.g. CLI/TUI/MCP recovery copy).
///
/// Per Plan C survey § 8 this is the canonical contract every runtime
/// gate-check site must use.
pub fn require_feature(gate: &FeatureGate, key: FeatureKey) -> Result<(), FeatureGateError> {
    if gate.has(key) {
        Ok(())
    } else {
        Err(FeatureGateError::Denied {
            key,
            tier: gate.tier(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn inactive_license_is_fail_closed() {
        let policy = PolicyResolver::embedded();
        let gate = FeatureGate::new(policy);
        let inactive = LicenseState::inactive("test inactive");
        gate.update_state(&inactive);
        assert!(!gate.has(FeatureKey::CORE_CORE_BRAIN_SESSION));
        assert_eq!(gate.quota(QuotaKey::MaxConcurrentWorkers), None);
    }

    #[test]
    fn tier_transition_updates_atomically() {
        let policy = PolicyResolver::embedded();
        let gate = FeatureGate::new(policy);

        // Start as community
        assert_eq!(gate.tier(), Tier::Community);
        assert!(gate.has(FeatureKey::CORE_CORE_PARALLEL_WORKERS));

        // Update to Pro with a Pro-only feature.
        let mut features = BTreeSet::new();
        features.insert("pm_pro_beads_advanced".to_string());
        let pro_state = LicenseState::active_validated(Plan::Pro, features);
        gate.update_state(&pro_state);

        assert_eq!(gate.tier(), Tier::Pro);
        assert!(gate.has(FeatureKey::PM_PRO_BEADS_ADVANCED));
    }

    #[test]
    fn pro_cached_jwt_legacy_features_map_to_wave9_keys_and_log() {
        use std::fmt::Write as _;
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing_subscriber::layer::{Context, Layer};
        use tracing_subscriber::prelude::*;

        struct DebugCapture(Arc<Mutex<Vec<String>>>);

        impl<S> Layer<S> for DebugCapture
        where
            S: tracing::Subscriber,
        {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                if *event.metadata().level() != tracing::Level::DEBUG {
                    return;
                }

                let mut visitor = FieldVisitor {
                    fields: String::new(),
                };
                event.record(&mut visitor);
                self.0.lock().unwrap().push(visitor.fields);
            }
        }

        struct FieldVisitor {
            fields: String,
        }

        impl Visit for FieldVisitor {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if !self.fields.is_empty() {
                    self.fields.push(' ');
                }
                let _ = write!(self.fields, "{}={value:?}", field.name());
            }
        }

        let logs = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::DEBUG)
            .with(DebugCapture(Arc::clone(&logs)));

        let resolved = tracing::subscriber::with_default(subscriber, || {
            let policy = PolicyResolver::embedded();
            let gate = FeatureGate::new(policy);

            let features = BTreeSet::from([
                "parallel_workers".to_string(),
                "auto_review_policies".to_string(),
                "tui_dashboard".to_string(),
            ]);
            let pro_state = LicenseState::active_validated(Plan::Pro, features);
            gate.update_state(&pro_state);

            (
                gate.has(FeatureKey::CORE_CORE_PARALLEL_WORKERS),
                gate.has(FeatureKey::CORE_PRO_REVIEW_AUTO_APPROVE),
                gate.has(FeatureKey::TUI_CORE_VIEW_DASHBOARD),
            )
        });
        assert!(resolved.0);
        assert!(resolved.1);
        assert!(resolved.2);

        let logs = logs.lock().unwrap();
        assert!(
            logs.iter().any(|event| {
                event.contains("mapped legacy license feature")
                    && event.contains("parallel_workers")
                    && event.contains("core_core_parallel_workers")
            }),
            "expected legacy feature fallback debug log, got {logs:?}"
        );
    }

    #[test]
    fn legacy_mapping_skips_quota_only_and_unmapped_keys() {
        assert_eq!(
            legacy_to_wave9_mapping("brain_session"),
            Some(FeatureKey::CORE_CORE_BRAIN_SESSION)
        );
        assert_eq!(legacy_to_wave9_mapping("single_worker"), None);
        assert_eq!(legacy_to_wave9_mapping("local_config"), None);
    }

    #[test]
    fn parse_quota_value_accepts_unlimited_string() {
        assert_eq!(
            parse_quota_value(&serde_json::json!("unlimited")),
            Some(QuotaValue::Unlimited)
        );
    }
}
