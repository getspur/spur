#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId};
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::{DelegationChannel, McpCallbackServer};

pub fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

pub fn continuation_ctx_arc() -> Arc<DetachedContinuationCtx> {
    Arc::new(continuation_ctx())
}

pub fn community_feature_gate() -> Arc<FeatureGate> {
    Arc::new(FeatureGate::new(PolicyResolver::embedded()))
}

pub fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
}

pub fn mock_community_server() -> McpCallbackServer {
    MockServerBuilder::community().build().0
}

pub fn mock_pro_server() -> McpCallbackServer {
    MockServerBuilder::pro().build().0
}

pub struct MockServerBuilder {
    session_id: BrainSessionId,
    pm_service: Option<Arc<spur_pm::PmService>>,
    feature_gate: Arc<FeatureGate>,
}

impl MockServerBuilder {
    pub fn community() -> Self {
        Self {
            session_id: BrainSessionId::new(SessionId::new()),
            pm_service: None,
            feature_gate: community_feature_gate(),
        }
    }

    pub fn pro() -> Self {
        Self {
            session_id: BrainSessionId::new(SessionId::new()),
            pm_service: None,
            feature_gate: pro_feature_gate(),
        }
    }

    pub fn with_session_id(mut self, session_id: BrainSessionId) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_pm_service(mut self, pm_service: Arc<spur_pm::PmService>) -> Self {
        self.pm_service = Some(pm_service);
        self
    }

    pub fn build(self) -> (McpCallbackServer, DelegationChannel) {
        McpCallbackServer::new(
            Some(&self.session_id),
            self.pm_service,
            None,
            continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            self.feature_gate,
        )
    }
}
