//! End-to-end smoke: a fresh process with no LicenseSeat env vars must come
//! up as Community with the correct entitlements.

use spur_license::{Plan, SpurLicense};

#[test]
fn fresh_process_no_env_vars_is_community() {
    std::env::remove_var("SPUR_LICENSESEAT_API_KEY");
    std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG");
    let license = SpurLicense::from_env_or_disabled();
    let state = license.current_state();
    assert!(
        matches!(state.plan, Plan::Community),
        "got {:?}",
        state.plan
    );
    assert!(license.has_entitlement("chat"));
    assert!(license.has_entitlement("watch_loop"));
    assert!(!license.has_entitlement("advanced_agents"));
}

#[test]
fn community_provider_blocks_activate() {
    std::env::remove_var("SPUR_LICENSESEAT_API_KEY");
    std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG");
    let license = SpurLicense::from_env_or_disabled();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(license.activate("ANY-KEY"));
    assert!(result.is_err(), "Community provider must reject activate");
}
