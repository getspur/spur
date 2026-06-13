use std::collections::BTreeMap;

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde_json::{json, Value};

use crate::{
    context::{
        lineage::{Direction, LineageGraph, LineageNode, DEFAULT_WALK_DEPTH},
        refs::Ref,
    },
    dag::{notebook_port_root, PortStore},
    mcp::ServerDeps,
};

const METHOD: &str = "notebook_lineage";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Walk DAG lineage from any ds://, cell://, or port:// ref. Returns dataset/job nodes with states and failed-cell error excerpts; edges carry provenance. Start from a failed/stale ref out of notebook_context_pack and walk upstream.",
        rmcp_object(json!({
            "type": "object",
            "properties": {
                "ref": { "type": "string", "description": "Root ds://, cell://, or port:// ref" },
                "direction": { "type": "string", "enum": ["upstream", "downstream", "both"], "default": "both" },
                "depth": { "type": "integer", "default": DEFAULT_WALK_DEPTH, "minimum": 0 }
            },
            "required": ["ref"],
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let args = parse_args(arguments)?;
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_lineage requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_lineage requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let path = daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            "notebook_lineage requires an open notebook",
            Some(json!({ "code": "notebook_not_open" })),
        )
    })?;
    let (root, notebook_version) = state.notebook_for_path(&path).snapshot();
    let entries = state.datasource_catalog.lock().list();
    let port_versions = PortStore::open_read_only_at(notebook_port_root(&path))
        .map_err(|error| {
            McpError::internal_error(
                "notebook_lineage failed to read port manifest",
                Some(json!({ "error": error.to_string() })),
            )
        })?
        .manifest()
        .iter()
        .map(|(port, entry)| (port.clone(), entry.version))
        .collect::<BTreeMap<_, _>>();

    let graph = LineageGraph::build(&root, &entries, &port_versions);
    let walk = graph.walk(&args.r#ref, args.direction, args.depth);
    if walk.nodes.is_empty() {
        return Err(ref_not_found(
            &args.raw_ref,
            nearest_ref(&graph, &args.r#ref),
        ));
    }
    let next_queries = next_queries(&walk.nodes);

    Ok(CallToolResult::structured(json!({
        "notebook_version": notebook_version,
        "root": walk.root,
        "nodes": walk.nodes,
        "edges": walk.edges,
        "truncated": walk.truncated,
        "next_queries": next_queries,
    })))
}

struct Args {
    raw_ref: String,
    r#ref: Ref,
    direction: Direction,
    depth: usize,
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
    let raw_ref = match map.remove("ref") {
        Some(Value::String(value)) if !value.is_empty() => value,
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_lineage ref must be a non-empty string",
                Some(json!({ "code": "invalid_ref" })),
            ));
        }
        None => {
            return Err(McpError::invalid_params(
                "notebook_lineage ref is required",
                Some(json!({ "code": "missing_ref" })),
            ));
        }
    };
    let r#ref = parse_lineage_ref(&raw_ref)?;
    let direction = match map.remove("direction") {
        Some(Value::String(value)) if value == "upstream" => Direction::Upstream,
        Some(Value::String(value)) if value == "downstream" => Direction::Downstream,
        Some(Value::String(value)) if value == "both" => Direction::Both,
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_lineage direction must be \"upstream\", \"downstream\", or \"both\"",
                Some(json!({ "code": "invalid_direction" })),
            ));
        }
        None => Direction::Both,
    };
    let depth = match map.remove("depth") {
        Some(Value::Number(number)) => number.as_u64().ok_or_else(|| {
            McpError::invalid_params(
                "notebook_lineage depth must be a non-negative integer",
                Some(json!({ "code": "invalid_depth" })),
            )
        })? as usize,
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_lineage depth must be a non-negative integer",
                Some(json!({ "code": "invalid_depth" })),
            ));
        }
        None => DEFAULT_WALK_DEPTH,
    };
    if !map.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_lineage received unknown arguments",
            Some(json!({ "code": "unknown_arguments" })),
        ));
    }

    Ok(Args {
        raw_ref,
        r#ref,
        direction,
        depth,
    })
}

fn parse_lineage_ref(raw_ref: &str) -> Result<Ref, McpError> {
    let parsed = Ref::parse(raw_ref).map_err(|error| {
        McpError::invalid_params(
            "notebook_lineage ref must be a valid ds://, cell://, or port:// ref",
            Some(json!({ "code": "invalid_ref", "error": error.to_string() })),
        )
    })?;
    if !matches!(
        parsed,
        Ref::Datasource { .. } | Ref::Cell { .. } | Ref::Port { .. }
    ) {
        return Err(McpError::invalid_params(
            "notebook_lineage ref must use ds://, cell://, or port://",
            Some(json!({ "code": "invalid_ref" })),
        ));
    }
    Ok(parsed)
}

fn ref_not_found(raw_ref: &str, nearest: Option<String>) -> McpError {
    McpError::invalid_params(
        format!("notebook_lineage ref not found: {raw_ref}"),
        Some(json!({ "code": "ref_not_found", "nearest": nearest })),
    )
}

fn nearest_ref(graph: &LineageGraph, reference: &Ref) -> Option<String> {
    let nearest = match reference {
        Ref::Datasource { id, table: Some(_) } => Ref::Datasource {
            id: id.clone(),
            table: None,
        },
        Ref::Cell {
            id,
            version: Some(_),
        } => Ref::Cell {
            id: id.clone(),
            version: None,
        },
        Ref::Port {
            name,
            version: Some(_),
        } => Ref::Port {
            name: name.clone(),
            version: None,
        },
        Ref::Datasource { table: None, .. }
        | Ref::Cell { version: None, .. }
        | Ref::Port { version: None, .. }
        | Ref::Symbol { .. } => {
            return None;
        }
    };
    let walk = graph.walk(&nearest, Direction::Both, 0);
    (!walk.nodes.is_empty()).then_some(walk.root)
}

fn next_queries(nodes: &[LineageNode]) -> Vec<Value> {
    nodes
        .iter()
        .filter_map(|node| {
            if node.r#ref.starts_with("ds://") {
                return Some(json!({
                    "tool": "notebook_catalog",
                    "ref": node.r#ref,
                    "reason": "Inspect datasource catalog details"
                }));
            }
            if node.r#ref.starts_with("cell://") && node.state == "failed" {
                return Some(json!({
                    "tool": "notebook_read_cell",
                    "ref": node.r#ref,
                    "reason": "Read failed cell source and outputs"
                }));
            }
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        state::State,
    };
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use crate::{
        dag::PortStore,
        mcp::{
            bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
            DaemonWindowOps, NotebookDaemonControl, ServerDeps,
        },
    };

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

    fn cell(id: &str, produces: Vec<&str>, consumes: Vec<&str>, source: Option<DagSource>) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version: 7,
                    last_edited_by: Some("brain".to_string()),
                    datasource_setup: None,
                    dag: Some(CellDagMetadata {
                        produces: produces
                            .into_iter()
                            .map(|port| PortSpec {
                                port: port.to_string(),
                                repr: "arrow".to_string(),
                                display: None,
                                class: None,
                                schema: None,
                            })
                            .collect(),
                        consumes: consumes.into_iter().map(str::to_string).collect(),
                        source,
                    }),
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count: Some(3),
            outputs: Vec::new(),
        })
    }

    fn ipc_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "value",
            DataType::Int64,
            false,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1]))]).expect("batch")
    }

    async fn deps(root: NotebookRoot, temp: &TempDir) -> ServerDeps {
        let notebook_path = temp.path().join("nb.ipynb");
        let state = Arc::new(State::new());
        state
            .notebook_for_path(&notebook_path)
            .load(&notebook_path, root);
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
    async fn full_depth_walk_contains_every_dag_status_topology_edge() {
        let temp = TempDir::new().expect("temp dir");
        let root = notebook(vec![
            cell(
                "source",
                vec!["raw"],
                vec![],
                Some(DagSource {
                    kind: "csv".to_string(),
                    port: "sales".to_string(),
                    class: None,
                    schema: None,
                }),
            ),
            cell("middle", vec!["clean"], vec!["raw"], None),
            cell("consumer", vec![], vec!["clean"], None),
        ]);
        let deps = deps(root, &temp).await;
        let port_root = crate::dag::notebook_port_root(temp.path().join("nb.ipynb"));
        let mut store = PortStore::open_at(&port_root).expect("port store");
        store.put("raw", &ipc_batch()).expect("put raw");
        store.put("clean", &ipc_batch()).expect("put clean");

        let status = super::super::notebook_dag_status::call(&deps, json!({}))
            .await
            .expect("status succeeds")
            .structured_content
            .expect("status body");
        let lineage = super::call(
            &deps,
            json!({ "ref": "cell://source", "direction": "downstream", "depth": 99 }),
        )
        .await
        .expect("lineage succeeds")
        .structured_content
        .expect("lineage body");

        for edge in status["edges"].as_array().expect("status edges") {
            let producer = edge["producer"].as_str().expect("producer");
            let consumer = edge["consumer"].as_str().expect("consumer");
            let port = edge["port"].as_str().expect("port");
            let producer_ref = format!("cell://{producer}@v7");
            let consumer_ref = format!("cell://{consumer}@v7");
            let produced_port = lineage_edge_exists(&lineage, &producer_ref, port, "produces");
            let consumed_port = lineage_edge_exists(&lineage, &consumer_ref, port, "consumes");
            assert!(
                produced_port && consumed_port,
                "missing lineage topology for {producer}->{consumer} via {port}: {lineage}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_cell_ref_returns_ref_not_found() {
        let temp = TempDir::new().expect("temp dir");
        let deps = deps(notebook(vec![cell("known", vec![], vec![], None)]), &temp).await;

        let error = super::call(&deps, json!({ "ref": "cell://deleted" }))
            .await
            .expect_err("missing cell should be invalid params");
        let serialized = serde_json::to_value(&error).expect("error serializes");

        assert_eq!(serialized["data"]["code"], "ref_not_found");
        assert_eq!(serialized["data"]["nearest"], Value::Null);
    }

    fn lineage_edge_exists(lineage: &Value, cell_ref: &str, port: &str, via: &str) -> bool {
        lineage["edges"]
            .as_array()
            .expect("lineage edges")
            .iter()
            .any(|edge| {
                edge["via"] == via
                    && (edge["from"] == cell_ref || edge["to"] == cell_ref)
                    && (edge["from"]
                        .as_str()
                        .is_some_and(|value| value.starts_with(&format!("port://{port}")))
                        || edge["to"]
                            .as_str()
                            .is_some_and(|value| value.starts_with(&format!("port://{port}"))))
            })
    }
}
