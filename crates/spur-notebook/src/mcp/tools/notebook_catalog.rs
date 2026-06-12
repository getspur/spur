use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use crate::{
    context::{
        catalog::{catalog_layer1, datasource_id, descend, used_by_map, CatalogNode, UsedBy},
        refs::Ref,
    },
    mcp::ServerDeps,
};

const METHOD: &str = "notebook_catalog";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Navigate the datasource catalog one layer at a time. Layer model: \
         catalog -> connection -> table (file kinds are leaves at layer 2). \
         Omit `ref` for layer 1; pass a `ds://` ref to descend. Use scope=used \
         to see only tables wired into this notebook. Table leaves include \
         column schemas and the invoke syntax for cells. Follow next_queries; \
         for orientation start with notebook_context_pack.",
        rmcp_object(json!({
            "type": "object",
            "properties": {
                "ref": { "type": "string", "description": "ds:// ref to descend into; omit for layer 1" },
                "scope": { "type": "string", "enum": ["all", "used"], "default": "all" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let args = parse_args(arguments)?;
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_catalog requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_catalog requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let path = daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook_catalog requires an open notebook",
            Some(json!({ "code": "notebook_not_open" })),
        )
    })?;
    let (root, notebook_version) = state.notebook_for_path(&path).snapshot();
    let entries = state.datasource_catalog.lock().list();
    let used = used_by_map(&root, &entries);
    let visible_entries = if args.scope == Scope::Used {
        entries
            .iter()
            .filter(|entry| {
                let id = datasource_id(entry, &entries);
                used.get(&id).is_some_and(|used_by| !used_by.is_empty())
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        entries.clone()
    };

    if let Some(raw_ref) = args.r#ref {
        let target = parse_ds_ref(&raw_ref)?;
        let mut node = descend(&visible_entries, &target)
            .ok_or_else(|| ref_not_found(&raw_ref, nearest_ref(&entries, &target)))?;
        attach_used_by(&mut node, &visible_entries, &used);
        let next_queries = next_queries(&node.children);
        let mut body = serde_json::to_value(node).map_err(|error| {
            McpError::internal_error(
                "notebook_catalog failed to serialize catalog node",
                Some(json!({ "error": error.to_string() })),
            )
        })?;
        if let Value::Object(map) = &mut body {
            map.insert("notebook_version".to_string(), json!(notebook_version));
            map.insert("next_queries".to_string(), Value::Array(next_queries));
        }
        return Ok(CallToolResult::structured(body));
    }

    let mut nodes = catalog_layer1(&visible_entries);
    for node in &mut nodes {
        attach_used_by(node, &visible_entries, &used);
    }
    let next_queries = next_queries(&nodes);
    Ok(CallToolResult::structured(json!({
        "notebook_version": notebook_version,
        "nodes": nodes,
        "next_queries": next_queries,
    })))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Used,
}

struct Args {
    r#ref: Option<String>,
    scope: Scope,
}

fn parse_args(arguments: Value) -> Result<Args, McpError> {
    let value = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    let Value::Object(mut map) = value else {
        return Err(McpError::invalid_params(
            format!("{METHOD} arguments must be an object"),
            None,
        ));
    };
    let raw_ref = map.remove("ref").map(parse_optional_string).transpose()?;
    let scope = match map.remove("scope") {
        Some(Value::String(value)) if value == "all" => Scope::All,
        Some(Value::String(value)) if value == "used" => Scope::Used,
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_catalog scope must be \"all\" or \"used\"",
                Some(json!({ "code": "invalid_scope" })),
            ));
        }
        None => Scope::All,
    };
    if !map.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_catalog received unknown arguments",
            Some(json!({ "code": "unknown_arguments" })),
        ));
    }
    Ok(Args {
        r#ref: raw_ref,
        scope,
    })
}

fn parse_optional_string(value: Value) -> Result<String, McpError> {
    match value {
        Value::String(value) => Ok(value),
        _ => Err(McpError::invalid_params(
            "notebook_catalog ref must be a string",
            Some(json!({ "code": "invalid_ref" })),
        )),
    }
}

fn parse_ds_ref(raw_ref: &str) -> Result<Ref, McpError> {
    let parsed = Ref::parse(raw_ref).map_err(|error| {
        McpError::invalid_params(
            "notebook_catalog ref must be a valid ds:// ref",
            Some(json!({ "code": "invalid_ref", "error": error.to_string() })),
        )
    })?;
    if !matches!(parsed, Ref::Datasource { .. }) {
        return Err(McpError::invalid_params(
            "notebook_catalog ref must use the ds:// scheme",
            Some(json!({ "code": "invalid_ref" })),
        ));
    }
    Ok(parsed)
}

fn attach_used_by(
    node: &mut CatalogNode,
    entries: &[jute::commands::DatasourceEntry],
    used: &std::collections::BTreeMap<String, Vec<UsedBy>>,
) {
    if let Some(id) = datasource_id_for_node(&node.r#ref, entries) {
        if let Some(used_by) = used.get(&id) {
            node.used_by = used_by.clone();
        }
    }
    for child in &mut node.children {
        attach_used_by(child, entries, used);
    }
}

fn datasource_id_for_node(
    raw_ref: &str,
    entries: &[jute::commands::DatasourceEntry],
) -> Option<String> {
    let Ref::Datasource { id, .. } = Ref::parse(raw_ref).ok()? else {
        return None;
    };
    entries
        .iter()
        .any(|entry| datasource_id(entry, entries) == id)
        .then_some(id)
}

fn nearest_ref(entries: &[jute::commands::DatasourceEntry], target: &Ref) -> Option<String> {
    let Ref::Datasource { id, table } = target else {
        return None;
    };
    let entry = entries
        .iter()
        .find(|entry| datasource_id(entry, entries) == *id)?;
    if table.is_some() {
        Some(
            Ref::Datasource {
                id: datasource_id(entry, entries),
                table: None,
            }
            .to_string(),
        )
    } else {
        None
    }
}

fn ref_not_found(raw_ref: &str, nearest: Option<String>) -> McpError {
    McpError::invalid_params(
        format!("notebook_catalog ref not found: {raw_ref}"),
        Some(json!({ "code": "ref_not_found", "nearest": nearest })),
    )
}

fn next_queries(children: &[CatalogNode]) -> Vec<Value> {
    children
        .iter()
        .map(|child| {
            json!({
                "tool": METHOD,
                "ref": child.r#ref,
                "reason": format!("Descend into {}", child.name),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        commands::{Column, DatasourceEntry, DatasourceKind, Table},
        state::State,
    };
    use tempfile::TempDir;

    use crate::mcp::{
        bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
        DaemonWindowOps, NotebookDaemonControl,
    };

    use super::*;

    #[derive(Default)]
    struct TestBridge;

    impl BridgeRequester for TestBridge {
        fn listener_registered(&self) -> bool {
            true
        }

        fn window_alive(&self) -> bool {
            true
        }

        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            _method: &'static str,
            _params: Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async { Ok(Value::Null) })
        }
    }

    #[derive(Default)]
    struct TestWindows;

    impl DaemonWindowOps for TestWindows {
        fn show_and_focus(&self, _label: &str) -> bool {
            false
        }

        fn hide(&self, _label: &str) {}

        fn open_notebook_path(&self, _path: &Path) -> Result<String, BridgeError> {
            Ok("test".to_string())
        }

        fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

        fn exit(&self) {}
    }

    fn csv_entry(name: &str, path: &str) -> DatasourceEntry {
        DatasourceEntry {
            name: name.to_string(),
            path: path.to_string(),
            kind: DatasourceKind::Csv,
            group: None,
            columns: vec![Column {
                name: "amount".to_string(),
                sql_type: "DOUBLE".to_string(),
            }],
            row_count: Some(12),
            tables: Vec::new(),
        }
    }

    fn api_entry(name: &str) -> DatasourceEntry {
        DatasourceEntry {
            name: name.to_string(),
            path: format!("api://{name}"),
            kind: DatasourceKind::ApiTables,
            group: None,
            columns: Vec::new(),
            row_count: None,
            tables: vec![Table {
                name: "markets".to_string(),
                columns: vec![Column {
                    name: "market_id".to_string(),
                    sql_type: "TEXT".to_string(),
                }],
                row_count: Some(3200),
            }],
        }
    }

    fn notebook(cells: Vec<Cell>) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        }
    }

    fn cell(id: &str, source_text: &str, source: Option<DagSource>) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version: 7,
                    last_edited_by: Some("brain".to_string()),
                    datasource_setup: None,
                    dag: Some(CellDagMetadata {
                        produces: vec![PortSpec {
                            port: "sales".to_string(),
                            repr: "arrow".to_string(),
                            display: None,
                            class: None,
                            schema: None,
                        }],
                        consumes: Vec::new(),
                        source,
                    }),
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single(source_text.to_string()),
            execution_count: Some(3),
            outputs: Vec::new(),
        })
    }

    async fn deps_with_catalog_and_notebook(temp: &TempDir) -> ServerDeps {
        let notebook_path = temp.path().join("nb.ipynb");
        let state = Arc::new(State::new());
        let entries = vec![csv_entry("sales", "/d/sales.csv"), api_entry("polymarket")];
        state.focus_notebook_path(&notebook_path).load(
            &notebook_path,
            notebook(vec![
                cell(
                    "source",
                    "sales = read_csv()",
                    Some(DagSource {
                        kind: "csv".to_string(),
                        port: "sales".to_string(),
                        class: None,
                        schema: None,
                    }),
                ),
                cell("api", "df = polymarket_markets()", None),
            ]),
        );
        state.attach_datasources(entries);
        let control = NotebookDaemonControl::new_with_parts_for_test(
            Arc::new(AgentBridge::new()),
            Arc::new(TestBridge),
            Arc::clone(&state),
            Arc::new(TestWindows),
            None,
        );
        control.set_current_path_for_test(notebook_path).await;
        ServerDeps {
            bridge: Arc::new(TestBridge),
            state: Some(state),
            app: None,
            daemon: Some(control),
            plugins: None,
        }
    }

    #[tokio::test]
    async fn layer1_then_descend_then_used_scope() {
        let temp = TempDir::new().expect("temp dir");
        let deps = deps_with_catalog_and_notebook(&temp).await;

        let layer1 = super::call(&deps, json!({})).await.unwrap();
        let body = layer1.structured_content.unwrap();
        assert_eq!(body["nodes"].as_array().unwrap().len(), 2);
        assert!(body["notebook_version"].is_number());
        assert!(!body["next_queries"].as_array().unwrap().is_empty());

        let node = super::call(&deps, json!({"ref": body["nodes"][0]["ref"]}))
            .await
            .unwrap();
        assert!(node.structured_content.unwrap()["node_type"].is_string());

        let used = super::call(&deps, json!({"scope": "used"})).await.unwrap();
        assert_eq!(
            used.structured_content.unwrap()["nodes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn unknown_ds_ref_returns_nearest_parent() {
        let temp = TempDir::new().expect("temp dir");
        let deps = deps_with_catalog_and_notebook(&temp).await;

        let error = super::call(&deps, json!({"ref": "ds://polymarket/unknown"}))
            .await
            .expect_err("unknown table ref should fail");
        let serialized = serde_json::to_value(error).expect("error serializes");

        assert_eq!(serialized["data"]["code"], "ref_not_found");
        assert_eq!(serialized["data"]["nearest"], "ds://polymarket");
    }
}
