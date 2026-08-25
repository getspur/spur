use tree_sitter::Node;

use crate::extract::tree_sitter::FactBuilder;
use crate::{FileId, NodeId, NodeKind, RelationKind};

const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Naive named-key JSON/YAML extract stays within the 750ms rebuild budget
/// at elevenlabs density (1228 keys / 62_000 bytes / 110ms) iff keys ≤ 3206
/// (`sol_30c3db6ac2074bb1`; 3207 unsat `sol_9d016ca3d4fa4223`).
/// 3206 * 62_000 / 1228 = 161866 (`sol_623e3687e68546e4`).
pub(crate) const MAX_STRUCTURED_EXTRACT_BYTES: usize = 161_866;

struct Pair<'tree> {
    key: String,
    key_node: Node<'tree>,
    value: Node<'tree>,
}

#[derive(Clone, Copy)]
enum Dialect {
    Json,
    Yaml,
}

/// OpenAPI/Swagger-aware extract: path templates, HTTP methods, operationId
/// values, and `components.schemas` / Swagger `definitions` names only.
/// Returns true when the document was handled (skip generic named-key extract).
pub(crate) fn extract_openapi_document(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node: NodeId,
    source: &str,
    root_node: Node<'_>,
    language_label: &str,
) -> bool {
    let dialect = match language_label {
        "json" => Dialect::Json,
        "yaml" => Dialect::Yaml,
        _ => return false,
    };
    let Some(root_mapping) = root_mapping(dialect, root_node) else {
        return false;
    };
    let pairs = mapping_pairs(dialect, root_mapping, source);
    if !pairs
        .iter()
        .any(|pair| pair.key == "openapi" || pair.key == "swagger")
    {
        return false;
    }

    let file = Parent {
        node_id: file_node,
        fqn: String::new(),
    };
    for pair in &pairs {
        match pair.key.as_str() {
            "paths" => {
                if let Some(mapping) = as_mapping(dialect, pair.value) {
                    let paths = emit(
                        builder,
                        relative_path,
                        file_id,
                        &file,
                        pair.key.clone(),
                        NodeKind::Module,
                        pair.key_node,
                    );
                    extract_paths(
                        builder,
                        relative_path,
                        file_id,
                        dialect,
                        source,
                        mapping,
                        &paths,
                    );
                }
            }
            "components" => {
                if let Some(mapping) = as_mapping(dialect, pair.value) {
                    let components = emit(
                        builder,
                        relative_path,
                        file_id,
                        &file,
                        pair.key.clone(),
                        NodeKind::Module,
                        pair.key_node,
                    );
                    extract_component_schemas(
                        builder,
                        relative_path,
                        file_id,
                        dialect,
                        source,
                        mapping,
                        &components,
                    );
                }
            }
            "definitions" => {
                if let Some(mapping) = as_mapping(dialect, pair.value) {
                    extract_schema_names(
                        builder,
                        relative_path,
                        file_id,
                        dialect,
                        source,
                        mapping,
                        &file,
                    );
                }
            }
            _ => {}
        }
    }
    true
}

struct Parent {
    node_id: NodeId,
    fqn: String,
}

fn extract_paths(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    dialect: Dialect,
    source: &str,
    mapping: Node<'_>,
    parent: &Parent,
) {
    for pair in mapping_pairs(dialect, mapping, source) {
        let Some(path_mapping) = as_mapping(dialect, pair.value) else {
            continue;
        };
        let path = emit(
            builder,
            relative_path,
            file_id,
            parent,
            pair.key,
            NodeKind::Module,
            pair.key_node,
        );
        for inner in mapping_pairs(dialect, path_mapping, source) {
            if !is_http_method(&inner.key) {
                continue;
            }
            let Some(operation) = as_mapping(dialect, inner.value) else {
                continue;
            };
            let method = emit(
                builder,
                relative_path,
                file_id,
                &path,
                inner.key,
                NodeKind::Module,
                inner.key_node,
            );
            for field in mapping_pairs(dialect, operation, source) {
                if field.key != "operationId" {
                    continue;
                }
                let Some(name) = scalar_string(dialect, field.value, source) else {
                    continue;
                };
                emit(
                    builder,
                    relative_path,
                    file_id,
                    &method,
                    name,
                    NodeKind::Field,
                    field.value,
                );
            }
        }
    }
}

fn extract_component_schemas(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    dialect: Dialect,
    source: &str,
    components: Node<'_>,
    parent: &Parent,
) {
    for pair in mapping_pairs(dialect, components, source) {
        if pair.key != "schemas" {
            continue;
        }
        let Some(schemas) = as_mapping(dialect, pair.value) else {
            continue;
        };
        let schemas_parent = emit(
            builder,
            relative_path,
            file_id,
            parent,
            pair.key,
            NodeKind::Module,
            pair.key_node,
        );
        extract_schema_names(
            builder,
            relative_path,
            file_id,
            dialect,
            source,
            schemas,
            &schemas_parent,
        );
    }
}

fn extract_schema_names(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    dialect: Dialect,
    source: &str,
    mapping: Node<'_>,
    parent: &Parent,
) {
    for pair in mapping_pairs(dialect, mapping, source) {
        if as_mapping(dialect, pair.value).is_none() {
            continue;
        }
        emit(
            builder,
            relative_path,
            file_id,
            parent,
            pair.key,
            NodeKind::Module,
            pair.key_node,
        );
    }
}

fn emit(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    parent: &Parent,
    label: String,
    kind: NodeKind,
    node: Node<'_>,
) -> Parent {
    let fqn = if parent.fqn.is_empty() {
        label.clone()
    } else {
        format!("{}::{label}", parent.fqn)
    };
    let node_id = builder.add_node(relative_path, label, fqn.clone(), kind, file_id, node);
    builder.add_edge(parent.node_id, Some(node_id), RelationKind::Contains, None);
    if !parent.fqn.is_empty() {
        builder.add_edge(parent.node_id, Some(node_id), RelationKind::Defines, None);
    }
    Parent { node_id, fqn }
}

fn is_http_method(key: &str) -> bool {
    HTTP_METHODS
        .iter()
        .any(|method| key.eq_ignore_ascii_case(method))
}

fn root_mapping<'tree>(dialect: Dialect, root: Node<'tree>) -> Option<Node<'tree>> {
    match dialect {
        Dialect::Json => json_root_object(root),
        Dialect::Yaml => yaml_unwrap_mapping(root),
    }
}

fn as_mapping<'tree>(dialect: Dialect, node: Node<'tree>) -> Option<Node<'tree>> {
    match dialect {
        Dialect::Json => (node.kind() == "object").then_some(node),
        Dialect::Yaml => yaml_unwrap_mapping(node),
    }
}

fn mapping_pairs<'tree>(dialect: Dialect, mapping: Node<'tree>, source: &str) -> Vec<Pair<'tree>> {
    match dialect {
        Dialect::Json => json_object_pairs(mapping, source),
        Dialect::Yaml => yaml_mapping_pairs(mapping, source),
    }
}

fn scalar_string(dialect: Dialect, node: Node<'_>, source: &str) -> Option<String> {
    match dialect {
        Dialect::Json => {
            if node.kind() != "string" {
                return None;
            }
            Some(unquote(node.utf8_text(source.as_bytes()).ok()?.trim()))
        }
        Dialect::Yaml => yaml_scalar_string(node, source),
    }
}

fn json_root_object(root: Node<'_>) -> Option<Node<'_>> {
    if root.kind() == "object" {
        return Some(root);
    }
    (0..root.named_child_count()).find_map(|index| {
        let child = root.named_child(index)?;
        (child.kind() == "object").then_some(child)
    })
}

fn json_object_pairs<'tree>(object: Node<'tree>, source: &str) -> Vec<Pair<'tree>> {
    let mut pairs = Vec::new();
    let mut cursor = object.walk();
    for child in object.children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let Some(key_node) = child.child_by_field_name("key") else {
            continue;
        };
        let Some(value) = child.child_by_field_name("value") else {
            continue;
        };
        let Ok(raw) = key_node.utf8_text(source.as_bytes()) else {
            continue;
        };
        pairs.push(Pair {
            key: unquote(raw.trim()),
            key_node,
            value,
        });
    }
    pairs
}

fn yaml_unwrap_mapping(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "block_mapping" | "flow_mapping" => Some(node),
        "stream" | "document" | "block_node" | "flow_node" => (0..node.named_child_count())
            .find_map(|index| node.named_child(index).and_then(yaml_unwrap_mapping)),
        _ => None,
    }
}

fn yaml_mapping_pairs<'tree>(mapping: Node<'tree>, source: &str) -> Vec<Pair<'tree>> {
    let pair_kind = match mapping.kind() {
        "block_mapping" => "block_mapping_pair",
        "flow_mapping" => "flow_pair",
        _ => return Vec::new(),
    };
    let mut pairs = Vec::new();
    let mut cursor = mapping.walk();
    for child in mapping.children(&mut cursor) {
        if child.kind() != pair_kind {
            continue;
        }
        let Some(key_node) = child.child_by_field_name("key") else {
            continue;
        };
        let Some(value) = child.child_by_field_name("value") else {
            continue;
        };
        let Some(key) = yaml_scalar_string(key_node, source) else {
            continue;
        };
        pairs.push(Pair {
            key,
            key_node,
            value,
        });
    }
    pairs
}

fn yaml_scalar_string(node: Node<'_>, source: &str) -> Option<String> {
    if yaml_unwrap_mapping(node).is_some() {
        return None;
    }
    if matches!(node.kind(), "block_sequence" | "flow_sequence") {
        return None;
    }
    let leaf = yaml_scalar_leaf(node)?;
    if matches!(
        leaf.kind(),
        "block_sequence" | "flow_sequence" | "block_mapping" | "flow_mapping"
    ) {
        return None;
    }
    let text = leaf.utf8_text(source.as_bytes()).ok()?.trim();
    let unquoted = unquote(text);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted)
    }
}

fn yaml_scalar_leaf(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "flow_node" | "block_node" | "plain_scalar" => (0..node.named_child_count())
            .find_map(|index| node.named_child(index).and_then(yaml_scalar_leaf))
            .or(Some(node)),
        _ => Some(node),
    }
}

fn unquote(text: &str) -> String {
    let trimmed = text.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_owned();
        }
    }
    trimmed.to_owned()
}
