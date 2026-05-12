//! End-to-end smoke: a fresh process with no LicenseSeat env vars must come
//! up as Community with the correct entitlements.

use spur_license::{Plan, SpurLicense};
use std::sync::{Mutex, OnceLock};

static LOCK: Mutex<()> = Mutex::new(());
static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

fn test_home() -> &'static std::path::Path {
    TEST_HOME
        .get_or_init(|| {
            let path = std::env::temp_dir()
                .join(format!("spur-community-smoke-test-{}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create isolated community smoke home");
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

#[test]
fn fresh_process_no_env_vars_is_community() {
    let _guard = LOCK.lock().unwrap();
    clear_license_env();
    let license = SpurLicense::from_env_or_disabled();
    let state = license.current_state();
    assert!(
        matches!(state.plan, Plan::Community),
        "got {:?}",
        state.plan
    );
    assert!(license.has_entitlement("core_core_brain_session"));
    assert!(license.has_entitlement("pm_core_browse"));
    assert!(license.has_entitlement("pm_pro_beads_advanced"));
}

#[test]
fn community_provider_blocks_activate() {
    let _guard = LOCK.lock().unwrap();
    clear_license_env();
    let license = SpurLicense::from_env_or_disabled();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(license.activate("ANY-KEY"));
    assert!(result.is_err(), "Community provider must reject activate");
}
