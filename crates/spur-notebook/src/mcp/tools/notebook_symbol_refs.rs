use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Serialize;
use serde_json::{json, Value};
use spur_graph::{extract::GraphFacts, GraphEdge, GraphNode, NodeId, NodeKind, RelationKind};

use crate::{
    context::refs::Ref,
    mcp::{tools::notebook_symbol_search, ServerDeps},
};

const METHOD: &str = "notebook_symbol_refs";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Inspect one sym:// ref from the live notebook symbol index. Returns defining cell, related cells, ports touched, and declared-vs-actual port drift.",
        rmcp_object(json!({
            "type": "object",
            "properties": {
                "notebook_path": { "type": "string", "description": "Notebook .ipynb path to query; defaults to active notebook when omitted" },
                "ref": { "type": "string", "description": "sym://<cell-id>/<name> ref returned by notebook_symbol_search" }
            },
            "required": ["ref"],
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let args = parse_args(arguments)?;
    let path = notebook_symbol_search::resolve_notebook_path(deps, args.notebook_path).await?;
    let index = deps.symbol_index.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_symbol_refs requires live symbol index",
            Some(json!({ "code": "symbol_index_unavailable" })),
        )
    })?;
    let facts = index.facts_for(&path).ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "notebook_symbol_refs has no index facts for {}",
                path.display()
            ),
            Some(json!({ "code": "notebook_not_indexed" })),
        )
    })?;

    let Ref::Symbol { cell_id, name } = args.r#ref else {
        return Err(McpError::invalid_params(
            "notebook_symbol_refs ref must use the sym:// scheme",
            Some(json!({ "code": "invalid_ref" })),
        ));
    };
    let symbol = notebook_symbol_search::symbol_records(&facts)
        .into_iter()
        .find(|record| record.cell_id == cell_id && record.name == name)
        .ok_or_else(|| {
            McpError::invalid_params(
                format!("notebook_symbol_refs ref not found: {}", args.raw_ref),
                Some(json!({ "code": "ref_not_found" })),
            )
        })?;
    let refs = symbol_refs(&facts, &symbol);

    Ok(CallToolResult::structured(json!({
        "defined_in": refs.defined_in,
        "used_by": refs.used_by,
        "ports_touched": refs.ports_touched,
        "drift": refs.drift,
        "graph_ref": format!("graph://symbol/{}", symbol.graph_id),
        "next_queries": next_queries(&path, &refs),
    })))
}

struct Args {
    notebook_path: Option<PathBuf>,
    raw_ref: String,
    r#ref: Ref,
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
    let notebook_path = match map.remove("notebook_path") {
        Some(Value::String(value)) => Some(super::validate_notebook_path(METHOD, &value)?),
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_symbol_refs notebook_path must be a string",
                Some(json!({ "code": "invalid_notebook_path" })),
            ));
        }
        None => None,
    };
    let raw_ref = match map.remove("ref") {
        Some(Value::String(value)) if !value.is_empty() => value,
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_symbol_refs ref must be a non-empty string",
                Some(json!({ "code": "invalid_ref" })),
            ));
        }
        None => {
            return Err(McpError::invalid_params(
                "notebook_symbol_refs ref is required",
                Some(json!({ "code": "missing_ref" })),
            ));
        }
    };
    let r#ref = Ref::parse(&raw_ref).map_err(|error| {
        McpError::invalid_params(
            "notebook_symbol_refs ref must be a valid sym:// ref",
            Some(json!({ "code": "invalid_ref", "error": error.to_string() })),
        )
    })?;
    if !map.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_symbol_refs received unknown arguments",
            Some(json!({ "code": "unknown_arguments" })),
        ));
    }
    Ok(Args {
        notebook_path,
        raw_ref,
        r#ref,
    })
}

#[derive(Debug)]
struct SymbolRefs {
    defined_in: String,
    used_by: Vec<String>,
    ports_touched: Vec<String>,
    drift: Vec<Drift>,
}

#[derive(Debug, Serialize)]
struct Drift {
    port: String,
    relation: &'static str,
    direction: &'static str,
}

fn symbol_refs(facts: &GraphFacts, symbol: &notebook_symbol_search::SymbolRecord) -> SymbolRefs {
    let node_by_id = node_by_id(facts);
    let cell = node_by_id
        .values()
        .find(|node| {
            node.kind == NodeKind::Cell
                && node.label
                    == (Ref::Cell {
                        id: symbol.cell_id.clone(),
                        version: None,
                    })
                    .to_string()
        })
        .expect("symbol record should point at an indexed cell");
    let port_edges = facts
        .edges
        .iter()
        .filter(|edge| edge.source_node_id == cell.node_id)
        .filter(|edge| {
            matches!(
                edge.relation,
                RelationKind::Produces | RelationKind::Consumes
            )
        })
        .collect::<Vec<_>>();
    let ports_touched = port_edges
        .iter()
        .filter_map(|edge| edge_target_label(edge, &node_by_id))
        .collect::<BTreeSet<_>>();
    let used_by = cells_touching_ports(facts, &node_by_id, cell.node_id, &ports_touched);
    let drift = drift(&port_edges, &node_by_id);

    SymbolRefs {
        defined_in: cell.label.clone(),
        used_by: used_by.into_iter().collect(),
        ports_touched: ports_touched.into_iter().collect(),
        drift,
    }
}

fn node_by_id(facts: &GraphFacts) -> BTreeMap<NodeId, &GraphNode> {
    facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect()
}

fn cells_touching_ports(
    facts: &GraphFacts,
    node_by_id: &BTreeMap<NodeId, &GraphNode>,
    defined_cell: NodeId,
    ports: &BTreeSet<String>,
) -> BTreeSet<String> {
    facts
        .edges
        .iter()
        .filter(|edge| edge.source_node_id != defined_cell)
        .filter(|edge| {
            matches!(
                edge.relation,
                RelationKind::Produces | RelationKind::Consumes
            )
        })
        .filter(|edge| {
            edge_target_label(edge, node_by_id)
                .as_ref()
                .is_some_and(|label| ports.contains(label))
        })
        .filter_map(|edge| node_by_id.get(&edge.source_node_id))
        .filter(|node| node.kind == NodeKind::Cell)
        .map(|node| node.label.clone())
        .collect()
}

fn drift(edges: &[&GraphEdge], node_by_id: &BTreeMap<NodeId, &GraphNode>) -> Vec<Drift> {
    let mut out = Vec::new();
    for (relation, relation_name) in [
        (RelationKind::Produces, "produces"),
        (RelationKind::Consumes, "consumes"),
    ] {
        let declared = port_set(edges, node_by_id, relation, "declared");
        let actual = port_set(edges, node_by_id, relation, "actual");
        out.extend(declared.difference(&actual).map(|port| Drift {
            port: port.clone(),
            relation: relation_name,
            direction: "declared_without_actual",
        }));
        out.extend(actual.difference(&declared).map(|port| Drift {
            port: port.clone(),
            relation: relation_name,
            direction: "actual_without_declared",
        }));
    }
    out.sort_by(|left, right| {
        left.port
            .cmp(&right.port)
            .then(left.relation.cmp(right.relation))
            .then(left.direction.cmp(right.direction))
    });
    out
}

fn port_set(
    edges: &[&GraphEdge],
    node_by_id: &BTreeMap<NodeId, &GraphNode>,
    relation: RelationKind,
    bind_method: &str,
) -> BTreeSet<String> {
    edges
        .iter()
        .filter(|edge| edge.relation == relation)
        .filter(|edge| edge.bind_method.as_deref() == Some(bind_method))
        .filter_map(|edge| edge_target_label(edge, node_by_id))
        .collect()
}

fn edge_target_label(
    edge: &GraphEdge,
    node_by_id: &BTreeMap<NodeId, &GraphNode>,
) -> Option<String> {
    edge.target_node_id
        .and_then(|id| node_by_id.get(&id))
        .map(|node| node.label.clone())
        .or_else(|| edge.target_label.clone())
}

fn next_queries(path: &std::path::Path, refs: &SymbolRefs) -> Vec<Value> {
    refs.ports_touched
        .iter()
        .take(5)
        .map(|port| {
            json!({
                "tool": "notebook_lineage",
                "notebook_path": path.display().to_string(),
                "ref": port,
                "direction": "both",
                "reason": "Trace cells connected through this port",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, CodeType, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        state::State,
    };
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use crate::{
        context::symbol_index::SymbolIndex,
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

    fn cell(id: &str, source_text: &str, produces: Vec<&str>, consumes: Vec<&str>) -> Cell {
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
                        source: None,
                    }),
                    code_type: Some(CodeType::Python),
                    frontend: None,
                    cron: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single(source_text.to_string()),
            execution_count: Some(3),
            outputs: Vec::new(),
        })
    }

    async fn deps(root: NotebookRoot, temp: &TempDir) -> (ServerDeps, String) {
        let notebook_path = temp.path().join("nb.ipynb");
        let state = Arc::new(State::new());
        state
            .notebook_for_path(&notebook_path)
            .load(&notebook_path, root.clone());
        let index = SymbolIndex::shared();
        index
            .reindex(&notebook_path, &root)
            .expect("index notebook");
        let control = NotebookDaemonControl::new_with_parts_for_test(
            Arc::new(AgentBridge::new()),
            Arc::new(TestBridge),
            Arc::clone(&state),
            Arc::new(TestWindows),
            None,
        );
        control
            .set_current_path_for_test(notebook_path.clone())
            .await;
        (
            ServerDeps {
                bridge: Arc::new(TestBridge),
                state: Some(state),
                app: None,
                daemon: Some(control),
                plugins: None,
                symbol_index: Some(index),
            },
            notebook_path.display().to_string(),
        )
    }

    #[tokio::test]
    async fn returns_symbol_refs_ports_and_declared_actual_drift() {
        let temp = TempDir::new().expect("temp dir");
        let root = notebook(vec![
            cell(
                "load",
                "def load_sales():\n    return df\n",
                vec!["sales"],
                vec![],
            ),
            cell(
                "use",
                "def use_sales():\n    df = spur.get(\"sales\")\n    return df\n",
                vec![],
                vec!["sales"],
            ),
        ]);
        let (deps, notebook_path) = deps(root, &temp).await;

        let result = super::call(
            &deps,
            json!({
                "notebook_path": notebook_path,
                "ref": "sym://load/load_sales"
            }),
        )
        .await
        .expect("symbol refs succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["defined_in"], "cell://load");
        assert_eq!(body["used_by"], json!(["cell://use"]));
        assert_eq!(body["ports_touched"], json!(["port://sales"]));
        assert_eq!(
            body["drift"],
            json!([{
                "port": "port://sales",
                "relation": "produces",
                "direction": "declared_without_actual"
            }])
        );
        assert_eq!(body["next_queries"][0]["tool"], "notebook_lineage");
    }
}
