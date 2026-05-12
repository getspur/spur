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

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use spur_license::SpurLicense;

static LOCK: Mutex<()> = Mutex::new(());
static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

fn test_home() -> &'static std::path::Path {
    TEST_HOME
        .get_or_init(|| {
            let path = std::env::temp_dir()
                .join(format!("spur-emission-audit-test-{}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create isolated emission audit home");
            path
        })
        .as_path()
}

fn clear_license_env() {
    std::env::set_var("HOME", test_home());
    std::env::set_var("XDG_CACHE_HOME", test_home().join(".cache"));
    std::env::set_var("XDG_CONFIG_HOME", test_home().join(".config"));
    std::env::set_var("XDG_DATA_HOME", test_home().join(".local/share"));
    std::env::remove_var("SPUR_LICENSE_DEV_PLAN");
    std::env::remove_var("SPUR_LICENSESEAT_API_KEY");
    std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG");
}

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

// Regression baseline for C9. The DisabledProvider has no SDK and no bridge,
// so it must not broadcast on explicit handler calls — if it ever does, the
// event-duplication class of bugs has migrated into the disabled path.
#[tokio::test]
async fn disabled_provider_emits_no_events_on_explicit_calls() {
    let _guard = LOCK.lock().unwrap();
    clear_license_env();
    let license = spur_license::SpurLicense::from_env_or_disabled();
    let mut rx = license.subscribe();

    let _ = license.validate().await;
    let _ = license.heartbeat().await;

    let got = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(
        got.is_err(),
        "DisabledProvider must not broadcast on explicit calls; got {got:?}"
    );
}
