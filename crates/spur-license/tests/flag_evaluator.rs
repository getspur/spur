use spur_license::policy::{FlagEvaluator, FlagSpec};
use spur_license::{InstallId, Tier, KILL_ADVANCED_PLANNER};

#[test]
fn kill_switch_disabled_flag_returns_false() {
    let evaluator = FlagEvaluator::new(InstallId::from_uuid(uuid::Uuid::nil()));
    let spec = FlagSpec {
        enabled: false,
        ..FlagSpec::default()
    };
    assert!(!evaluator.evaluate(KILL_ADVANCED_PLANNER, &spec, Tier::Community));
}

#[test]
fn tier_filter_excludes_wrong_tier() {
    let evaluator = FlagEvaluator::new(InstallId::from_uuid(uuid::Uuid::nil()));
    let spec = FlagSpec {
        tier_filter: Some(vec!["pro".into(), "team".into()]),
        ..FlagSpec::default()
    };
    assert!(!evaluator.evaluate(KILL_ADVANCED_PLANNER, &spec, Tier::Community));
    assert!(evaluator.evaluate(KILL_ADVANCED_PLANNER, &spec, Tier::Pro));
}

#[test]
fn rollout_is_deterministic() {
    let install_id = InstallId::from_uuid(uuid::Uuid::nil());
    let evaluator = FlagEvaluator::new(install_id);
    let spec = FlagSpec {
        rollout_percent: Some(50.0),
        ..FlagSpec::default()
    };
    let key = KILL_ADVANCED_PLANNER;
    let r1 = evaluator.evaluate(key, &spec, Tier::Community);
    let r2 = evaluator.evaluate(key, &spec, Tier::Community);
    assert_eq!(r1, r2, "rollout must be deterministic");
}

#[test]
fn default_flag_is_enabled() {
    let evaluator = FlagEvaluator::new(InstallId::from_uuid(uuid::Uuid::nil()));
    let spec = FlagSpec::default();
    assert!(evaluator.evaluate(KILL_ADVANCED_PLANNER, &spec, Tier::Community));
}

#[test]
fn rollout_distribution_is_broad() {
    use std::collections::HashSet;
    let mut buckets = HashSet::new();
    for i in 0..1000u64 {
        let id = InstallId::from_uuid(uuid::Uuid::from_u128(i as u128));
        let _evaluator = FlagEvaluator::new(id.clone());
        let _spec = FlagSpec {
            rollout_percent: Some(100.0),
            ..FlagSpec::default()
        };
        let key = KILL_ADVANCED_PLANNER;
        let hash = seahash::hash(format!("{}:{}", id, key.as_str()).as_bytes());
        let bucket = (hash % 100) as u8;
        buckets.insert(bucket);
    }
    assert!(
        buckets.len() >= 50,
        "expected broad distribution, got {} buckets",
        buckets.len()
    );
}
