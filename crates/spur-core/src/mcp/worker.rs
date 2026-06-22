use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::ErrorData as McpError;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::handlers::{McpHandlerError, PlanResolver, WorkerCallContext};
use crate::outcome_materializer::OutcomeMaterializer;
use crate::plan::outcomes::OutcomeStore as ReconcilerOutcomeStore;
use crate::worker_server::WorkerReadToolSink;
use spur_blob_store::OutcomeStore;
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};

#[derive(Debug, Clone, Copy)]
enum WorkerReadToolSection {
    All,
    Plan,
    Artifact,
}

#[derive(Clone)]
pub struct WorkerReadMcpDeps {
    pub pm_service: Option<Arc<spur_pm::PmService>>,
    pub feature_gate: Arc<spur_license::FeatureGate>,
    pub plan_resolver: Arc<dyn PlanResolver>,
    pub reconciler_outcomes: Arc<Mutex<ReconcilerOutcomeStore>>,
    pub outcome_store: Arc<dyn OutcomeStore>,
    pub repo_root: Option<PathBuf>,
}

impl WorkerReadMcpDeps {
    pub fn catalog_only() -> Self {
        let outcome_store: Arc<dyn OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        Self {
            pm_service: None,
            feature_gate: crate::server::community_feature_gate(),
            plan_resolver: Arc::new(NoopPlanResolver),
            reconciler_outcomes: Arc::new(Mutex::new(ReconcilerOutcomeStore::default())),
            outcome_store,
            repo_root: None,
        }
    }
}

#[derive(Clone)]
pub struct WorkerReadMcpModule {
    deps: WorkerReadMcpDeps,
    materializer: OutcomeMaterializer,
    section: WorkerReadToolSection,
}

impl WorkerReadMcpModule {
    pub fn new(deps: WorkerReadMcpDeps) -> Self {
        Self {
            materializer: OutcomeMaterializer::new(Arc::clone(&deps.outcome_store)),
            deps,
            section: WorkerReadToolSection::All,
        }
    }

    pub(crate) fn plan(deps: WorkerReadMcpDeps) -> Self {
        Self {
            materializer: OutcomeMaterializer::new(Arc::clone(&deps.outcome_store)),
            deps,
            section: WorkerReadToolSection::Plan,
        }
    }

    pub(crate) fn artifact(deps: WorkerReadMcpDeps) -> Self {
        Self {
            materializer: OutcomeMaterializer::new(Arc::clone(&deps.outcome_store)),
            deps,
            section: WorkerReadToolSection::Artifact,
        }
    }

    async fn call_worker_read_tool(
        &self,
        ctx: &WorkerCallContext,
        name: &str,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        match name {
            "get_plan_status" => {
                crate::handlers::get_plan_status(
                    self.deps.plan_resolver.as_ref(),
                    &self.deps.reconciler_outcomes,
                    ctx,
                    args,
                )
                .await
            }
            "get_task_diff" => {
                crate::handlers::get_task_diff(
                    self.deps.pm_service.as_deref(),
                    self.deps.feature_gate.as_ref(),
                    self.deps.repo_root.as_deref(),
                    self.deps.plan_resolver.as_ref(),
                    ctx,
                    args,
                )
                .await
            }
            "fetch_outcome_artifact" => {
                if let Some(requested) = args.get("delegation_id").and_then(|v| v.as_str()) {
                    if requested != ctx.delegation_id {
                        return Err(McpHandlerError::Unauthorized(format!(
                            "delegation_id mismatch for bound session context (expected {}, got {requested})",
                            ctx.delegation_id
                        )));
                    }
                }
                crate::handlers::fetch_outcome_artifact(
                    &self.materializer,
                    self.deps.outcome_store.as_ref(),
                    ctx,
                    args,
                )
                .await
            }
            other => Err(McpHandlerError::InvalidParams(format!(
                "unknown worker read tool: {other}"
            ))),
        }
    }
}

#[async_trait]
impl WorkerReadToolSink for WorkerReadMcpModule {
    async fn get_plan_status(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        self.call_worker_read_tool(ctx, "get_plan_status", args)
            .await
    }

    async fn get_task_diff(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        self.call_worker_read_tool(ctx, "get_task_diff", args).await
    }

    async fn fetch_outcome_artifact(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        self.call_worker_read_tool(ctx, "fetch_outcome_artifact", args)
            .await
    }
}

#[async_trait]
impl ToolModule for WorkerReadMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        match self.section {
            WorkerReadToolSection::All => tool_definitions(),
            WorkerReadToolSection::Plan => plan_tool_definitions(),
            WorkerReadToolSection::Artifact => artifact_tool_definitions(),
        }
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let brain_session_id = ctx
            .brain_session_id
            .map(|id| id.as_session_id().0.clone())
            .unwrap_or_default();
        let worker_ctx = WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id,
        };
        let value = self
            .call_worker_read_tool(&worker_ctx, name, args)
            .await
            .map_err(rmcp::ErrorData::from)?;
        Ok(ToolResponse::json(value))
    }
}

pub(crate) fn tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = plan_tool_definitions();
    definitions.extend(artifact_tool_definitions());
    definitions
}

fn plan_tool_definitions() -> Vec<ToolDefinition> {
    crate::mcp::plan::worker_tool_definitions()
}

fn artifact_tool_definitions() -> Vec<ToolDefinition> {
    vec![crate::mcp::delegation::fetch_outcome_artifact_def()]
}

struct NoopPlanResolver;

#[async_trait]
impl PlanResolver for NoopPlanResolver {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<Mutex<crate::plan::PlanState>>, String> {
        Err(format!("Unknown plan_id: {plan_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn catalog_module_dispatches_missing_args_to_real_handlers() {
        let module = WorkerReadMcpModule::new(WorkerReadMcpDeps::catalog_only());
        let ctx = WorkerCallContext {
            delegation_id: "del-1".into(),
            brain_session_id: "brain-1".into(),
        };

        for tool_name in ["get_plan_status", "get_task_diff", "fetch_outcome_artifact"] {
            let err = module
                .call_worker_read_tool(&ctx, tool_name, json!({}))
                .await
                .expect_err("missing args should be rejected");
            assert!(
                matches!(err, McpHandlerError::InvalidParams(_)),
                "{tool_name} should reject missing args as invalid params, got {err:?}"
            );
        }
    }
}
