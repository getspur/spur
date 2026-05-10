use spur_license::policy::{PolicyResolver, TierPolicy};
use spur_license::{
    require_feature, EntitlementSnapshot, FeatureGate, FeatureGateError, FeatureKey, FlagKey,
    InstallId, QuotaKey, QuotaValue, Tier,
};

#[test]
fn community_has_core_features() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert!(gate.has(FeatureKey::CORE_CORE_BRAIN_SESSION));
    assert!(gate.has(FeatureKey::CORE_CORE_PARALLEL_WORKERS));
    assert!(gate.has(FeatureKey::PM_PRO_BEADS_ADVANCED));
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
fn community_compat_grants_pm_beads_advanced() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    assert!(gate.has(FeatureKey::PM_PRO_BEADS_ADVANCED));
}

#[test]
fn flag_evaluation_returns_some_for_known_flag() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    // kill_advanced_planner is in default_policy.json flags
    let result = gate.is_flag_enabled(FlagKey::KILL_ADVANCED_PLANNER);
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
    flags.insert(FlagKey::KILL_ADVANCED_PLANNER, disabled);
    let doc = PolicyDocument {
        schema_version: 1,
        issued_at: chrono::Utc::now(),
        policy_version: None,
        expires_at: None,
        tier_policies: BTreeMap::new(),
        v1_1_q3_roadmap: None,
        flags,
    };
    let resolver = PolicyResolver::from_document(doc);
    let gate = FeatureGate::new_with_install_id(resolver, InstallId::from_uuid(uuid::Uuid::nil()));
    assert_eq!(
        gate.is_flag_enabled(FlagKey::KILL_ADVANCED_PLANNER),
        Some(false)
    );
}

fn policy_doc_with_tiers(
    schema_version: u32,
    tier_policies: std::collections::BTreeMap<String, TierPolicy>,
) -> spur_license::policy::PolicyDocument {
    spur_license::policy::PolicyDocument {
        schema_version,
        issued_at: chrono::Utc::now(),
        policy_version: (schema_version >= 2).then(|| "2026-04-27".to_string()),
        expires_at: None,
        tier_policies,
        v1_1_q3_roadmap: None,
        flags: std::collections::BTreeMap::new(),
    }
}

fn tier_policy_with_quotas(
    quotas: std::collections::BTreeMap<String, serde_json::Value>,
) -> TierPolicy {
    TierPolicy {
        features: std::collections::BTreeSet::new(),
        quotas,
        metadata: std::collections::BTreeMap::new(),
    }
}

#[test]
fn v2_policy_quotas_drive_gate_values() {
    let mut tiers = std::collections::BTreeMap::new();
    tiers.insert(
        "pro".into(),
        tier_policy_with_quotas(std::collections::BTreeMap::from([
            (
                "max_concurrent_workers".into(),
                serde_json::json!({"count": 10}),
            ),
            (
                "event_retention_bytes".into(),
                serde_json::json!({"bytes": 2048}),
            ),
        ])),
    );
    let resolver = PolicyResolver::from_document(policy_doc_with_tiers(2, tiers));
    let gate = FeatureGate::new_with_install_id(resolver, InstallId::from_uuid(uuid::Uuid::nil()));

    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        std::collections::BTreeSet::new(),
    ));

    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(10))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(2048))
    );
}

#[test]
fn v2_partial_policy_quotas_keep_compatibility_defaults() {
    let mut tiers = std::collections::BTreeMap::new();
    tiers.insert(
        "pro".into(),
        tier_policy_with_quotas(std::collections::BTreeMap::from([(
            "event_retention_bytes".into(),
            serde_json::json!({"bytes": 2048}),
        )])),
    );
    let resolver = PolicyResolver::from_document(policy_doc_with_tiers(2, tiers));
    let gate = FeatureGate::new_with_install_id(resolver, InstallId::from_uuid(uuid::Uuid::nil()));

    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        std::collections::BTreeSet::new(),
    ));

    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(5))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(2048))
    );
    assert_eq!(
        gate.quota(QuotaKey::BrainFailoverChainDepth),
        Some(QuotaValue::Count(3))
    );
}

#[test]
fn v1_silent_policy_uses_compatibility_quota_defaults() {
    let mut tiers = std::collections::BTreeMap::new();
    tiers.insert(
        "community".into(),
        tier_policy_with_quotas(std::collections::BTreeMap::new()),
    );
    tiers.insert(
        "pro".into(),
        tier_policy_with_quotas(std::collections::BTreeMap::new()),
    );
    let resolver = PolicyResolver::from_document(policy_doc_with_tiers(1, tiers));
    let gate = FeatureGate::new_with_install_id(resolver, InstallId::from_uuid(uuid::Uuid::nil()));

    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(3))
    );

    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        std::collections::BTreeSet::new(),
    ));
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(5))
    );
}

// ----- Plan C M0 (wave C.1) — `require_feature` typed-error contract -----

#[test]
fn require_feature_passes_when_key_present_in_community_tier() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    assert!(require_feature(&gate, FeatureKey::CORE_CORE_BRAIN_SESSION).is_ok());
}

#[test]
fn require_feature_returns_typed_error_with_key_when_absent() {
    // Default snapshot = inactive license = empty feature set, so any
    // require_feature call genuinely denies. (We can't use an
    // `active_validated(Pro, empty)` state here anymore: post-fix, the
    // policy's `@inherit:community` directive grants Community features
    // to Pro automatically, so an empty JWT no longer strips
    // pm_pro_beads_advanced — and even pm_pro_beads_advanced itself is
    // listed in the Pro policy and therefore granted.)
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    gate.set_snapshot_for_test(EntitlementSnapshot::default());

    let err = require_feature(&gate, FeatureKey::PM_PRO_BEADS_ADVANCED)
        .expect_err("default snapshot must reject pm_pro_beads_advanced");
    // `#[non_exhaustive]` makes irrefutable destructuring impossible in
    // external crates; use `let ... else` form.
    let FeatureGateError::Denied { key, .. } = err else {
        panic!("expected Denied, got {err:?}");
    };
    assert_eq!(key, FeatureKey::PM_PRO_BEADS_ADVANCED);
}

#[test]
fn feature_gate_error_display_names_the_key_and_tier() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    gate.set_snapshot_for_test(EntitlementSnapshot::default());

    let err = require_feature(&gate, FeatureKey::CLI_CORE_RUN).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cli_core_run"),
        "error must name the key: {msg}"
    );
    assert!(
        msg.contains("tier"),
        "error must name the tier so callers can render recovery: {msg}"
    );
}
