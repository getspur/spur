use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, InstallId, QuotaKey, QuotaValue, Tier};

#[test]
fn community_has_core_features() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert!(gate.has(FeatureKey::BRAIN_SESSION));
    assert!(gate.has(FeatureKey::SINGLE_WORKER));
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
    assert_eq!(gate.tier(), Tier::Community);
}

#[test]
fn community_quota_defaults() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
}

#[test]
fn unknown_feature_returns_false() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
}

#[test]
fn flag_evaluation_returns_some_for_known_flag() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    // kill_advanced_planner is in default_policy.json flags
    let result = gate.is_flag_enabled(FeatureKey::KILL_ADVANCED_PLANNER);
    assert!(result.is_some(), "known flag should evaluate to Some(bool)");
}

#[test]
fn flag_evaluation_respects_kill_switch() {
    use spur_license::policy::{FlagSpec, PolicyDocument};
    use std::collections::BTreeMap;
    let mut flags = BTreeMap::new();
    let disabled = FlagSpec {
        enabled: false,
        ..Default::default()
    };
    flags.insert("kill_advanced_planner".into(), disabled);
    let doc = PolicyDocument {
        schema_version: 1,
        issued_at: chrono::Utc::now(),
        expires_at: None,
        tier_policies: BTreeMap::new(),
        flags,
    };
    let resolver = PolicyResolver::from_document(doc);
    let gate = FeatureGate::new_with_install_id(resolver, InstallId::from_uuid(uuid::Uuid::nil()));
    assert_eq!(
        gate.is_flag_enabled(FeatureKey::KILL_ADVANCED_PLANNER),
        Some(false)
    );
}
