use serde_json::{json, Map, Value};

use crate::tools::McpHandlerError;

use super::{validate_project_name, LocalProjectError, LocalProjectResolver, ResolvedLocalProject};

/// Policy controlling whether a graph/analyst module may resolve catalog names.
#[derive(Clone, Default)]
pub enum LocalProjectAccess {
    #[default]
    CurrentWorktreeOnly,
    Catalog(LocalProjectResolver),
}

/// Tool surface available for project-aware follow-up suggestions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LocalProjectFollowupSurface {
    /// Graph and analyst tools are composed into the same server.
    #[default]
    GraphAndAnalyst,
    /// Only analyst query tools are available alongside catalog management.
    AnalystOnly,
}

/// Adds the optional catalog selector to one object input schema.
#[must_use]
pub fn with_optional_project_schema(schema: &Value) -> Value {
    let mut schema = schema.clone();
    if let Some(object) = schema.as_object_mut() {
        let properties = object
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(properties) = properties.as_object_mut() {
            properties.insert(
                "project".to_owned(),
                json!({
                    "type": "string",
                    "description": "Optional registered local project name. Omit to query the active worktree."
                }),
            );
        }
    }
    schema
}

/// Extracts and resolves an explicit selector before existing domain parsing.
pub fn extract_project(
    args: &mut Value,
    access: &LocalProjectAccess,
) -> Result<Option<ResolvedLocalProject>, McpHandlerError> {
    let Some(object) = args.as_object_mut() else {
        return Ok(None);
    };
    let Some(project) = object.remove("project") else {
        return Ok(None);
    };
    let name = project.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams("field `project` must be a string".to_owned())
    })?;
    validate_project_name(name).map_err(project_error_to_handler)?;
    match access {
        LocalProjectAccess::CurrentWorktreeOnly => Err(McpHandlerError::InvalidParams(
            "field `project` is not available on this MCP server".to_owned(),
        )),
        LocalProjectAccess::Catalog(resolver) => resolver
            .resolve(name)
            .map(Some)
            .map_err(project_error_to_handler),
    }
}

/// Adds explicit scope metadata and project-aware follow-up suggestions.
#[must_use]
pub fn decorate_project_response(response: Value, project: Option<&ResolvedLocalProject>) -> Value {
    decorate_project_response_for_surface(
        response,
        project,
        LocalProjectFollowupSurface::GraphAndAnalyst,
    )
}

/// Adds project scope while retaining only follow-ups callable on `surface`.
#[must_use]
pub fn decorate_project_response_for_surface(
    response: Value,
    project: Option<&ResolvedLocalProject>,
    surface: LocalProjectFollowupSurface,
) -> Value {
    let Some(project) = project else {
        return response;
    };
    let mut response = response;
    propagate_project_to_suggestions(&mut response, &project.name, surface);
    let scope = json!({
        "name": project.name,
        "root": project.root,
        "catalog_generation": project.catalog_generation,
    });
    match response {
        Value::Object(ref mut object) => {
            object.insert("project".to_owned(), scope);
            response
        }
        other => json!({"result": other, "project": scope}),
    }
}

fn propagate_project_to_suggestions(
    value: &mut Value,
    project: &str,
    surface: LocalProjectFollowupSurface,
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for key in ["next", "recommended_next_tools"] {
        add_project_to_suggestion_array(object.get_mut(key), project, surface);
    }
    for evidence_key in ["primary_evidence", "supporting_docs"] {
        if let Some(evidence) = object.get_mut(evidence_key).and_then(Value::as_array_mut) {
            for item in evidence {
                if let Some(item) = item.as_object_mut() {
                    for key in ["next", "recommended_next_tools"] {
                        add_project_to_suggestion_array(item.get_mut(key), project, surface);
                    }
                }
            }
        }
    }
}

fn add_project_to_suggestion_array(
    value: Option<&mut Value>,
    project: &str,
    surface: LocalProjectFollowupSurface,
) {
    let Some(entries) = value.and_then(Value::as_array_mut) else {
        return;
    };
    entries.retain(|entry| suggestion_is_callable(entry, surface));
    for entry in entries {
        if let Some(entry) = entry.as_object_mut() {
            entry.insert("project".to_owned(), Value::String(project.to_owned()));
        }
    }
}

fn suggestion_is_callable(entry: &Value, surface: LocalProjectFollowupSurface) -> bool {
    let tool = entry.get("tool").and_then(Value::as_str);
    match surface {
        LocalProjectFollowupSurface::GraphAndAnalyst => tool != Some("code_semantic_search"),
        LocalProjectFollowupSurface::AnalystOnly => matches!(
            tool,
            Some("doc_navigate" | "knowledge_context_pack" | "knowledge_context_pack_2" | "query")
        ),
    }
}

fn project_error_to_handler(error: LocalProjectError) -> McpHandlerError {
    match error.json_rpc_code() {
        -32602 => McpHandlerError::InvalidParams(error.to_string()),
        -32004 => McpHandlerError::NotFound(error.to_string()),
        _ => McpHandlerError::Internal(error.to_string()),
    }
}
