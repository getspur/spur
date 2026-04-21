//! Runtime state-transition tests backed by FakeProvider. Exercise the three
//! invariants that license_runtime.rs must preserve: transient errors
//! downgrade Active→Degraded with recovery on next success, authoritative
//! revocation propagates to Invalid, and injected autonomous SDK events
//! reach the funnel unchanged.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::LicenseStatusEvent;
use spur_core::event_funnel::spawn_funnel;
use spur_core::license_runtime::spawn_license_runtime;
use spur_license::test_support::FakeProvider;
use spur_license::{
    LicenseError, LicenseEventKind, LicenseState, LicenseStatus, Plan, SpurLicense,
};
use spur_license::policy::PolicyResolver;
use spur_license::FeatureGate;

fn test_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}
use tokio::sync::broadcast;

#[tokio::test]
async fn active_to_degraded_and_back_via_validate() {
    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(FakeProvider::new(seed).with_refresh_policy(
        spur_license::provider::RefreshPolicy {
            validate_interval: Duration::from_millis(40),
            heartbeat_interval: Duration::from_secs(3600),
        },
    ));
    fake.push_validate_result(Err(LicenseError::Provider("network".into())));
    fake.push_validate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));

    let license = SpurLicense::from_provider(fake.clone(), test_feature_gate());
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    let mut statuses = Vec::<LicenseStatusEvent>::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, bcast_rx.recv()).await {
            Ok(Ok(ev)) => {
                if let SpurEventBody::LicenseUpdated { state } = ev.body {
                    statuses.push(state.status);
                    // Once we see Active after Degraded, we have recovery — stop early.
                    if statuses.contains(&LicenseStatusEvent::Degraded)
                        && matches!(state.status, LicenseStatusEvent::Active)
                    {
                        break;
                    }
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
    handle.abort();

    assert!(
        statuses.contains(&LicenseStatusEvent::Active),
        "missing Active in trail: {statuses:?}"
    );
    assert!(
        statuses.contains(&LicenseStatusEvent::Degraded),
        "missing Degraded in trail: {statuses:?}"
    );
    assert_eq!(
        statuses.last().copied(),
        Some(LicenseStatusEvent::Active),
        "trailing status should recover to Active, trail: {statuses:?}"
    );
}

#[tokio::test]
async fn authoritative_invalid_propagates_to_funnel() {
    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(FakeProvider::new(seed).with_refresh_policy(
        spur_license::provider::RefreshPolicy {
            validate_interval: Duration::from_millis(20),
            heartbeat_interval: Duration::from_secs(3600),
        },
    ));
    let mut invalid = LicenseState::inactive("revoked");
    invalid.status = LicenseStatus::Invalid;
    fake.push_validate_result(Ok(invalid));

    let license = SpurLicense::from_provider(fake, test_feature_gate());
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    let mut saw_invalid = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    while tokio::time::Instant::now() < deadline && !saw_invalid {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(40), bcast_rx.recv()).await {
            if let SpurEventBody::LicenseUpdated { state } = ev.body {
                if matches!(state.status, LicenseStatusEvent::Invalid) {
                    saw_invalid = true;
                }
            }
        }
    }
    handle.abort();
    assert!(saw_invalid, "runtime must propagate Invalid to the funnel");
}

#[tokio::test]
async fn injected_subscription_event_reaches_funnel() {
    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(FakeProvider::new(seed));
    let license = SpurLicense::from_provider(fake.clone(), test_feature_gate());
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    // Drain the initial snapshot so we can cleanly assert on the injected one.
    let _ = tokio::time::timeout(Duration::from_millis(80), bcast_rx.recv()).await;

    let mut degraded = LicenseState::active_validated(Plan::Pro, Default::default());
    degraded.status = LicenseStatus::Degraded;
    degraded.status_text = "simulated SDK degrade".into();
    fake.inject_event(LicenseEventKind::ValidationFailed, degraded.clone());

    let mut saw_degraded = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    while tokio::time::Instant::now() < deadline && !saw_degraded {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(40), bcast_rx.recv()).await {
            if let SpurEventBody::LicenseUpdated { state } = ev.body {
                if matches!(state.status, LicenseStatusEvent::Degraded) {
                    saw_degraded = true;
                }
            }
        }
    }
    handle.abort();
    assert!(
        saw_degraded,
        "autonomous inject_event must propagate through the runtime relay"
    );
}

#[tokio::test]
async fn degraded_from_preserves_invalid_status_text() {
    let mut invalid = LicenseState::inactive("revoked");
    invalid.status = LicenseStatus::Invalid;
    let fake = Arc::new(FakeProvider::new(invalid).with_refresh_policy(
        spur_license::provider::RefreshPolicy {
            validate_interval: Duration::from_millis(20),
            heartbeat_interval: Duration::from_secs(3600),
        },
    ));
    fake.push_validate_result(Err(LicenseError::Provider("transient".into())));

    let license = SpurLicense::from_provider(fake, test_feature_gate());
    let (bcast_tx, mut bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    let mut last_text = String::new();
    let mut last_status: Option<LicenseStatusEvent> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if let Ok(Ok(ev)) = tokio::time::timeout(remaining, bcast_rx.recv()).await {
            if let SpurEventBody::LicenseUpdated { state } = ev.body {
                last_text = state.status_text;
                last_status = Some(state.status);
            }
        } else {
            break;
        }
    }
    handle.abort();

    assert_eq!(
        last_status,
        Some(LicenseStatusEvent::Invalid),
        "status must stay Invalid"
    );
    assert_eq!(
        last_text, "revoked",
        "transient validate error must not overwrite prior Invalid text"
    );
}

#[tokio::test]
async fn runtime_skips_heartbeat_when_provider_declines() {
    // Seed an Active NodeLocked state — the SPUR-layer coarse gate would
    // allow heartbeat — but FakeProvider.requires_heartbeat=false should
    // suppress it.
    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(
        FakeProvider::new(seed)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_secs(3600),
                heartbeat_interval: Duration::from_millis(30),
            })
            .with_requires_heartbeat(false),
    );
    let probe = fake.clone();
    let license = SpurLicense::from_provider(fake, test_feature_gate());

    let (bcast_tx, _bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    tokio::time::sleep(Duration::from_millis(250)).await;
    handle.abort();

    assert_eq!(
        probe.heartbeat_call_count(),
        0,
        "runtime must respect provider.requires_heartbeat=false"
    );
}

#[tokio::test]
async fn runtime_heartbeats_when_provider_requires_it() {
    // Symmetric positive case: requires_heartbeat=true AND state bound.
    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(
        FakeProvider::new(seed)
            .with_refresh_policy(spur_license::provider::RefreshPolicy {
                validate_interval: Duration::from_secs(3600),
                heartbeat_interval: Duration::from_millis(30),
            })
            .with_requires_heartbeat(true),
    );
    let probe = fake.clone();
    let license = SpurLicense::from_provider(fake, test_feature_gate());

    let (bcast_tx, _bcast_rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    tokio::time::sleep(Duration::from_millis(250)).await;
    handle.abort();

    assert!(
        probe.heartbeat_call_count() >= 1,
        "runtime must heartbeat when provider.requires_heartbeat=true; \
         got {}",
        probe.heartbeat_call_count(),
    );
}

#[tokio::test(start_paused = true)]
async fn runtime_validates_within_first_minute_after_boot() {
    let seed = LicenseState::active_validated(Plan::Pro, Default::default());
    let fake = Arc::new(FakeProvider::new(seed).with_refresh_policy(
        spur_license::provider::RefreshPolicy {
            validate_interval: Duration::from_secs(3600),
            heartbeat_interval: Duration::from_secs(3600),
        },
    ));
    fake.push_validate_result(Ok(LicenseState::active_validated(
        Plan::Pro,
        Default::default(),
    )));

    let license = SpurLicense::from_provider(fake.clone(), test_feature_gate());
    let (bcast_tx, _rx) = broadcast::channel::<SpurEvent>(64);
    let funnel = spawn_funnel(bcast_tx, Arc::new(AtomicU64::new(0)));
    let handle = spawn_license_runtime(license, funnel);

    // Yield to let the spawned runtime task start and register its timers.
    tokio::task::yield_now().await;

    // Advance virtual time past the 30s initial clamp (but well under the
    // configured 3600s interval). The runtime's validate arm must fire.
    tokio::time::advance(Duration::from_secs(60)).await;
    // Give the runtime task multiple yield points to drive the select arm
    // and complete the validate() call.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    handle.abort();

    assert_eq!(
        fake.validate_call_count(),
        1,
        "runtime must perform an initial validate shortly after startup \
         even when validate_interval is an hour"
    );
}
