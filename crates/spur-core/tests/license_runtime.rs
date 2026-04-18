use spur_acp::{LicenseStatusEvent, SpurEventBody};
use spur_core::event_funnel::spawn_funnel;
use spur_core::license_runtime::spawn_license_runtime;
use tokio::sync::broadcast;

#[tokio::test]
async fn runtime_emits_initial_license_snapshot() {
    let (tx, mut rx) = broadcast::channel(16);
    let funnel = spawn_funnel(
        tx,
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    );
    let runtime = spawn_license_runtime(spur_license::SpurLicense::from_env_or_disabled(), funnel);

    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for initial license event")
        .expect("broadcast closed before initial event");

    match ev.body {
        SpurEventBody::LicenseUpdated { state } => {
            assert!(matches!(
                state.status,
                LicenseStatusEvent::ConfigError
                    | LicenseStatusEvent::Inactive
                    | LicenseStatusEvent::Active
            ));
            assert!(!state.status_text.is_empty());
        }
        other => panic!("expected LicenseUpdated, got {other:?}"),
    }

    runtime.abort();
}
