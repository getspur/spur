use tree_sitter::Node;

use crate::extract::languages::DefinitionBinding;
use crate::extract::tree_sitter::FactBuilder;
use crate::{FileId, NodeId, NodeKind, RelationKind};

pub(crate) fn emit_mcp_tools(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node_id: NodeId,
    source: &str,
    root_node: Node<'_>,
    definitions: &[DefinitionBinding<'_>],
) {
    walk(root_node, &mut |node| {
        let Some((field_node, tool_name)) = tool_definition_name_field(node, source) else {
            return;
        };
        let tool_node = builder.add_node(
            relative_path,
            tool_name.clone(),
            tool_name,
            NodeKind::McpTool,
            file_id,
            field_node,
        );
        let parent_id = nearest_definition(file_node_id, definitions, field_node);
        builder.add_edge(parent_id, Some(tool_node), RelationKind::Contains, None);
    });
}

fn walk(node: Node<'_>, visit: &mut impl FnMut(Node<'_>)) {
    visit(node);

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, visit);
    }
}

fn tool_definition_name_field<'tree>(
    node: Node<'tree>,
    source: &str,
) -> Option<(Node<'tree>, String)> {
    if node.kind() != "struct_expression" || !is_tool_definition_struct(node, source) {
        return None;
    }

    let field_list = named_child_of_kind(node, "field_initializer_list")?;
    let mut cursor = field_list.walk();
    for field in field_list
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "field_initializer")
    {
        if field_name(field, source).as_deref() != Some("name") {
            continue;
        }
        let literal = first_descendant_of_kind(field, "string_literal")?;
        let tool_name = string_literal_content(literal, source)?;
        if !tool_name.is_empty() {
            return Some((field, tool_name));
        }
    }

    None
}

fn is_tool_definition_struct(node: Node<'_>, source: &str) -> bool {
    node.child_by_field_name("type")
        .or_else(|| named_child_of_kind(node, "type_identifier"))
        .is_some_and(|type_node| {
            type_node.kind() == "type_identifier"
                && node_text(type_node, source) == "ToolDefinition"
        })
}

fn field_name(field: Node<'_>, source: &str) -> Option<String> {
    field
        .child_by_field_name("field")
        .or_else(|| field.child_by_field_name("name"))
        .or_else(|| named_child_of_kind(field, "field_identifier"))
        .map(|node| node_text(node, source).trim().to_string())
        .filter(|name| !name.is_empty())
}

fn nearest_definition(
    file_node_id: NodeId,
    definitions: &[DefinitionBinding<'_>],
    node: Node<'_>,
) -> NodeId {
    definitions
        .iter()
        .rev()
        .find(|definition| contains(definition.node, node))
        .map(|definition| definition.node_id)
        .unwrap_or(file_node_id)
}

fn named_child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

fn first_descendant_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = first_descendant_of_kind(child, kind) {
            return Some(found);
        }
    }

    None
}

fn string_literal_content(literal: Node<'_>, source: &str) -> Option<String> {
    let text = node_text(literal, source).trim();
    if text.starts_with('"') {
        return serde_json::from_str::<String>(text).ok();
    }

    raw_string_literal_content(text)
}

fn raw_string_literal_content(text: &str) -> Option<String> {
    if !text.starts_with('r') {
        return None;
    }
    let quote_index = text.find('"')?;
    let hashes = &text[1..quote_index];
    if !hashes.bytes().all(|byte| byte == b'#') {
        return None;
    }
    let closing_delimiter = format!("\"{hashes}");
    if !text.ends_with(&closing_delimiter) {
        return None;
    }
    Some(text[quote_index + 1..text.len() - closing_delimiter.len()].to_string())
}

fn contains(parent: Node<'_>, child: Node<'_>) -> bool {
    parent.start_byte() <= child.start_byte() && child.end_byte() <= parent.end_byte()
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}
