//! Emission-count audit test. Runs only when live LicenseSeat credentials
//! are present. Records the number of `LicenseEvent`s observed on the
//! facade's subscribe() channel for a single explicit handler cycle.
//!
//! Expected counts after Phase 2 Task 7 (C9 dedup):
//! - activate: 1
//! - validate (ok): 1
//! - heartbeat (ok): 1
//! - deactivate: 1
//!
//! Any count > 1 for a successful explicit handler call indicates the C9
//! duplicate-emission defect is still present.

use std::time::Duration;

use spur_license::SpurLicense;

#[tokio::test]
#[ignore = "requires live LicenseSeat credentials and a test key"]
async fn explicit_handlers_emit_exactly_once() {
    let license = SpurLicense::from_env().expect("env configured");
    let mut rx = license.subscribe();

    // Drain any initial snapshot.
    let _ = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;

    let test_key = std::env::var("SPUR_LICENSESEAT_TEST_KEY")
        .expect("set SPUR_LICENSESEAT_TEST_KEY to a throwaway key");

    let count = count_emissions_during(&mut rx, || async {
        license.activate(&test_key).await.expect("activate");
    })
    .await;
    assert_eq!(count, 1, "activate emitted {count} events, expected 1");

    let count = count_emissions_during(&mut rx, || async {
        license.validate().await.expect("validate");
    })
    .await;
    assert_eq!(count, 1, "validate emitted {count} events, expected 1");

    let count = count_emissions_during(&mut rx, || async {
        license.heartbeat().await.expect("heartbeat");
    })
    .await;
    assert_eq!(count, 1, "heartbeat emitted {count} events, expected 1");

    let count = count_emissions_during(&mut rx, || async {
        license.deactivate().await.expect("deactivate");
    })
    .await;
    assert_eq!(count, 1, "deactivate emitted {count} events, expected 1");
}

// Run `op`, then drain the receiver for a short settling window and count
// every event that arrives. Used to diagnose whether an explicit handler
// produces duplicate emissions (Task 7 regression oracle).
async fn count_emissions_during<F, Fut>(
    rx: &mut tokio::sync::broadcast::Receiver<spur_license::LicenseEvent>,
    op: F,
) -> usize
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    op().await;
    let mut count = 0usize;
    loop {
        match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
            Ok(Ok(_)) => count += 1,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    count
}
