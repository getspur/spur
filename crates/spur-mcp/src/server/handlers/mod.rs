pub(crate) mod code_graph;
pub(crate) mod delegation;
pub(crate) mod graph;
pub(crate) mod plan;
pub(crate) mod plan_execute;
pub(crate) mod pm;

use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
    pub(crate) fn rmcp_tools(&self) -> Vec<Tool> {
        tools::tools_list()
            .into_iter()
            .map(|def| Tool::new(def.name, def.description, rmcp_object(def.input_schema)))
            .collect()
    }

    pub(crate) fn rmcp_tool(&self, name: &str) -> Option<Tool> {
        self.rmcp_tools().into_iter().find(|tool| tool.name == name)
    }

    pub(crate) fn call_tool_result_from_legacy_response(
        response: JsonRpcResponse,
        tool_name: &str,
    ) -> Result<CallToolResult, McpError> {
        match (response.result, response.error) {
            (Some(result), None) => serde_json::from_value(result).map_err(|error| {
                McpError::internal_error(
                    format!("failed to serialize tool result for {tool_name}: {error}"),
                    None,
                )
            }),
            (None, Some(error)) => Err(error.into_mcp_error()),
            (Some(_), Some(_)) | (None, None) => Err(McpError::internal_error(
                format!("tool handler returned an invalid response envelope for {tool_name}"),
                None,
            )),
        }
    }

    // ─── Tool call dispatcher ─────────────────────────────────────────

    pub(crate) async fn handle_tool_call(&self, id: Value, params: Value) -> JsonRpcResponse {
        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing 'name' in params"),
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        debug!(tool = %tool_name, "Handling tool call");

        if tokio::time::timeout(BRAIN_SESSION_BIND_TIMEOUT, self.brain_session_id_ready())
            .await
            .is_err()
        {
            return JsonRpcResponse::internal_error(id, "server not yet bound to brain session");
        }

        match tool_name.as_str() {
            "delegate_to_worker" => self.handle_delegate_to_worker(id, arguments).await,
            "delegate_parallel" => self.handle_delegate_parallel(id, arguments).await,
            "check_delegation_status" => self.handle_check_delegation_status(id, arguments).await,
            "fetch_outcome_artifact" => self.handle_fetch_outcome_artifact(id, arguments).await,
            "cancel_delegation" => self.handle_cancel_delegation(id, arguments).await,
            "list_available_workers" => self.handle_list_available_workers(id).await,
            "get_issue" => self.handle_get_issue(id, arguments).await,
            "list_issues" => self.handle_list_issues(id, arguments).await,
            "update_issue" => self.handle_update_issue(id, arguments).await,
            "report_signal" => {
                if let Some(response) =
                    self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
                {
                    return response;
                }
                let pm = match self.pm_service.clone() {
                    Some(pm) => pm,
                    None => {
                        return JsonRpcResponse::internal_error(id, "No issue tracker configured");
                    }
                };

                let ctx = crate::handlers::WorkerCallContext {
                    delegation_id: String::new(),
                    brain_session_id: self.brain_session_id().as_session_id().0.clone(),
                };
                match crate::handlers::report_signal(
                    pm.as_ref(),
                    self.feature_gate.as_ref(),
                    &ctx,
                    arguments,
                )
                .await
                {
                    Ok(result) => {
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        JsonRpcResponse::success(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )
                    }
                    Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                        JsonRpcResponse::invalid_params(id, e)
                    }
                    Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                        JsonRpcResponse::error(id, -32004, e)
                    }
                    Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                        JsonRpcResponse::error(id, -32001, e)
                    }
                    Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                        JsonRpcResponse::internal_error(id, format!("report_signal failed: {e}"))
                    }
                    Err(crate::handlers::McpHandlerError::Internal(e)) => {
                        JsonRpcResponse::internal_error(id, e)
                    }
                }
            }
            "create_issue" => self.handle_create_issue(id, arguments).await,
            "add_dependency" => self.handle_add_dependency(id, arguments).await,
            "create_pr" => self.handle_create_pr(id, arguments).await,
            "merge_plan" => self.handle_merge_plan(id, arguments).await,
            "resume_plan" => self.handle_resume_plan(id, arguments).await,
            "force_reclaim_plan" => self.handle_force_reclaim_plan(id, arguments).await,
            "graph_triage" => self.handle_graph_triage(id, arguments).await,
            "graph_plan" => self.handle_graph_plan(id, arguments).await,
            "graph_insights" => self.handle_graph_insights(id, arguments).await,
            "graph_alerts" => self.handle_graph_alerts(id, arguments).await,
            "graph_subgraph" => self.handle_graph_subgraph(id, arguments).await,
            "code_resolve" => self.handle_code_resolve(id, arguments).await,
            "code_file_symbols" => self.handle_code_file_symbols(id, arguments).await,
            "code_symbol_info" => self.handle_code_symbol_info(id, arguments).await,
            "code_callers" => self.handle_code_callers(id, arguments).await,
            "code_callees" => self.handle_code_callees(id, arguments).await,
            "code_subgraph" => self.handle_code_subgraph(id, arguments).await,
            "submit_plan" => self.handle_submit_plan(id, arguments).await,
            "execute_epic" => self.handle_execute_epic(id, arguments).await,
            "get_plan_status" => self.handle_get_plan_status(id, arguments).await,
            "get_reconciler_status" => self.handle_get_reconciler_status(id).await,
            "get_task_diff" => self.handle_get_task_diff(id, arguments).await,
            "preview_task_base" => self.handle_preview_task_base(id, arguments).await,
            "plan_truncate_and_restart" => {
                self.handle_plan_truncate_and_restart(id, arguments).await
            }
            "recover_orphaned_dispatch" => {
                self.handle_recover_orphaned_dispatch(id, arguments).await
            }
            "review_task" => {
                if let Some(plan_id) = arguments.get("plan_id").and_then(|v| v.as_str()) {
                    if let Err((code, message)) =
                        self.check_plan_owner_for_op(plan_id, "review_task").await
                    {
                        return JsonRpcResponse::error(id, code, message);
                    }
                }
                match self.handle_review_task(&arguments).await {
                    Ok(text) => JsonRpcResponse::success(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }] }),
                    ),
                    Err(e) => JsonRpcResponse::internal_error(id, e),
                }
            }
            "submit_plan_mutation" => self.handle_submit_plan_mutation(id, arguments).await,
            "report_progress" => self.handle_report_progress(id, arguments).await,
            _ => JsonRpcResponse::error(id, -32601, format!("Unknown tool: {tool_name}")),
        }
    }
}
