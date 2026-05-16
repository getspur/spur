use tree_sitter::{Node, Parser};

use crate::extract::languages::LanguageConfig;
use crate::extract::tree_sitter::{
    relative_path, run_query, CaptureHit, CompiledQueries, FactBuilder, PendingEdge,
};
use crate::{FileId, NodeId, NodeKind, RelationKind};

#[derive(Debug, Clone)]
struct SectionBinding<'tree> {
    node: Node<'tree>,
    node_id: NodeId,
}

pub(crate) fn extract_markdown_file(
    builder: &mut FactBuilder<'_>,
    config: &LanguageConfig,
    path: &std::path::Path,
    source: &str,
    root_node: Node<'_>,
    queries: &CompiledQueries,
    inline_parser: Option<&mut Parser>,
) -> anyhow::Result<()> {
    let relative_path = relative_path(builder.root(), path)?;
    let file_id = FileId(builder.next_file_id());
    let file_node = builder.add_file_node(&relative_path, file_id, root_node);

    let tag_captures = run_query(&queries.tags, root_node, source);
    let sections = emit_sections(
        config,
        builder,
        &relative_path,
        file_id,
        file_node,
        source,
        &tag_captures,
    );

    emit_markdown_links(
        builder,
        config,
        file_node,
        source,
        &sections,
        root_node,
        queries,
        inline_parser,
        &relative_path,
    )?;
    Ok(())
}

fn emit_sections<'tree>(
    config: &LanguageConfig,
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node_id: NodeId,
    source: &str,
    captures: &[CaptureHit<'tree>],
) -> Vec<SectionBinding<'tree>> {
    let Some((definition_capture, _)) = config.definition_kind_map.first() else {
        return Vec::new();
    };

    let mut headings: Vec<(Node<'tree>, String, usize)> = captures
        .iter()
        .filter(|capture| capture.name == *definition_capture)
        .filter_map(|capture| {
            let name = definition_name(capture.node, source, captures)?;
            let level = heading_level(capture.node)?;
            Some((capture.node, name, level))
        })
        .collect();

    headings.sort_by_key(|(node, _, _)| (node.start_byte(), node.end_byte()));

    let mut sections = Vec::new();
    let mut stack: Vec<(usize, NodeId, String)> = Vec::new();

    for (node, label, level) in headings {
        while stack
            .last()
            .is_some_and(|(ancestor_level, _, _)| *ancestor_level >= level)
        {
            stack.pop();
        }

        let (parent_id, parent_fqn) = stack
            .last()
            .map(|(_, node_id, fqn)| (*node_id, Some(fqn.as_str())))
            .unwrap_or((file_node_id, None));

        let fqn = scoped_name(parent_fqn.unwrap_or(""), &label);
        let node_id = builder.add_node(
            relative_path,
            label,
            fqn.clone(),
            NodeKind::Section,
            file_id,
            node,
        );
        builder.add_edge(parent_id, Some(node_id), RelationKind::Contains, None);
        stack.push((level, node_id, fqn.clone()));
        sections.push(SectionBinding { node, node_id });
    }

    sections
}

#[allow(clippy::too_many_arguments)]
fn emit_markdown_links(
    builder: &mut FactBuilder<'_>,
    config: &LanguageConfig,
    file_node_id: NodeId,
    source: &str,
    sections: &[SectionBinding<'_>],
    root_node: Node<'_>,
    queries: &CompiledQueries,
    inline_parser: Option<&mut Parser>,
    relative_path: &str,
) -> anyhow::Result<()> {
    emit_markdown_block_links(
        builder,
        config,
        file_node_id,
        source,
        sections,
        root_node,
        queries,
        relative_path,
    );

    let Some(inline_query) = queries.inline_spur_edges.as_ref() else {
        return Ok(());
    };
    let Some(inline_parser) = inline_parser else {
        return Ok(());
    };

    let relation = config
        .relation_kind_map
        .and_then(|map| {
            map.iter()
                .find_map(|(name, kind)| (*name == "import").then_some(*kind))
        })
        .unwrap_or(RelationKind::Imports);

    for inline_node in inline_nodes(root_node) {
        let Some(text) = inline_node.utf8_text(source.as_bytes()).ok() else {
            continue;
        };
        let Some(inline_tree) = inline_parser.parse(text, None) else {
            continue;
        };

        let inline_captures = run_query(inline_query, inline_tree.root_node(), text);
        let source_id = source_section_for_inline(file_node_id, sections, inline_node);
        for capture in inline_captures
            .iter()
            .filter(|capture| capture.name == "import")
        {
            for target in
                contained_capture_text(capture.node, text, &inline_captures, "import.name")
            {
                if let Some(target_name) = normalize_link_target(&target, relative_path) {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name,
                        relation,
                    });
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_markdown_block_links(
    builder: &mut FactBuilder<'_>,
    config: &LanguageConfig,
    file_node_id: NodeId,
    source: &str,
    sections: &[SectionBinding<'_>],
    root_node: Node<'_>,
    queries: &CompiledQueries,
    relative_path: &str,
) {
    let Some(block_query) = queries.spur_edges.as_ref() else {
        return;
    };
    let captures = run_query(block_query, root_node, source);
    let relation = config
        .relation_kind_map
        .and_then(|map| {
            map.iter()
                .find_map(|(name, kind)| (*name == "import").then_some(*kind))
        })
        .unwrap_or(RelationKind::Imports);

    for capture in captures.iter().filter(|capture| capture.name == "import") {
        let source_id = source_section_for_inline(file_node_id, sections, capture.node);
        for target in contained_capture_text(capture.node, source, &captures, "import.name") {
            if let Some(target_name) = normalize_link_target(&target, relative_path) {
                builder.pending_edges.push(PendingEdge {
                    source: source_id,
                    target_name,
                    relation,
                });
            }
        }
    }
}

fn inline_nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "inline" {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|node| (node.start_byte(), node.end_byte()));
    out
}

fn source_section_for_inline(
    file_node_id: NodeId,
    sections: &[SectionBinding<'_>],
    inline_node: Node<'_>,
) -> NodeId {
    let pos = inline_node.start_byte();
    sections
        .iter()
        .rev()
        .find(|section| section.node.start_byte() <= pos)
        .map(|section| section.node_id)
        .unwrap_or(file_node_id)
}

fn normalize_link_target(target: &str, relative_path: &str) -> Option<String> {
    let mut normalized = target.trim();
    if normalized.starts_with('#') {
        return Some(relative_path.to_string());
    }
    if let Some(inner) = normalized
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    {
        normalized = inner;
    }
    normalized = normalized.strip_prefix("./").unwrap_or(normalized);

    let normalized = normalized
        .split('#')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();

    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn definition_name(
    definition_node: Node<'_>,
    source: &str,
    captures: &[CaptureHit<'_>],
) -> Option<String> {
    contained_capture_text(definition_node, source, captures, "name")
        .into_iter()
        .next()
}

fn heading_level(node: Node<'_>) -> Option<usize> {
    if node.kind() == "setext_heading" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "setext_h1_underline" {
                return Some(1);
            }
            if child.kind() == "setext_h2_underline" {
                return Some(2);
            }
        }
        return None;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "atx_h1_marker" {
            return Some(1);
        }
        if child.kind() == "atx_h2_marker" {
            return Some(2);
        }
        if child.kind() == "atx_h3_marker" {
            return Some(3);
        }
        if child.kind() == "atx_h4_marker" {
            return Some(4);
        }
        if child.kind() == "atx_h5_marker" {
            return Some(5);
        }
        if child.kind() == "atx_h6_marker" {
            return Some(6);
        }
    }
    None
}

fn contains(parent: Node<'_>, child: Node<'_>) -> bool {
    parent.start_byte() <= child.start_byte() && child.end_byte() <= parent.end_byte()
}

fn scoped_name(prefix: &str, label: &str) -> String {
    if prefix.is_empty() {
        label.to_string()
    } else {
        format!("{prefix}::{label}")
    }
}

fn contained_capture_text(
    parent: Node<'_>,
    source: &str,
    captures: &[CaptureHit<'_>],
    capture_name: &str,
) -> Vec<String> {
    captures
        .iter()
        .filter(|capture| capture.name == capture_name && contains(parent, capture.node))
        .map(|capture| child_text(capture.node, source).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect()
}

fn child_text(node: Node<'_>, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .unwrap_or_default()
        .to_string()
}
