use std::{collections::BTreeMap, path::Path};

use jute::{
    backend::notebook::{
        Cell, CellDagMetadata, CodeType, FrontendCellMetadata, NotebookRoot, Output,
    },
    commands::DatasourceEntry,
    state::State,
};
use serde_json::{json, Value};

use crate::{
    context::{catalog::catalog_layer1, refs::Ref},
    dag::{notebook_port_root, NotebookDag, PortStore},
    spur_app::{resolve_app_manifest, SPUR_APP_SCHEMA},
};

const DEFAULT_SKILL_PATH: &str = "skill/SKILL.md";
const CATALOG_NODE_CAP: usize = 12;
const FAILED_CELL_CAP: usize = 5;
const FRONTEND_CELL_CAP: usize = 8;
const ERROR_EXCERPT_CAP: usize = 160;

pub fn build_context_pack(
    state: &State,
    notebook_path: &Path,
    entries: &[DatasourceEntry],
) -> Value {
    let (root, notebook_version) = state.notebook_for_path(notebook_path).snapshot();
    let mut truncated = Vec::new();
    let dag = dag_section(&root, notebook_path, notebook_version, &mut truncated);
    let failed_refs = dag["failed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|failed| failed.get("ref").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    json!({
        "notebook_version": notebook_version,
        "app": app_section(notebook_path, notebook_version).unwrap_or(Value::Null),
        "notebook": notebook_section(&root, notebook_path, notebook_version),
        "catalog": catalog_section(entries, notebook_version, &mut truncated),
        "dag": dag,
        "next_queries": next_queries(failed_refs),
        "truncated": truncated,
    })
}

fn read_skill(app_root: &Path, skill_path: Option<&str>) -> anyhow::Result<Option<String>> {
    use anyhow::Context as _;

    let explicit_skill = skill_path.map(|path| app_root.join(path));
    if let Some(path) = explicit_skill {
        return std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read app skill {}", path.display()))
            .map(Some);
    }

    let default_path = app_root.join(DEFAULT_SKILL_PATH);
    if default_path.is_file() {
        return std::fs::read_to_string(&default_path)
            .with_context(|| format!("failed to read app skill {}", default_path.display()))
            .map(Some);
    }

    Ok(None)
}

fn app_section(notebook_path: &Path, notebook_version: u64) -> Option<Value> {
    let (app_root, manifest, _source) = resolve_app_manifest(notebook_path)?;
    if manifest.schema != SPUR_APP_SCHEMA {
        return None;
    }
    let skill_path = manifest
        .skill
        .clone()
        .unwrap_or_else(|| DEFAULT_SKILL_PATH.to_string());
    let skill = read_skill(&app_root, manifest.skill.as_deref())
        .ok()
        .flatten();

    Some(json!({
        "notebook_version": notebook_version,
        "name": manifest.name,
        "app_key": app_root.display().to_string(),
        "entry_notebook": manifest.entry_notebook,
        "open_mode": manifest.open_mode,
        "runtime_features": manifest.runtime.features,
        "mcp_server": if manifest.mcp_server.is_some() { "present" } else { "none" },
        "skill": skill,
        "skill_path": skill_path,
    }))
}

fn notebook_section(root: &NotebookRoot, notebook_path: &Path, notebook_version: u64) -> Value {
    let mut cells = BTreeMap::<&'static str, usize>::new();
    let mut languages = BTreeMap::<&'static str, usize>::new();

    for cell in &root.cells {
        *cells.entry(cell_kind(cell)).or_default() += 1;
        if matches!(cell, Cell::Code(_)) {
            *languages.entry(cell_language(cell)).or_default() += 1;
        }
    }

    json!({
        "notebook_version": notebook_version,
        "path": notebook_path.display().to_string(),
        "cells": cells,
        "languages": languages,
    })
}

fn catalog_section(
    entries: &[DatasourceEntry],
    notebook_version: u64,
    truncated: &mut Vec<Value>,
) -> Value {
    let nodes = catalog_layer1(entries);
    if nodes.len() > CATALOG_NODE_CAP {
        truncated.push(truncated_marker(
            "catalog",
            nodes.len() - CATALOG_NODE_CAP,
            "notebook_catalog",
        ));
    }

    let summaries = nodes
        .into_iter()
        .take(CATALOG_NODE_CAP)
        .map(|node| {
            json!({
                "ref": node.r#ref,
                "name": node.name,
                "kind": node.kind,
                "node_type": node.node_type,
                "status": node.status,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "notebook_version": notebook_version,
        "count": entries.len(),
        "nodes": summaries,
    })
}

fn dag_section(
    root: &NotebookRoot,
    notebook_path: &Path,
    notebook_version: u64,
    truncated: &mut Vec<Value>,
) -> Value {
    let metadata = dag_metadata(root);
    let (node_count, edge_count) = match NotebookDag::from_metadata(metadata.clone()) {
        Ok(dag) => (metadata.len(), dag.edges().len()),
        Err(_) => (metadata.len(), 0),
    };
    let port_manifest = read_port_manifest(notebook_path);
    let failed = capped_failed_cells(root, truncated);
    let frontend_cells = capped_frontend_cells(root, truncated);

    json!({
        "notebook_version": notebook_version,
        "nodes": node_count,
        "edges": edge_count,
        "failed": failed,
        "stale": [],
        "frontend_cells": frontend_cells,
        "port_manifest": port_manifest,
    })
}

fn dag_metadata(root: &NotebookRoot) -> Vec<(String, CellDagMetadata)> {
    root.cells
        .iter()
        .filter_map(|cell| Some((cell_id(cell)?, cell_dag(cell)?.clone())))
        .collect()
}

fn read_port_manifest(notebook_path: &Path) -> BTreeMap<String, u64> {
    PortStore::open_read_only_at(notebook_port_root(notebook_path))
        .map(|store| {
            store
                .manifest()
                .iter()
                .map(|(port, entry)| (port.clone(), entry.version))
                .collect()
        })
        .unwrap_or_default()
}

fn capped_failed_cells(root: &NotebookRoot, truncated: &mut Vec<Value>) -> Vec<Value> {
    let failed = root
        .cells
        .iter()
        .filter_map(|cell| {
            let error_excerpt = cell_error_excerpt(cell)?;
            Some(json!({
                "ref": cell_ref(cell)?,
                "error_excerpt": error_excerpt,
            }))
        })
        .collect::<Vec<_>>();

    if failed.len() > FAILED_CELL_CAP {
        truncated.push(truncated_marker(
            "dag.failed",
            failed.len() - FAILED_CELL_CAP,
            "notebook_lineage",
        ));
    }
    failed.into_iter().take(FAILED_CELL_CAP).collect()
}

fn capped_frontend_cells(root: &NotebookRoot, truncated: &mut Vec<Value>) -> Vec<Value> {
    let frontend_cells = root
        .cells
        .iter()
        .filter_map(|cell| {
            let frontend = cell_frontend(cell)?;
            Some(json!({
                "ref": cell_ref(cell)?,
                "binds": frontend.binds,
                "emits": frontend.emits,
            }))
        })
        .collect::<Vec<_>>();

    if frontend_cells.len() > FRONTEND_CELL_CAP {
        truncated.push(truncated_marker(
            "dag.frontend_cells",
            frontend_cells.len() - FRONTEND_CELL_CAP,
            "notebook_lineage",
        ));
    }
    frontend_cells.into_iter().take(FRONTEND_CELL_CAP).collect()
}

fn next_queries(failed_refs: Vec<String>) -> Vec<Value> {
    let mut queries = vec![json!({
        "tool": "notebook_catalog",
        "reason": "Explore datasource catalog entries for this notebook",
    })];
    queries.extend(failed_refs.into_iter().map(|cell_ref| {
        json!({
            "tool": "notebook_lineage",
            "ref": cell_ref,
            "reason": "Inspect lineage for failed cell",
        })
    }));
    queries
}

fn truncated_marker(section: &str, dropped: usize, query_deeper_via: &str) -> Value {
    json!({
        "section": section,
        "dropped": dropped,
        "query_deeper_via": query_deeper_via,
    })
}

fn cell_ref(cell: &Cell) -> Option<String> {
    Some(
        Ref::Cell {
            id: cell_id(cell)?,
            version: cell_version(cell),
        }
        .to_string(),
    )
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
    }
}

fn cell_kind(cell: &Cell) -> &'static str {
    match cell {
        Cell::Raw(_) => "raw",
        Cell::Markdown(_) => "markdown",
        Cell::Code(_) => "code",
    }
}

fn cell_version(cell: &Cell) -> Option<u64> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Code(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
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

fn cell_dag(cell: &Cell) -> Option<&CellDagMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
    }
}

fn cell_frontend(cell: &Cell) -> Option<FrontendCellMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.frontend.clone(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.frontend.clone(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.frontend.clone(),
    }
}

fn cell_error_excerpt(cell: &Cell) -> Option<String> {
    let Cell::Code(cell) = cell else {
        return None;
    };
    cell.outputs.iter().find_map(|output| {
        let Output::Error(error) = output else {
            return None;
        };
        Some(truncate_chars(
            &format!("{}: {}", error.ename, error.evalue),
            ERROR_EXCERPT_CAP,
        ))
    })
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, CodeType, FrontendCellMetadata,
            MultilineString, NotebookMetadata, NotebookRoot, Output, OutputError, PortSpec,
            SpurCellMetadata,
        },
        commands::{Column, DatasourceEntry, DatasourceKind},
        state::State,
    };
    use serde_json::{json, Map, Value};

    use super::build_context_pack;

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

    fn code_cell(
        id: &str,
        version: u64,
        code_type: Option<CodeType>,
        produces: Vec<&str>,
        consumes: Vec<&str>,
        outputs: Vec<Output>,
        frontend: Option<FrontendCellMetadata>,
    ) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version,
                    last_edited_by: None,
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
                    code_type,
                    frontend,
                    cron: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count: Some(1),
            outputs,
        })
    }

    fn error_output(ename: &str, evalue: &str) -> Output {
        Output::Error(OutputError {
            ename: ename.to_string(),
            evalue: evalue.to_string(),
            traceback: Vec::new(),
            other: Map::new(),
        })
    }

    fn datasource(name: &str) -> DatasourceEntry {
        DatasourceEntry {
            name: name.to_string(),
            path: format!("/data/{name}.csv"),
            kind: DatasourceKind::Csv,
            group: None,
            columns: vec![Column {
                name: "amount".to_string(),
                sql_type: "DOUBLE".to_string(),
            }],
            row_count: Some(10),
            tables: Vec::new(),
        }
    }

    #[test]
    fn plain_notebook_pack_counts_cells_languages_catalog_and_dag() {
        let temp = tempfile::tempdir().expect("temp dir");
        let notebook_path = temp.path().join("analysis.ipynb");
        let state = State::new();
        let root = notebook(vec![
            code_cell("source", 3, None, vec!["raw"], vec![], Vec::new(), None),
            code_cell(
                "viz",
                5,
                Some(CodeType::Javascript),
                vec![],
                vec!["raw"],
                vec![error_output("KeyError", "'volume'")],
                Some(FrontendCellMetadata {
                    kind: Some("html".to_string()),
                    binds: vec!["raw".to_string()],
                    emits: vec!["horizon".to_string()],
                }),
            ),
        ]);
        state
            .notebook_for_path(&notebook_path)
            .load(&notebook_path, root);
        let entries = (0..14)
            .map(|index| datasource(&format!("table-{index:02}")))
            .collect::<Vec<_>>();

        let pack = build_context_pack(&state, &notebook_path, &entries);

        assert_eq!(pack["app"], Value::Null);
        assert_eq!(pack["notebook"]["notebook_version"], 1);
        assert_eq!(
            pack["notebook"]["path"],
            notebook_path.display().to_string()
        );
        assert_eq!(pack["notebook"]["cells"], json!({ "code": 2 }));
        assert_eq!(
            pack["notebook"]["languages"],
            json!({ "javascript": 1, "python": 1 })
        );
        assert_eq!(pack["catalog"]["count"], 14);
        assert_eq!(pack["catalog"]["nodes"].as_array().unwrap().len(), 12);
        assert_eq!(pack["dag"]["nodes"], 2);
        assert_eq!(pack["dag"]["edges"], 1);
        assert_eq!(
            pack["dag"]["failed"][0],
            json!({ "ref": "cell://viz@v5", "error_excerpt": "KeyError: 'volume'" })
        );
        assert_eq!(
            pack["dag"]["frontend_cells"][0],
            json!({ "ref": "cell://viz@v5", "binds": ["raw"], "emits": ["horizon"] })
        );
        assert!(pack["next_queries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|query| query["tool"] == "notebook_catalog"));
        assert!(pack["next_queries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|query| query["tool"] == "notebook_lineage" && query["ref"] == "cell://viz@v5"));
        assert!(pack["truncated"].as_array().unwrap().iter().any(|marker| {
            marker
                == &json!({
                    "section": "catalog",
                    "dropped": 2,
                    "query_deeper_via": "notebook_catalog"
                })
        }));
    }

    #[test]
    fn app_section_reads_manifest_and_inlines_skill_without_mcp_config() {
        let temp = tempfile::tempdir().expect("temp dir");
        let app_root = temp.path();
        std::fs::write(
            app_root.join("spur-app.json"),
            r#"{
              "schema": "spur.app/v1",
              "name": "Code Graph Workbench",
              "entry_notebook": "app.ipynb",
              "open_mode": "app",
              "runtime": {
                "jute_min": "0.1.0",
                "features": ["frontend-cells", "ports-arrow"]
              },
              "mcp_server": {
                "type": "python",
                "entry": "server/main.py",
                "env": { "SECRET_TOKEN": "do-not-leak" }
              },
              "skill": "skill/SKILL.md"
            }"#,
        )
        .expect("manifest");
        std::fs::create_dir_all(app_root.join("skill")).expect("skill dir");
        std::fs::write(
            app_root.join("skill/SKILL.md"),
            "# Workbench\nUse the app catalog.",
        )
        .expect("skill");
        let notebook_path = app_root.join("app.ipynb");
        let state = State::new();
        state
            .notebook_for_path(&notebook_path)
            .load(&notebook_path, notebook(vec![]));

        let pack = build_context_pack(&state, &notebook_path, &[]);

        assert_eq!(pack["app"]["notebook_version"], 1);
        assert_eq!(pack["app"]["name"], "Code Graph Workbench");
        assert_eq!(pack["app"]["app_key"], app_root.display().to_string());
        assert_eq!(pack["app"]["entry_notebook"], "app.ipynb");
        assert_eq!(pack["app"]["open_mode"], "app");
        assert_eq!(
            pack["app"]["runtime_features"],
            json!(["frontend-cells", "ports-arrow"])
        );
        assert_eq!(pack["app"]["mcp_server"], "present");
        assert_eq!(pack["app"]["skill"], "# Workbench\nUse the app catalog.");
        assert_eq!(pack["app"]["skill_path"], "skill/SKILL.md");
        let app_json = serde_json::to_string(&pack["app"]).expect("serialize app");
        assert!(!app_json.contains("SECRET_TOKEN"));
        assert!(!app_json.contains("server/main.py"));
    }

    #[test]
    fn dag_frontend_cells_are_capped_with_truncation_marker() {
        let temp = tempfile::tempdir().expect("temp dir");
        let notebook_path = temp.path().join("frontends.ipynb");
        let state = State::new();
        let cells = (0..10)
            .map(|index| {
                code_cell(
                    &format!("ui-{index}"),
                    2,
                    None,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Some(FrontendCellMetadata {
                        kind: Some("html".to_string()),
                        binds: vec![format!("input-{index}")],
                        emits: vec![format!("output-{index}")],
                    }),
                )
            })
            .collect();
        state
            .notebook_for_path(&notebook_path)
            .load(&notebook_path, notebook(cells));

        let pack = build_context_pack(&state, &notebook_path, &[]);

        assert_eq!(pack["dag"]["frontend_cells"].as_array().unwrap().len(), 8);
        assert!(pack["truncated"].as_array().unwrap().iter().any(|marker| {
            marker
                == &json!({
                    "section": "dag.frontend_cells",
                    "dropped": 2,
                    "query_deeper_via": "notebook_lineage"
                })
        }));
    }
}
