//! End-to-end smoke: a fresh process with no LicenseSeat env vars must come
//! up as Community with the correct entitlements.

use spur_license::{Plan, SpurLicense};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn clear_license_env() {
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
