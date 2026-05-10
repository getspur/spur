use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
    pub(crate) async fn handle_get_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };

        match crate::handlers::get_issue(pm, &ctx, args).await {
            Ok(issue) => {
                let text =
                    serde_json::to_string_pretty(&issue).unwrap_or_else(|_| issue.to_string());
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
                JsonRpcResponse::internal_error(id, format!("get_issue failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    pub(crate) async fn handle_list_issues(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let filter = IssueFilter {
            status: args
                .get("status")
                .and_then(|v| v.as_str())
                .map(String::from),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
            priority_min: args
                .get("priority_min")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            priority_max: args
                .get("priority_max")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            issue_type: args
                .get("issue_type")
                .and_then(|v| v.as_str())
                .map(String::from),
            text_search: args
                .get("text_search")
                .and_then(|v| v.as_str())
                .map(String::from),
            limit: Some(
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .min(100) as usize,
            ),
            offset: None,
            labels,
            since: None,
            include_closed: false,
        };

        match pm.list_issues(filter).await {
            Ok(issues) => {
                let text =
                    serde_json::to_string_pretty(&issues).unwrap_or_else(|_| format!("{issues:?}"));
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("list_issues failed: {e}")),
        }
    }

    pub(crate) async fn handle_update_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };

        match crate::handlers::update_issue(pm, &ctx, args).await {
            Ok(result) => {
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
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
                JsonRpcResponse::internal_error(id, format!("update_issue failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    pub(crate) async fn handle_create_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let depends_on: Vec<String> = args
            .get("depends_on")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let params = spur_pm::IssueCreate {
            title,
            description: args
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            issue_type: args.get("type").and_then(|v| v.as_str()).map(String::from),
            priority: args
                .get("priority")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            labels,
            parent: args
                .get("parent")
                .and_then(|v| v.as_str())
                .map(String::from),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
            estimate_minutes: args
                .get("estimate")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            depends_on,
        };

        match pm.create_issue(params).await {
            Ok(issue_id) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Issue created: {issue_id}")
                    }]
                }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_issue failed: {e}")),
        }
    }

    pub(crate) async fn handle_add_dependency(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let issue_id = match args.get("issue_id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(id, "Missing required field 'issue_id'")
            }
        };
        let depends_on_id = match args.get("depends_on_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'depends_on_id'",
                )
            }
        };

        match pm.add_dependency(&issue_id, &depends_on_id).await {
            Ok(()) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Dependency added: {issue_id} depends on {depends_on_id}")
                    }]
                }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("add_dependency failed: {e}")),
        }
    }

    pub(crate) async fn handle_create_pr(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No PR service configured"),
        };
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };
        let body = match args.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'body'"),
        };
        let head_branch = match args.get("branch").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'branch'"),
        };

        let params = PrParams {
            title,
            body,
            head_branch,
            base_branch: args
                .get("base_branch")
                .and_then(|v| v.as_str())
                .map(String::from),
            repo: args.get("repo").and_then(|v| v.as_str()).map(String::from),
        };

        match pm.create_pr(params).await {
            Ok(url) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": format!("PR created: {url}") }] }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_pr failed: {e}")),
        }
    }
}
