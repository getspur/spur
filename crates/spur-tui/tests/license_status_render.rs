use spur_acp::{
    LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent, LicenseSubjectKind,
    SpurEvent, SpurEventBody,
};

#[test]
fn app_starts_with_community_badge_when_unconfigured() {
    let app = spur_tui::test_support::new_app();
    let badge = app
        .license_badge_for_test()
        .expect("default app should expose a license badge");
    assert_eq!(badge.label, "community");
}

#[test]
fn license_update_event_refreshes_badge_projection() {
    let mut app = spur_tui::test_support::new_app();
    let event = SpurEvent::now(SpurEventBody::LicenseUpdated {
        state: LicenseStateEvent {
            status: LicenseStatusEvent::Degraded,
            subject_kind: LicenseSubjectKind::User,
            plan: LicensePlan::Pro,
            features: ["cloud-sync".to_string()].into_iter().collect(),
            expires_at: None,
            binding_mode: LicenseBindingMode::NodeLocked,
            offline_ok: true,
            status_text: "Offline fallback active".into(),
        },
    });

    spur_tui::test_support::push_event(&mut app, event);

    assert!(matches!(
        app.license_state_for_test().status,
        spur_acp::LicenseStatusEvent::Degraded
    ));
    let badge = app
        .license_badge_for_test()
        .expect("badge should exist after update");
    assert!(badge.label.contains("pro"));
    assert!(badge.label.contains("degraded"));
}

#[test]
fn app_starts_with_active_plan_badge_when_seeded() {
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
    let app = spur_tui::app::App::new_with_license(
        None,
        false,
        cfg,
        state,
        spur_tui::landing::LandingDecision::ShowDashboard,
        None,
    );

    let badge = app
        .license_badge_for_test()
        .expect("badge present on active seed");
    assert!(
        badge.label.contains("pro"),
        "active Pro seed should surface 'pro' label, got {:?}",
        badge.label
    );
}

#[test]
fn active_to_invalid_transition_flips_badge_to_danger_tone() {
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
    let mut app = spur_tui::app::App::new_with_license(
        None,
        false,
        cfg,
        active,
        spur_tui::landing::LandingDecision::ShowDashboard,
        None,
    );

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
    assert_eq!(
        badge.tone,
        LicenseBadgeTone::Danger,
        "Invalid state must render Danger tone, got {:?}",
        badge.tone
    );
    assert!(
        badge.label.contains("invalid"),
        "Invalid badge label should contain 'invalid', got {:?}",
        badge.label
    );
}
