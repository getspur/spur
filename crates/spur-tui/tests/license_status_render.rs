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
