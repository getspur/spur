use std::collections::BTreeSet;
use std::sync::Arc;

use spur_acp::{
    LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent, LicenseSubjectKind,
    SpurEvent, SpurEventBody,
};
use spur_license::{policy::PolicyResolver, FeatureKey};
use spur_tui::test_support::{feature_enabled, new_app, push_event};

const PRO_COST_TRACKING: FeatureKey = FeatureKey::COST_PRO_PER_PROJECT_TRACKING;

fn tier_features(tier: &str) -> BTreeSet<String> {
    PolicyResolver::embedded()
        .tier_features(tier)
        .unwrap_or_else(|_| panic!("embedded policy must define `{tier}` tier"))
        .into_iter()
        .collect()
}

fn pro_license_state_event() -> LicenseStateEvent {
    LicenseStateEvent {
        status: LicenseStatusEvent::Active,
        subject_kind: LicenseSubjectKind::User,
        plan: LicensePlan::Pro,
        features: tier_features("pro"),
        expires_at: None,
        binding_mode: LicenseBindingMode::NodeLocked,
        offline_ok: true,
        status_text: "Pro license active".into(),
    }
}

fn community_license_state_event() -> LicenseStateEvent {
    LicenseStateEvent {
        status: LicenseStatusEvent::Active,
        subject_kind: LicenseSubjectKind::User,
        plan: LicensePlan::Community,
        features: tier_features("community"),
        expires_at: None,
        binding_mode: LicenseBindingMode::NodeLocked,
        offline_ok: true,
        status_text: "Community license active".into(),
    }
}

#[test]
fn pro_license_update_grants_pro_only_key() {
    let mut app = new_app();

    assert!(
        !feature_enabled(&app, PRO_COST_TRACKING),
        "community baseline must deny Pro-only cost tracking",
    );

    push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::LicenseUpdated {
            state: pro_license_state_event(),
        }),
    );

    assert!(
        feature_enabled(&app, PRO_COST_TRACKING),
        "Pro license update must refresh the App feature gate",
    );
}

#[test]
fn community_license_update_after_pro_re_denies() {
    let mut app = new_app();

    push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::LicenseUpdated {
            state: pro_license_state_event(),
        }),
    );
    assert!(
        feature_enabled(&app, PRO_COST_TRACKING),
        "Pro event should grant Pro-only cost tracking before downgrade",
    );

    push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::LicenseUpdated {
            state: community_license_state_event(),
        }),
    );

    assert!(
        !feature_enabled(&app, PRO_COST_TRACKING),
        "Community downgrade must re-deny Pro-only cost tracking",
    );
}

#[test]
fn pro_seeded_app_grants_pro_key_at_startup() {
    let app = spur_tui::app::App::new_with_license(
        None,
        false,
        Arc::new(spur_acp::SpurConfig::default()),
        pro_license_state_event(),
        spur_tui::landing::LandingDecision::ShowDashboard,
    );

    assert!(
        feature_enabled(&app, PRO_COST_TRACKING),
        "App seeded with Pro state must grant Pro-only cost tracking before any event",
    );
}
