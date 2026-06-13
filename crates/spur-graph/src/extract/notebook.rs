use std::path::Path;
use std::str;

use anyhow::Context as _;
use serde_json::Value;
use tree_sitter::{Parser, Point, Range};

use crate::extract::languages::Language;
use crate::extract::markdown::extract_markdown_contents;
use crate::extract::tree_sitter::{
    compile_queries, extract_file_contents_from_tree, relative_path, FactBuilder,
};
use crate::{FileId, NodeId, NodeKind, RelationKind};

fn language_for_token(token: &str) -> Option<Language> {
    match token.to_ascii_lowercase().as_str() {
        "python" | "python3" => Some(Language::Python),
        "javascript" => Some(Language::Javascript),
        "rust" | "evcxr" => Some(Language::Rust),
        _ => None,
    }
}

pub(crate) fn cell_is_markdown(cell: &Value) -> bool {
    cell.get("cell_type").and_then(Value::as_str) == Some("markdown")
}

pub(crate) fn resolve_cell_language(cell: &Value, root: &Value) -> Option<Language> {
    let cell_meta = cell.get("metadata");
    let candidates = [
        cell_meta
            .and_then(|metadata| metadata.get("spur"))
            .and_then(|spur| spur.get("code_type"))
            .and_then(Value::as_str),
        cell_meta
            .and_then(|metadata| metadata.get("kernelspec"))
            .and_then(|kernelspec| kernelspec.get("name"))
            .and_then(Value::as_str),
        root.get("metadata")
            .and_then(|metadata| metadata.get("kernelspec"))
            .and_then(|kernelspec| kernelspec.get("name"))
            .and_then(Value::as_str),
        root.get("metadata")
            .and_then(|metadata| metadata.get("language_info"))
            .and_then(|language_info| language_info.get("name"))
            .and_then(Value::as_str),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(language_for_token)
}

pub(crate) fn extract_notebook_file(
    builder: &mut FactBuilder<'_>,
    path: &Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let notebook: Value = serde_json::from_slice(bytes).context("parse .ipynb JSON")?;
    let source = str::from_utf8(bytes).context("read .ipynb as UTF-8")?;
    let relative_path = relative_path(builder.root(), path)?;
    let file_id = FileId(builder.next_file_id());
    let root_tree = parse_source(Language::JupyterNotebook, source)?;
    let file_node = builder.add_file_node(&relative_path, file_id, root_tree.root_node());

    let Some(cells) = notebook.get("cells").and_then(Value::as_array) else {
        return Ok(());
    };

    for (idx, cell) in cells.iter().enumerate() {
        let cell_source = cell_source_text(cell);
        let cell_id = cell_id(cell, idx);
        let cell_node = add_cell_node(
            builder,
            &relative_path,
            file_id,
            file_node,
            &cell_id,
            &cell_source,
        );
        if cell_is_markdown(cell) {
            extract_cell(
                builder,
                &relative_path,
                file_id,
                cell_node,
                Language::Markdown,
                &cell_source,
            )?;
            continue;
        }

        if cell.get("cell_type").and_then(Value::as_str) != Some("code") {
            continue;
        }

        match resolve_cell_language(cell, &notebook) {
            Some(language) => {
                extract_cell(
                    builder,
                    &relative_path,
                    file_id,
                    cell_node,
                    language,
                    &cell_source,
                )?;
            }
            None => {
                tracing::warn!(
                    path = %path.display(),
                    "spur-graph: skipping notebook cell (unsupported language)"
                );
            }
        }
    }

    Ok(())
}

fn cell_id(cell: &Value, idx: usize) -> String {
    cell.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        // nbformat 4.5 requires cell ids, but older notebooks may not have them.
        .unwrap_or_else(|| format!("cell-{idx}"))
}

fn add_cell_node(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node: NodeId,
    cell_id: &str,
    source: &str,
) -> NodeId {
    let cell_label = format!("cell://{cell_id}");
    let cell_node = builder.add_node_with_range(
        relative_path,
        cell_label.clone(),
        cell_label,
        NodeKind::Cell,
        file_id,
        source_range(source),
    );
    builder.add_edge(file_node, Some(cell_node), RelationKind::Contains, None);
    cell_node
}

fn source_range(source: &str) -> Range {
    Range {
        start_byte: 0,
        end_byte: source.len(),
        start_point: Point { row: 0, column: 0 },
        end_point: end_point(source),
    }
}

fn end_point(source: &str) -> Point {
    let mut row = 0;
    let mut column = 0;
    for byte in source.bytes() {
        if byte == b'\n' {
            row += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    Point { row, column }
}

fn cell_source_text(cell: &Value) -> String {
    match cell.get("source") {
        Some(Value::String(source)) => source.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn extract_cell(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    parent_node: NodeId,
    language: Language,
    source: &str,
) -> anyhow::Result<()> {
    let config = language.config();
    let queries = compile_queries(&config, language)?;
    let tree = parse_source(language, source)?;

    if language == Language::Markdown {
        let mut inline_parser = markdown_inline_parser(&config)?;
        return extract_markdown_contents(
            builder,
            &config,
            relative_path,
            file_id,
            parent_node,
            source,
            tree.root_node(),
            &queries,
            inline_parser.as_mut(),
        );
    }

    extract_file_contents_from_tree(
        builder,
        language.label(),
        &config,
        relative_path,
        file_id,
        parent_node,
        source,
        tree.root_node(),
        &queries,
    )
}

fn parse_source(language: Language, source: &str) -> anyhow::Result<tree_sitter::Tree> {
    let mut parser = Parser::new();
    let ts_language = language.tree_sitter_language();
    parser
        .set_language(&ts_language)
        .with_context(|| format!("set {} parser language", language.label()))?;
    parser
        .parse(source.as_bytes(), None)
        .with_context(|| format!("parse {} notebook cell", language.label()))
}

fn markdown_inline_parser(
    config: &crate::extract::languages::LanguageConfig,
) -> anyhow::Result<Option<Parser>> {
    let Some(inline_language) = config.inline_language.as_ref() else {
        return Ok(None);
    };
    let mut parser = Parser::new();
    parser
        .set_language(inline_language)
        .context("set markdown inline parser language")?;
    Ok(Some(parser))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::extract::tree_sitter::FactBuilder;
    use crate::extract::GraphFacts;
    use crate::{NodeKind, RelationKind};

    #[test]
    fn code_type_takes_precedence() {
        let cell = json!({"cell_type":"code","metadata":{"spur":{"code_type":"javascript"}}});
        let root = json!({"metadata":{"kernelspec":{"name":"python3"}}});

        assert_eq!(
            resolve_cell_language(&cell, &root),
            Some(Language::Javascript)
        );
    }

    #[test]
    fn falls_back_to_notebook_kernelspec_then_language_info() {
        let cell = json!({"cell_type":"code","metadata":{}});
        let root = json!({"metadata":{"kernelspec":{"name":"python3"}}});
        assert_eq!(resolve_cell_language(&cell, &root), Some(Language::Python));

        let root2 = json!({"metadata":{"language_info":{"name":"rust"}}});
        assert_eq!(resolve_cell_language(&cell, &root2), Some(Language::Rust));
    }

    #[test]
    fn unknown_and_unsupported_languages_resolve_to_none() {
        let root = json!({"metadata":{}});
        for kernel in ["go", "gonb", "julia", "r", "ir", "haskell"] {
            let cell = json!({"cell_type":"code","metadata":{"spur":{"code_type":kernel}}});
            assert_eq!(resolve_cell_language(&cell, &root), None, "{kernel}");
        }
    }

    #[test]
    fn detects_markdown_cells() {
        assert!(cell_is_markdown(&json!({"cell_type":"markdown"})));
        assert!(!cell_is_markdown(&json!({"cell_type":"code"})));
    }

    #[test]
    fn extracts_python_def_and_markdown_heading_as_transitively_contained_children() {
        let nb = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {"kernelspec": {"name": "python3"}},
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["def load_df():\n", "    return 1\n"]
                },
                {
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["# Analysis\n"]
                }
            ]
        });
        let bytes = serde_json::to_vec(&nb).expect("serialize notebook");

        let facts =
            run_notebook_extraction(Path::new("nb.ipynb"), &bytes).expect("notebook extracts");

        assert!(has_symbol(&facts, "load_df"));
        assert!(has_section(&facts, "Analysis"));
        assert!(all_non_file_nodes_reachable_from_file_via_contains(
            &facts, "nb.ipynb"
        ));
        assert!(all_symbols_contained_by_cells(&facts));
        assert!(!file_directly_contains_symbols(&facts, "nb.ipynb"));
    }

    #[test]
    fn each_cell_gets_a_cell_container_node() {
        let nb = serde_json::to_vec(&serde_json::json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [
                {"cell_type":"code","id":"a3f1","source":["def load():\n"," pass\n"],"metadata":{},"outputs":[],"execution_count":null},
                {"cell_type":"code","id":"b2c9","source":["x = load()\n"],"metadata":{},"outputs":[],"execution_count":null}
            ]
        }))
        .unwrap();
        let mut builder = FactBuilder::new(Path::new("/nb"));
        extract_notebook_file(&mut builder, Path::new("/nb/app.ipynb"), &nb).unwrap();
        let facts = builder.into_facts();
        let cell_nodes: Vec<_> = facts
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Cell)
            .collect();
        assert_eq!(cell_nodes.len(), 2);
        let labels: Vec<&str> = cell_nodes.iter().map(|node| node.label.as_str()).collect();
        assert!(labels.contains(&"cell://a3f1"));
        assert!(labels.contains(&"cell://b2c9"));
    }

    #[test]
    fn skips_unknown_language_cells() {
        let nb = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {"spur": {"code_type": "julia"}},
                    "source": ["function nope()\nend\n"]
                }
            ]
        });
        let bytes = serde_json::to_vec(&nb).expect("serialize notebook");

        let facts = run_notebook_extraction(Path::new("nb.ipynb"), &bytes)
            .expect("unknown language cell is skipped");

        assert!(facts
            .nodes
            .iter()
            .any(|node| { node.kind == NodeKind::File && node.label == "nb.ipynb" }));
        assert!(!has_symbol(&facts, "nope"));
    }

    #[test]
    fn malformed_json_returns_error() {
        let err = run_notebook_extraction(Path::new("nb.ipynb"), b"{bad json")
            .expect_err("malformed JSON should be an error");

        assert!(
            err.to_string().contains("parse .ipynb JSON"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn direct_notebook_facts_match_batch_extraction() {
        let nb = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {"kernelspec": {"name": "python3"}},
            "cells": [
                {
                    "cell_type": "code",
                    "id": "a3f1",
                    "metadata": {},
                    "source": ["def load():\n", "    return 1\n"]
                },
                {
                    "cell_type": "code",
                    "id": "b2c9",
                    "metadata": {},
                    "source": ["x = load()\n"]
                }
            ]
        });
        let bytes = serde_json::to_vec(&nb).expect("serialize notebook");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nb.ipynb");
        std::fs::write(&path, &bytes).expect("write notebook");

        let batch = crate::extract::build_facts_for_paths(dir.path(), std::slice::from_ref(&path))
            .expect("batch notebook facts");
        let direct = crate::extract::extract_notebook_facts(dir.path(), &path, &bytes)
            .expect("direct notebook facts");

        assert_eq!(direct, batch);
    }

    fn run_notebook_extraction(path: &Path, bytes: &[u8]) -> anyhow::Result<GraphFacts> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(path);
        let mut builder = FactBuilder::new(dir.path());
        extract_notebook_file(&mut builder, &path, bytes)?;
        Ok(builder.into_facts())
    }

    fn has_symbol(facts: &GraphFacts, label: &str) -> bool {
        facts.nodes.iter().any(|node| {
            node.label == label && matches!(node.kind, NodeKind::Function | NodeKind::Method)
        })
    }

    fn has_section(facts: &GraphFacts, label: &str) -> bool {
        facts
            .nodes
            .iter()
            .any(|node| node.label == label && node.kind == NodeKind::Section)
    }

    fn all_non_file_nodes_reachable_from_file_via_contains(
        facts: &GraphFacts,
        file_label: &str,
    ) -> bool {
        let file_node = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::File && node.label == file_label)
            .expect("file node")
            .node_id;
        let mut reachable = HashSet::from([file_node]);
        loop {
            let before = reachable.len();
            for edge in facts
                .edges
                .iter()
                .filter(|edge| edge.relation == RelationKind::Contains)
            {
                if reachable.contains(&edge.source_node_id) {
                    if let Some(target) = edge.target_node_id {
                        reachable.insert(target);
                    }
                }
            }
            if reachable.len() == before {
                break;
            }
        }

        facts
            .nodes
            .iter()
            .filter(|node| node.kind != NodeKind::File)
            .all(|node| reachable.contains(&node.node_id))
    }

    fn all_symbols_contained_by_cells(facts: &GraphFacts) -> bool {
        let cell_nodes: HashSet<_> = facts
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Cell)
            .map(|node| node.node_id)
            .collect();

        facts
            .nodes
            .iter()
            .filter(|node| !matches!(node.kind, NodeKind::File | NodeKind::Cell))
            .all(|node| {
                facts.edges.iter().any(|edge| {
                    edge.relation == RelationKind::Contains
                        && edge.target_node_id == Some(node.node_id)
                        && cell_nodes.contains(&edge.source_node_id)
                })
            })
    }

    fn file_directly_contains_symbols(facts: &GraphFacts, file_label: &str) -> bool {
        let file_node = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::File && node.label == file_label)
            .expect("file node")
            .node_id;
        let symbol_nodes: HashSet<_> = facts
            .nodes
            .iter()
            .filter(|node| !matches!(node.kind, NodeKind::File | NodeKind::Cell))
            .map(|node| node.node_id)
            .collect();

        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Contains
                && edge.source_node_id == file_node
                && edge
                    .target_node_id
                    .is_some_and(|target| symbol_nodes.contains(&target))
        })
    }
}
