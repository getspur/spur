use tree_sitter::Node;

use crate::extract::languages::{nearest_parent, DefinitionBinding};
use crate::extract::tree_sitter::{FactBuilder, PendingEdge};
use crate::{NodeId, RelationKind};

pub(crate) fn emit_macro_call_edges(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    root_node: Node<'_>,
    definitions: &[DefinitionBinding<'_>],
) {
    walk(root_node, &mut |node| {
        if node.kind() != "macro_invocation" {
            return;
        }

        let mut cursor = node.walk();
        for child in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "token_tree")
        {
            scan_token_tree(builder, file_node_id, source, child, definitions);
        }
    });
}

fn scan_token_tree(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    token_tree: Node<'_>,
    definitions: &[DefinitionBinding<'_>],
) {
    for index in 0..token_tree.child_count() {
        let Some(child) = token_tree.child(index) else {
            continue;
        };

        match child.kind() {
            "identifier" => emit_identifier_call(
                builder,
                file_node_id,
                source,
                token_tree,
                index,
                child,
                definitions,
            ),
            "scoped_identifier" => emit_scoped_identifier_call(
                builder,
                file_node_id,
                source,
                token_tree,
                index,
                child,
                definitions,
            ),
            "token_tree" => scan_token_tree(builder, file_node_id, source, child, definitions),
            _ => {}
        }
    }
}

fn emit_identifier_call(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    parent: Node<'_>,
    index: usize,
    identifier: Node<'_>,
    definitions: &[DefinitionBinding<'_>],
) {
    let Some(right) = next_significant_child(parent, index) else {
        return;
    };
    if right.kind() == "!" {
        return;
    }
    if !opens_call_parens(right) {
        return;
    }

    if previous_significant_child(parent, index).is_some_and(|left| left.kind() == "::") {
        if path_call_starts_before(parent, index) {
            emit_call(builder, file_node_id, source, identifier, definitions);
        }
        return;
    }

    emit_call(builder, file_node_id, source, identifier, definitions);
}

fn emit_scoped_identifier_call(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    parent: Node<'_>,
    index: usize,
    scoped_identifier: Node<'_>,
    definitions: &[DefinitionBinding<'_>],
) {
    let Some(right) = next_significant_child(parent, index) else {
        return;
    };
    if right.kind() == "!" || !opens_call_parens(right) {
        return;
    }

    if let Some(identifier) = scoped_identifier_name(scoped_identifier) {
        emit_call(builder, file_node_id, source, identifier, definitions);
    }
}

fn emit_call(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    identifier: Node<'_>,
    definitions: &[DefinitionBinding<'_>],
) {
    let target_name = node_text(identifier, source).trim();
    if target_name.is_empty() {
        return;
    }

    let source_id = nearest_parent(file_node_id, definitions, identifier).node_id;
    builder.pending_edges.push(PendingEdge {
        source: source_id,
        target_name: target_name.to_string(),
        relation: RelationKind::Calls,
    });
}

fn walk(node: Node<'_>, visit: &mut impl FnMut(Node<'_>)) {
    visit(node);

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, visit);
    }
}

fn next_significant_child(parent: Node<'_>, index: usize) -> Option<Node<'_>> {
    for next_index in index + 1..parent.child_count() {
        let child = parent.child(next_index)?;
        if is_significant(child) {
            return Some(child);
        }
    }
    None
}

fn previous_significant_child(parent: Node<'_>, index: usize) -> Option<Node<'_>> {
    let mut previous_index = index.checked_sub(1)?;
    loop {
        let child = parent.child(previous_index)?;
        if is_significant(child) {
            return Some(child);
        }
        let Some(next_index) = previous_index.checked_sub(1) else {
            return None;
        };
        previous_index = next_index;
    }
}

fn is_significant(node: Node<'_>) -> bool {
    !node.is_extra() && !matches!(node.kind(), "line_comment" | "block_comment")
}

fn opens_call_parens(node: Node<'_>) -> bool {
    if node.kind() == "(" {
        return true;
    }
    if node.kind() != "token_tree" {
        return false;
    }

    first_significant_child(node).is_some_and(|child| child.kind() == "(")
}

fn first_significant_child(parent: Node<'_>) -> Option<Node<'_>> {
    for index in 0..parent.child_count() {
        let child = parent.child(index)?;
        if is_significant(child) {
            return Some(child);
        }
    }
    None
}

fn path_call_starts_before(parent: Node<'_>, identifier_index: usize) -> bool {
    let Some(scope_operator_index) = previous_significant_index(parent, identifier_index) else {
        return false;
    };
    let Some(path_segment_index) = previous_significant_index(parent, scope_operator_index) else {
        return false;
    };
    parent
        .child(path_segment_index)
        .is_some_and(is_path_segment)
}

fn is_path_segment(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "scoped_identifier" | "type_identifier" | "crate" | "self" | "super"
    )
}

fn previous_significant_index(parent: Node<'_>, index: usize) -> Option<usize> {
    let mut previous_index = index.checked_sub(1)?;
    loop {
        let child = parent.child(previous_index)?;
        if is_significant(child) {
            return Some(previous_index);
        }
        let Some(next_index) = previous_index.checked_sub(1) else {
            return None;
        };
        previous_index = next_index;
    }
}

fn scoped_identifier_name(scoped_identifier: Node<'_>) -> Option<Node<'_>> {
    scoped_identifier
        .child_by_field_name("name")
        .or_else(|| last_named_identifier(scoped_identifier))
}

fn last_named_identifier(node: Node<'_>) -> Option<Node<'_>> {
    let mut last = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "identifier" {
            last = Some(child);
        }
    }
    last
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}
