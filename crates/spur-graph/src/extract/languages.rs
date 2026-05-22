use std::collections::HashMap;

use tree_sitter::{Language, Node};

use crate::extract::tree_sitter::{CaptureHit, FactBuilder, PendingEdge};
use crate::{FileId, GraphEdgeKind, NodeId, NodeKind, RelationKind};

#[derive(Debug)]
pub(crate) struct LanguageConfig {
    pub(crate) language: Language,
    pub(crate) inline_language: Option<Language>,
    pub(crate) queries: &'static [(&'static str, &'static str)],
    pub(crate) definition_kind_map: &'static [(&'static str, NodeKind)],
    pub(crate) relation_kind_map: Option<&'static [(&'static str, RelationKind)]>,
    pub(crate) is_method: Option<fn(Node<'_>) -> bool>,
}

pub(crate) struct LanguageDescriptor {
    pub(crate) matcher: fn(&std::path::Path) -> bool,
    pub(crate) factory: fn() -> LanguageConfig,
    pub(crate) label: &'static str,
    pub(crate) extensions: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub(crate) struct DefinitionBinding<'tree> {
    pub(crate) node: Node<'tree>,
    pub(crate) node_id: NodeId,
    fqn: String,
}

pub(crate) fn rust_config() -> LanguageConfig {
    LanguageConfig {
        language: tree_sitter_rust::LANGUAGE.into(),
        inline_language: None,
        queries: &[
            ("tags", include_str!("../../queries/rust/tags.scm")),
            (
                "spur-edges",
                include_str!("../../queries/rust/spur-edges.scm"),
            ),
        ],
        definition_kind_map: &[
            ("definition.module", NodeKind::Module),
            ("definition.function", NodeKind::Function),
            ("definition.method", NodeKind::Method),
            ("definition.struct", NodeKind::Struct),
            ("definition.enum", NodeKind::Enum),
            ("definition.trait", NodeKind::Trait),
            ("definition.impl", NodeKind::Impl),
        ],
        relation_kind_map: None,
        is_method: Some(has_impl_ancestor),
    }
}

pub(crate) fn python_config() -> LanguageConfig {
    LanguageConfig {
        language: tree_sitter_python::LANGUAGE.into(),
        inline_language: None,
        queries: &[
            ("tags", include_str!("../../queries/python/tags.scm")),
            (
                "spur-edges",
                include_str!("../../queries/python/spur-edges.scm"),
            ),
        ],
        definition_kind_map: &[
            ("definition.function", NodeKind::Function),
            ("definition.class", NodeKind::Class),
        ],
        relation_kind_map: None,
        is_method: Some(has_class_definition_ancestor),
    }
}

fn typescript_config_for(language: Language) -> LanguageConfig {
    LanguageConfig {
        language,
        inline_language: None,
        queries: &[
            ("tags", include_str!("../../queries/typescript/tags.scm")),
            (
                "spur-edges",
                include_str!("../../queries/typescript/spur-edges.scm"),
            ),
        ],
        definition_kind_map: &[
            ("definition.class", NodeKind::Class),
            ("definition.interface", NodeKind::Interface),
            ("definition.enum", NodeKind::Enum),
            ("definition.function", NodeKind::Function),
            ("definition.method", NodeKind::Method),
            ("definition.type_alias", NodeKind::TypeAlias),
        ],
        relation_kind_map: None,
        is_method: None,
    }
}

pub(crate) fn typescript_config() -> LanguageConfig {
    typescript_config_for(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
}

pub(crate) fn tsx_config() -> LanguageConfig {
    typescript_config_for(tree_sitter_typescript::LANGUAGE_TSX.into())
}

pub(crate) fn cpp_config() -> LanguageConfig {
    LanguageConfig {
        language: tree_sitter_cpp::LANGUAGE.into(),
        inline_language: None,
        queries: &[
            ("tags", include_str!("../../queries/cpp/tags.scm")),
            (
                "spur-edges",
                include_str!("../../queries/cpp/spur-edges.scm"),
            ),
        ],
        definition_kind_map: &[
            ("definition.module", NodeKind::Module),
            ("definition.class", NodeKind::Class),
            ("definition.struct", NodeKind::Struct),
            ("definition.enum", NodeKind::Enum),
            ("definition.function", NodeKind::Function),
            ("definition.method", NodeKind::Method),
            ("definition.type_alias", NodeKind::TypeAlias),
            ("definition.macro", NodeKind::Macro),
            ("definition.field", NodeKind::Field),
        ],
        relation_kind_map: None,
        is_method: Some(has_cpp_class_ancestor),
    }
}

pub(crate) fn markdown_config() -> LanguageConfig {
    LanguageConfig {
        language: tree_sitter_md::LANGUAGE.into(),
        inline_language: Some(tree_sitter_md::INLINE_LANGUAGE.into()),
        queries: &[
            ("tags", include_str!("../../queries/markdown/tags.scm")),
            (
                "spur-edges",
                include_str!("../../queries/markdown/spur-edges.scm"),
            ),
            (
                "inline-spur-edges",
                include_str!("../../queries/markdown/inline-spur-edges.scm"),
            ),
        ],
        definition_kind_map: &[("definition.section", NodeKind::Section)],
        relation_kind_map: Some(&[("import", RelationKind::Links)]),
        is_method: None,
    }
}

fn markdown_matcher(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn rust_matcher(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn python_matcher(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
}

fn typescript_matcher(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ts"))
}

fn tsx_matcher(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tsx"))
}

const CPP_EXTENSIONS: &[&str] = &[
    "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "tpp", "h",
];

fn cpp_matcher(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            CPP_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

pub(crate) fn language_registry() -> &'static [LanguageDescriptor] {
    &[
        LanguageDescriptor {
            matcher: rust_matcher,
            factory: rust_config,
            label: "rust",
            extensions: &["rs"],
        },
        LanguageDescriptor {
            matcher: python_matcher,
            factory: python_config,
            label: "python",
            extensions: &["py"],
        },
        LanguageDescriptor {
            matcher: typescript_matcher,
            factory: typescript_config,
            label: "typescript",
            extensions: &["ts"],
        },
        LanguageDescriptor {
            matcher: tsx_matcher,
            factory: tsx_config,
            label: "tsx",
            extensions: &["tsx"],
        },
        LanguageDescriptor {
            matcher: cpp_matcher,
            factory: cpp_config,
            label: "cpp",
            extensions: CPP_EXTENSIONS,
        },
        LanguageDescriptor {
            matcher: markdown_matcher,
            factory: markdown_config,
            label: "markdown",
            extensions: &["md"],
        },
    ]
}

pub(crate) fn all_supported_extensions() -> Vec<&'static str> {
    language_registry()
        .iter()
        .flat_map(|descriptor| descriptor.extensions.iter().copied())
        .collect()
}

pub(crate) fn emit_definitions<'tree>(
    config: &LanguageConfig,
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node_id: NodeId,
    source: &str,
    captures: &[CaptureHit<'tree>],
) -> Vec<DefinitionBinding<'tree>> {
    let mut definitions: Vec<_> = captures
        .iter()
        .filter_map(|capture| {
            definition_kind(config, capture.name.as_str(), capture.node).map(|kind| {
                (
                    kind,
                    capture.node,
                    definition_name(kind, capture.node, source, captures),
                )
            })
        })
        .collect();

    definitions.sort_by_key(|(kind, node, _)| {
        (node.start_byte(), node.end_byte(), definition_rank(*kind))
    });
    definitions.dedup_by(|left, right| {
        left.1.start_byte() == right.1.start_byte()
            && left.1.end_byte() == right.1.end_byte()
            && left.1.kind() == right.1.kind()
    });

    let mut bindings: Vec<DefinitionBinding<'tree>> = Vec::new();
    for (kind, node, label) in definitions {
        // Language adapters must provide an inner @name capture for every definition.
        // If a grammar edge case lacks one, skip it until the query is extended.
        let Some(label) = label else {
            continue;
        };
        let parent = nearest_parent(file_node_id, &bindings, node);
        let fqn = scoped_name(parent.fqn.unwrap_or(""), &label);
        let node_id = builder.add_node(relative_path, label, fqn.clone(), kind, file_id, node);
        builder.add_edge(parent.node_id, Some(node_id), RelationKind::Contains, None);
        bindings.push(DefinitionBinding { node, node_id, fqn });
    }
    bindings
}

pub(crate) fn emit_edges(
    config: &LanguageConfig,
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    definitions: &[DefinitionBinding<'_>],
    captures: &[CaptureHit<'_>],
) {
    for capture in captures {
        match capture.name.as_str() {
            "import" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                let relation = relation_kind_for_capture(config, "import", RelationKind::Imports);
                for imported in
                    contained_capture_text(capture.node, source, captures, "import.name")
                {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: imported,
                        relation,
                        edge_kind: None,
                    });
                }
            }
            "call" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for callee in contained_capture_text(capture.node, source, captures, "call.name") {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: callee,
                        relation: RelationKind::Calls,
                        edge_kind: None,
                    });
                }
            }
            "reference.name" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                let Ok(target_name) = capture.node.utf8_text(source.as_bytes()) else {
                    continue;
                };
                builder.pending_edges.push(PendingEdge {
                    source: source_id,
                    target_name: target_name.to_string(),
                    relation: RelationKind::References,
                    edge_kind: Some(GraphEdgeKind::ReferencesHof),
                });
            }
            _ => {}
        }
    }
}

pub(crate) fn emit_rust_dyn_trait_edges(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    definitions: &[DefinitionBinding<'_>],
    root_node: Node<'_>,
) {
    let mut bindings_by_scope: HashMap<NodeId, HashMap<String, String>> = HashMap::new();
    for definition in definitions {
        let mut bindings = HashMap::new();
        collect_dyn_trait_bindings(definition.node, definition.node, source, &mut bindings);
        if !bindings.is_empty() {
            bindings_by_scope.insert(definition.node_id, bindings);
        }
    }

    let mut calls = Vec::new();
    collect_nodes_by_kind(root_node, "call_expression", &mut calls);
    for call in calls {
        let source_id = nearest_parent(file_node_id, definitions, call).node_id;
        let Some(bindings) = bindings_by_scope.get(&source_id) else {
            continue;
        };
        let Some((receiver, method)) = receiver_method_call(call, source) else {
            continue;
        };
        let Some(trait_name) = bindings.get(&receiver) else {
            continue;
        };
        builder.pending_edges.push(PendingEdge {
            source: source_id,
            target_name: format!("{trait_name}::{method}"),
            relation: RelationKind::Calls,
            edge_kind: Some(GraphEdgeKind::CallsDyn),
        });
    }
}

fn collect_dyn_trait_bindings(
    root: Node<'_>,
    node: Node<'_>,
    source: &str,
    bindings: &mut HashMap<String, String>,
) {
    if node != root && is_definition_node(node) {
        return;
    }

    if matches!(node.kind(), "parameter" | "let_declaration") {
        if let Some((name, trait_name)) = dyn_trait_binding(node, source) {
            bindings.insert(name, trait_name);
        }
    }

    for index in 0..node.named_child_count() {
        if let Some(child) = node.named_child(index) {
            collect_dyn_trait_bindings(root, child, source, bindings);
        }
    }
}

fn dyn_trait_binding(node: Node<'_>, source: &str) -> Option<(String, String)> {
    let pattern = node.child_by_field_name("pattern")?;
    let name = single_identifier_text(pattern, source)?;
    let type_node = node.child_by_field_name("type")?;
    let trait_name = dyn_trait_name_from_type(type_node, source)?;
    Some((name, trait_name))
}

fn dyn_trait_name_from_type(type_node: Node<'_>, source: &str) -> Option<String> {
    match type_node.kind() {
        "reference_type" => {
            let inner = type_node.child_by_field_name("type")?;
            (inner.kind() == "dynamic_type").then(|| trait_name_from_dynamic_type(inner, source))?
        }
        "generic_type" => {
            let base = type_node.child_by_field_name("type")?;
            let base = child_text(base, source).trim();
            if !matches!(base, "Box" | "Arc" | "Rc") {
                return None;
            }
            let type_arguments = type_node.child_by_field_name("type_arguments")?;
            let dynamic = direct_named_child_by_kind(type_arguments, "dynamic_type")?;
            trait_name_from_dynamic_type(dynamic, source)
        }
        _ => None,
    }
}

fn trait_name_from_dynamic_type(dynamic: Node<'_>, source: &str) -> Option<String> {
    let text = child_text(dynamic, source).trim();
    let rest = text.strip_prefix("dyn")?.trim_start();
    let primary = rest.split('+').next()?.trim();
    if primary.is_empty() || primary.contains(char::is_whitespace) || primary.contains('<') {
        return None;
    }
    Some(primary.to_string())
}

fn direct_named_child_by_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    (0..node.named_child_count())
        .filter_map(|index| node.named_child(index))
        .find(|child| child.kind() == kind)
}

fn receiver_method_call(call: Node<'_>, source: &str) -> Option<(String, String)> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "field_expression" {
        return None;
    }
    let receiver = function.child_by_field_name("value")?;
    if receiver.kind() != "identifier" {
        return None;
    }
    let method = function.child_by_field_name("field")?;
    if method.kind() != "field_identifier" {
        return None;
    }
    Some((
        child_text(receiver, source).trim().to_string(),
        child_text(method, source).trim().to_string(),
    ))
}

fn collect_nodes_by_kind<'tree>(node: Node<'tree>, kind: &str, nodes: &mut Vec<Node<'tree>>) {
    if node.kind() == kind {
        nodes.push(node);
    }
    for index in 0..node.named_child_count() {
        if let Some(child) = node.named_child(index) {
            collect_nodes_by_kind(child, kind, nodes);
        }
    }
}

fn single_identifier_text(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(child_text(node, source).trim().to_string());
    }

    let identifiers = named_descendants_by_kind(node, "identifier");
    match identifiers.as_slice() {
        [identifier] => Some(child_text(*identifier, source).trim().to_string()),
        _ => None,
    }
}

fn named_descendants_by_kind<'tree>(node: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut nodes = Vec::new();
    collect_nodes_by_kind(node, kind, &mut nodes);
    nodes
}

fn is_definition_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "mod_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "function_item"
            | "function_signature_item"
    )
}

fn relation_kind_for_capture(
    config: &LanguageConfig,
    capture_name: &str,
    default: RelationKind,
) -> RelationKind {
    config
        .relation_kind_map
        .and_then(|map| {
            map.iter()
                .find_map(|(name, kind)| (*name == capture_name).then_some(*kind))
        })
        .unwrap_or(default)
}

fn definition_kind(
    config: &LanguageConfig,
    capture_name: &str,
    node: Node<'_>,
) -> Option<NodeKind> {
    let mut kind = config
        .definition_kind_map
        .iter()
        .find_map(|(name, kind)| (*name == capture_name).then_some(*kind))?;
    if kind == NodeKind::Function && config.is_method.is_some_and(|is_method| is_method(node)) {
        kind = NodeKind::Method;
    }
    Some(kind)
}

fn definition_name(
    kind: NodeKind,
    definition_node: Node<'_>,
    source: &str,
    captures: &[CaptureHit<'_>],
) -> Option<String> {
    if kind == NodeKind::Impl {
        let self_type = contained_capture_text(definition_node, source, captures, "impl.self")
            .into_iter()
            .next()?;
        let trait_type = contained_capture_text(definition_node, source, captures, "impl.trait")
            .into_iter()
            .next();
        return Some(match trait_type {
            Some(trait_type) => format!("{trait_type} for {self_type}"),
            None => self_type,
        });
    }

    contained_capture_text(definition_node, source, captures, "name")
        .into_iter()
        .next()
}

fn nearest_parent<'a, 'tree>(
    file_node_id: NodeId,
    definitions: &'a [DefinitionBinding<'tree>],
    node: Node<'_>,
) -> Parent<'a> {
    definitions
        .iter()
        .rev()
        .find(|definition| contains(definition.node, node))
        .map(|definition| Parent {
            node_id: definition.node_id,
            fqn: Some(&definition.fqn),
        })
        .unwrap_or(Parent {
            node_id: file_node_id,
            fqn: None,
        })
}

#[derive(Debug, Clone, Copy)]
struct Parent<'a> {
    node_id: NodeId,
    fqn: Option<&'a str>,
}

fn contains(parent: Node<'_>, child: Node<'_>) -> bool {
    parent.start_byte() <= child.start_byte() && child.end_byte() <= parent.end_byte()
}

fn has_impl_ancestor(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let Some(grandparent) = parent.parent() else {
        return false;
    };

    parent.kind() == "declaration_list" && grandparent.kind() == "impl_item"
}

fn has_cpp_class_ancestor(node: Node<'_>) -> bool {
    // A C++ `function_definition` is a method when it appears inside a class
    // or struct body (i.e. its enclosing `field_declaration_list` belongs to
    // a `class_specifier` / `struct_specifier` / `union_specifier`).
    // tree-sitter-cpp's `function_definition` for in-class methods nests
    // inside `field_declaration_list`. Walk upwards looking for that pairing.
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "field_declaration_list" {
            if let Some(grandparent) = parent.parent() {
                return matches!(
                    grandparent.kind(),
                    "class_specifier" | "struct_specifier" | "union_specifier"
                );
            }
            return false;
        }
        // Stop walking once we hit a containing definition that isn't a class body.
        if matches!(
            parent.kind(),
            "namespace_definition" | "translation_unit" | "function_definition"
        ) {
            return false;
        }
        current = parent.parent();
    }
    false
}

fn has_class_definition_ancestor(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if let Some(grandparent) = parent.parent() {
        if parent.kind() == "block" && grandparent.kind() == "class_definition" {
            return true;
        }
    }

    if parent.kind() == "decorated_definition" {
        let Some(grandparent) = parent.parent() else {
            return false;
        };
        let Some(great_grandparent) = grandparent.parent() else {
            return false;
        };
        return grandparent.kind() == "block" && great_grandparent.kind() == "class_definition";
    }

    false
}

fn definition_rank(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Module => 0,
        NodeKind::Struct => 1,
        NodeKind::Class => 2,
        NodeKind::Interface => 3,
        NodeKind::Enum => 4,
        NodeKind::Trait => 5,
        NodeKind::Impl => 6,
        NodeKind::Method => 7,
        NodeKind::Function => 8,
        NodeKind::Section => 9,
        _ => 10,
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

fn child_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn scoped_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}
