use std::collections::BTreeSet;
use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId};
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::McpCallbackServer;

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn community_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

fn pro_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let mut features = BTreeSet::new();
    features.insert(FeatureKey::MCP_PRO_PLAN_DURABLE.as_str().to_string());
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
}

fn server_with_gate(feature_gate: Arc<FeatureGate>) -> McpCallbackServer {
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        feature_gate,
    );
    server
}

#[test]
fn callback_server_stores_feature_gate_tier_entitlements() {
    let community_server = server_with_gate(community_gate());
    assert!(!community_server
        .feature_gate()
        .has(FeatureKey::MCP_PRO_PLAN_DURABLE));

    let pro_server = server_with_gate(pro_gate());
    assert!(pro_server
        .feature_gate()
        .has(FeatureKey::MCP_PRO_PLAN_DURABLE));
}
