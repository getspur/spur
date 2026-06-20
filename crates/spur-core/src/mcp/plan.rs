//! Plan / review / reconciler orchestration MCP surface (spur-core owned).
//!
//! Phase 4 of the MCP crate-ownership refactor moves the plan/reconciler engine
//! and its orchestration runtime state out of `spur-mcp` and into `spur-core`.
//! That move is an irreducible, multi-stage relocation (the engine, the plan
//! handlers, the `McpCallbackServer` state fields, the worker read tools, and
//! ~40 integration tests are mutually coupled). See
//! `docs/superpowers/plans/2026-06-21-phase4-plan-reconciler-core-extraction.md`.
//!
//! This module is **Stage 0**: it defines the typed dependency bundle
//! (`PlanMcpDeps`) that captures the orchestration-domain handles off
//! `McpCallbackServer`. It mirrors `DelegationMcpDeps::from_server` and is the
//! concrete input the staged engine migration (Stage 2+) consumes. It adds no
//! behavior and changes no tool dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use spur_blob_store::OutcomeStore as BlobOutcomeStore;
use spur_license::FeatureGate;
use spur_mcp::outcome_materializer::OutcomeMaterializer;
use spur_mcp::plan::outcomes::OutcomeStore;
use spur_mcp::plan::{PlanRegistry, PmLike};
use spur_mcp::server::{CachedPlan, DetachedContinuationCtx};
use spur_mcp::{McpCallbackServer, McpEventSink};
use spur_pm::PmService;

/// Orchestration-domain handles for the plan/review/reconciler MCP tools.
///
/// Bundles the plan/reconciler runtime state currently owned by
/// `McpCallbackServer`. The handles are clone-shared with the server, so the
/// staged migration can move the plan handlers onto this bundle without copying
/// or diverging state.
#[derive(Clone)]
pub struct PlanMcpDeps {
    /// Versioned active-plan cache (`plan_id → CachedPlan`).
    pub active_plans: Arc<tokio::sync::Mutex<HashMap<String, CachedPlan>>>,
    /// `epic_id → plan_id` registry for idempotent execute/resume.
    pub plan_registry: Arc<tokio::sync::Mutex<PlanRegistry>>,
    /// Serializes current-brain plan-ownership claims.
    pub plan_claim_lock: Arc<tokio::sync::Mutex<()>>,
    /// Ephemeral reconciler outcome buffers (MUST NOT be persisted to beads).
    pub reconciler_outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>,
    /// PM service for plan submission/projection.
    pub pm_service: Option<Arc<PmService>>,
    /// `PmLike` substrate handle used by the projector/reconciler.
    pub pm_service_like: Option<Arc<dyn PmLike>>,
    /// Feature gate shared with the license runtime.
    pub feature_gate: Arc<FeatureGate>,
    /// Detached-completion continuation bridge.
    pub continuation_ctx: Arc<DetachedContinuationCtx>,
    /// Outcome materializer for review/reconciler dispatch.
    pub materializer: OutcomeMaterializer,
    /// Blob outcome store backing materialization.
    pub outcome_store: Arc<dyn BlobOutcomeStore>,
    /// Optional MCP lifecycle event sink.
    pub event_sink: Option<Arc<dyn McpEventSink>>,
    /// Repository root for beads-backed plan automation.
    pub repo_root: Option<std::path::PathBuf>,
    /// Persisted-plan versioned-cache serving flag.
    pub versioned_cache_serve: bool,
    /// PR3 non-advisory review-write flag.
    pub nonadvisory_review_writes: bool,
    /// Reconciler-owned dispatch lease duration.
    pub dispatch_lease_duration: Duration,
    /// Opt-in auto-merge/PR on durable epic completion.
    pub auto_merge_approved_plans: bool,
    /// Startup quarantine grace for stale `spur:plan-pending` epics.
    pub plan_pending_grace: Duration,
    /// Whether the beads reconciler is enabled.
    pub reconciler_enabled: bool,
}

impl PlanMcpDeps {
    /// Capture the plan/reconciler orchestration handles off a brain server.
    ///
    /// The handles are `Arc`-shared with the server (see the `ptr_eq` test), so
    /// later stages can route plan handlers through this bundle while the
    /// `McpCallbackServer` still co-owns the same state during the migration.
    pub fn from_server(server: &McpCallbackServer) -> Self {
        Self {
            active_plans: server.active_plans_handle(),
            plan_registry: server.plan_registry_handle(),
            plan_claim_lock: server.plan_claim_lock_handle(),
            reconciler_outcomes: server.reconciler_outcomes_handle(),
            pm_service: server.pm_service_handle(),
            pm_service_like: server.pm_like_handle(),
            feature_gate: server.feature_gate(),
            continuation_ctx: server.continuation_ctx_handle(),
            materializer: server.outcome_materializer(),
            outcome_store: server.outcome_store_handle(),
            event_sink: server.event_sink_handle(),
            repo_root: server.repo_root().map(std::path::Path::to_path_buf),
            versioned_cache_serve: server.versioned_cache_serve(),
            nonadvisory_review_writes: server.nonadvisory_review_writes(),
            dispatch_lease_duration: server.dispatch_lease_duration(),
            auto_merge_approved_plans: server.auto_merge_approved_plans(),
            plan_pending_grace: server.plan_pending_grace(),
            reconciler_enabled: server.reconciler_enabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{BrainSessionId, SessionId};
    use spur_mcp::server::community_feature_gate;

    fn no_op_continuation() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    #[tokio::test]
    async fn from_server_captures_orchestration_state() {
        let session = BrainSessionId::new(SessionId("brain".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&session),
            None,
            None,
            no_op_continuation(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            community_feature_gate(),
        );
        server.set_dispatch_lease_duration(Duration::from_secs(123));
        server.set_versioned_cache_serve(true);
        server.set_nonadvisory_review_writes(true);

        let deps = PlanMcpDeps::from_server(&server);

        // Config snapshot is captured faithfully from the server.
        assert_eq!(deps.dispatch_lease_duration, Duration::from_secs(123));
        assert!(deps.versioned_cache_serve);
        assert!(deps.nonadvisory_review_writes);
        assert!(!deps.reconciler_enabled);
        assert!(!deps.auto_merge_approved_plans);
        assert!(deps.pm_service.is_none());
        assert!(deps.pm_service_like.is_none());

        // Plan-state handles are clone-shared with the server, not fresh copies.
        assert!(
            Arc::ptr_eq(&deps.active_plans, &server.active_plans_handle()),
            "active_plans must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(&deps.plan_registry, &server.plan_registry_handle()),
            "plan_registry must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(
                &deps.reconciler_outcomes,
                &server.reconciler_outcomes_handle()
            ),
            "reconciler_outcomes must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(&deps.plan_claim_lock, &server.plan_claim_lock_handle()),
            "plan_claim_lock must be shared with the server"
        );
    }
}
