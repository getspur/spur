use std::sync::Arc;

use rmcp::ErrorData as McpError;
use serde_json::json;
use spur_license::FeatureKey;

pub const MCP_NOT_LICENSED_ERROR_CODE: i32 = -32041;

pub fn require_feature(
    key: FeatureKey,
    feature_gate: &spur_license::FeatureGate,
) -> Result<(), McpError> {
    if feature_gate.has(key) {
        return Ok(());
    }

    Err(McpError::new(
        rmcp::model::ErrorCode(MCP_NOT_LICENSED_ERROR_CODE),
        format!("not licensed for feature {}", key.as_str()),
        Some(json!({
            "reason": "not_licensed",
            "feature": key.as_str(),
            "required_tier": "pro"
        })),
    ))
}

pub fn feature_error_message(error: McpError) -> String {
    error.message.into_owned()
}

/// Build the embedded Community-tier feature gate used by fallback and tests.
pub fn community_feature_gate() -> Arc<spur_license::FeatureGate> {
    Arc::new(spur_license::FeatureGate::new(
        spur_license::policy::PolicyResolver::embedded(),
    ))
}

#[cfg(any(test, feature = "test-support"))]
pub fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
    let gate = Arc::new(spur_license::FeatureGate::new(
        spur_license::policy::PolicyResolver::embedded(),
    ));
    let features =
        std::collections::BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        features,
    ));
    gate
}

#[cfg(any(test, feature = "test-support"))]
pub fn unlicensed_feature_gate() -> Arc<spur_license::FeatureGate> {
    let gate = community_feature_gate();
    let mut snapshot = (**gate.snapshot()).clone();
    snapshot
        .features
        .remove(&spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED);
    gate.set_snapshot_for_test(snapshot);
    gate
}
