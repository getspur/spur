use std::{collections::BTreeMap, path::PathBuf};

use jute::backend::notebook::{Cell, CodeType, NotebookRoot};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Serialize;
use serde_json::{json, Value};
use spur_graph::{extract::GraphFacts, GraphNode, NodeKind, RelationKind};

use crate::{context::refs::Ref, mcp::ServerDeps};

const METHOD: &str = "notebook_symbol_search";

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Search live notebook symbol facts by name/kind. Returns sym:// refs for follow-up with notebook_symbol_refs.",
        rmcp_object(json!({
            "type": "object",
            "properties": {
                "notebook_path": { "type": "string", "description": "Notebook .ipynb path to query; defaults to active notebook when omitted" },
                "query": { "type": "string", "description": "Case-insensitive symbol-name substring" },
                "kind": { "type": "string", "description": "Optional symbol kind filter such as function, class, method" }
            },
            "required": ["query"],
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let args = parse_args(arguments)?;
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_symbol_search requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let path = resolve_notebook_path(deps, args.notebook_path).await?;
    let index = deps.symbol_index.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_symbol_search requires live symbol index",
            Some(json!({ "code": "symbol_index_unavailable" })),
        )
    })?;
    let facts = index.facts_for(&path).ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "notebook_symbol_search has no index facts for {}",
                path.display()
            ),
            Some(json!({ "code": "notebook_not_indexed" })),
        )
    })?;
    let (root, _) = state.notebook_for_path(&path).snapshot();
    let cell_langs = cell_languages(&root);
    let query = args.query.to_ascii_lowercase();
    let kind = args.kind.map(|kind| kind.to_ascii_lowercase());

    let matches = symbol_records(&facts)
        .into_iter()
        .filter(|record| record.name.to_ascii_lowercase().contains(&query))
        .filter(|record| kind.as_ref().is_none_or(|kind| record.kind == *kind))
        .map(|record| SymbolMatch {
            r#ref: Ref::Symbol {
                cell_id: record.cell_id.clone(),
                name: record.name.clone(),
            }
            .to_string(),
            graph_ref: Some(format!("graph://symbol/{}", record.graph_id)),
            name: record.name,
            kind: record.kind,
            lang: cell_langs
                .get(&record.cell_id)
                .copied()
                .unwrap_or("python")
                .to_string(),
            cell: Ref::Cell {
                id: record.cell_id,
                version: None,
            }
            .to_string(),
        })
        .collect::<Vec<_>>();

    let next_queries = matches
        .iter()
        .take(5)
        .map(|entry| {
            json!({
                "tool": "notebook_symbol_refs",
                "notebook_path": path.display().to_string(),
                "ref": entry.r#ref,
                "reason": format!("Inspect references for {}", entry.name),
            })
        })
        .collect::<Vec<_>>();

    Ok(CallToolResult::structured(json!({
        "matches": matches,
        "next_queries": next_queries,
    })))
}

#[derive(Debug)]
struct Args {
    notebook_path: Option<PathBuf>,
    query: String,
    kind: Option<String>,
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
                "notebook_symbol_search notebook_path must be a string",
                Some(json!({ "code": "invalid_notebook_path" })),
            ));
        }
        None => None,
    };
    let query = match map.remove("query") {
        Some(Value::String(value)) if !value.is_empty() => value,
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_symbol_search query must be a non-empty string",
                Some(json!({ "code": "invalid_query" })),
            ));
        }
        None => {
            return Err(McpError::invalid_params(
                "notebook_symbol_search query is required",
                Some(json!({ "code": "missing_query" })),
            ));
        }
    };
    let kind = match map.remove("kind") {
        Some(Value::String(value)) if !value.is_empty() => Some(value),
        Some(_) => {
            return Err(McpError::invalid_params(
                "notebook_symbol_search kind must be a non-empty string",
                Some(json!({ "code": "invalid_kind" })),
            ));
        }
        None => None,
    };
    if !map.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_symbol_search received unknown arguments",
            Some(json!({ "code": "unknown_arguments" })),
        ));
    }
    Ok(Args {
        notebook_path,
        query,
        kind,
    })
}

pub(super) async fn resolve_notebook_path(
    deps: &ServerDeps,
    notebook_path: Option<PathBuf>,
) -> Result<PathBuf, McpError> {
    if let Some(path) = notebook_path {
        return Ok(path);
    }
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            format!("{METHOD} requires notebook daemon control"),
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    daemon.current_path().await.ok_or_else(|| {
        McpError::invalid_params(
            format!("{METHOD} requires notebook_path or an open notebook"),
            Some(json!({ "code": "notebook_not_open" })),
        )
    })
}

#[derive(Debug, Serialize)]
struct SymbolMatch {
    r#ref: String,
    name: String,
    kind: String,
    lang: String,
    cell: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SymbolRecord {
    pub graph_id: String,
    pub name: String,
    pub kind: String,
    pub cell_id: String,
}

pub(super) fn symbol_records(facts: &GraphFacts) -> Vec<SymbolRecord> {
    let node_by_id = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect::<BTreeMap<_, _>>();
    facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Contains)
        .filter_map(|edge| {
            let source = node_by_id.get(&edge.source_node_id)?;
            let target = edge.target_node_id.and_then(|id| node_by_id.get(&id))?;
            if source.kind != NodeKind::Cell || !is_searchable_symbol(target) {
                return None;
            }
            Some(SymbolRecord {
                graph_id: target.stable_key.clone(),
                name: target.label.clone(),
                kind: target.kind.discriminator().to_string(),
                cell_id: source.label.strip_prefix("cell://")?.to_string(),
            })
        })
        .collect()
}

fn is_searchable_symbol(node: &GraphNode) -> bool {
    !matches!(
        node.kind,
        NodeKind::File | NodeKind::Cell | NodeKind::Port | NodeKind::External
    )
}

fn cell_languages(root: &NotebookRoot) -> BTreeMap<String, &'static str> {
    root.cells
        .iter()
        .filter_map(|cell| Some((cell_id(cell)?, cell_language(cell))))
        .collect()
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
    }
}

fn cell_language(cell: &Cell) -> &'static str {
    match cell {
        Cell::Code(cell) => cell
            .metadata
            .spur
            .as_ref()
            .and_then(|spur| spur.code_type)
            .map(code_type_name)
            .unwrap_or("python"),
        Cell::Raw(_) | Cell::Markdown(_) => "python",
    }
}

fn code_type_name(code_type: CodeType) -> &'static str {
    match code_type {
        CodeType::Python => "python",
        CodeType::Javascript => "javascript",
        CodeType::Rust => "rust",
        CodeType::Go => "go",
    }
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

    fn cell(id: &str, source_text: &str) -> Cell {
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
                        source: None,
                    }),
                    code_type: Some(CodeType::Python),
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
    async fn searches_live_index_symbols_for_notebook_path() {
        let temp = TempDir::new().expect("temp dir");
        let root = notebook(vec![cell(
            "load",
            "def load_sales():\n    spur.put(\"sales\", df)\n    return df\n",
        )]);
        let (deps, notebook_path) = deps(root, &temp).await;

        let result = super::call(
            &deps,
            json!({
                "notebook_path": notebook_path,
                "query": "load",
                "kind": "function"
            }),
        )
        .await
        .expect("symbol search succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["matches"][0]["ref"], "sym://load/load_sales");
        assert_eq!(body["matches"][0]["name"], "load_sales");
        assert_eq!(body["matches"][0]["kind"], "function");
        assert_eq!(body["matches"][0]["lang"], "python");
        assert_eq!(body["matches"][0]["cell"], "cell://load");
        assert_eq!(body["next_queries"][0]["tool"], "notebook_symbol_refs");
    }
}
