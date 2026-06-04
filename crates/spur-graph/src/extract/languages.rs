use std::collections::{BTreeSet, HashMap};

use tree_sitter::{Language as TsLanguage, Node};

use crate::extract::tree_sitter::{CaptureHit, ExtractedSymbol, FactBuilder, PendingEdge};
use crate::validation::compute_anchor_hash;
use crate::{FileId, GraphEdgeKind, NodeId, NodeKind, RelationKind};

#[derive(Debug)]
pub(crate) struct LanguageConfig {
    pub(crate) language: TsLanguage,
    pub(crate) inline_language: Option<TsLanguage>,
    pub(crate) queries: &'static [(&'static str, &'static str)],
    pub(crate) definition_kind_map: &'static [(&'static str, NodeKind)],
    pub(crate) relation_kind_map: Option<&'static [(&'static str, RelationKind)]>,
    pub(crate) is_method: Option<fn(Node<'_>) -> bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Markdown,
    Cpp,
}

impl Language {
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        language_registry()
            .iter()
            .find_map(|descriptor| (descriptor.matcher)(path).then_some(descriptor.language))
    }

    pub fn tree_sitter_language(self) -> TsLanguage {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }

    pub(crate) fn config(self) -> LanguageConfig {
        match self {
            Self::Rust => rust_config(),
            Self::Python => python_config(),
            Self::TypeScript => typescript_config(),
            Self::Tsx => tsx_config(),
            Self::Markdown => markdown_config(),
            Self::Cpp => cpp_config(),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Markdown => "markdown",
            Self::Cpp => "cpp",
        }
    }
}

pub(crate) struct LanguageDescriptor {
    pub(crate) matcher: fn(&std::path::Path) -> bool,
    pub(crate) factory: fn() -> LanguageConfig,
    pub(crate) language: Language,
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
            ("definition.enum_variant", NodeKind::EnumVariant),
            ("definition.field", NodeKind::Field),
            ("definition.trait", NodeKind::Trait),
            ("definition.impl", NodeKind::Impl),
            ("definition.type_alias", NodeKind::TypeAlias),
            ("definition.constant", NodeKind::Constant),
            ("definition.macro", NodeKind::Macro),
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
            ("definition.constant", NodeKind::Constant),
        ],
        relation_kind_map: None,
        is_method: Some(has_class_definition_ancestor),
    }
}

// Shared by the plain TypeScript grammar. Must only reference nodes that exist
// in that grammar — JSX nodes are excluded here and added for TSX below.
const TYPESCRIPT_QUERIES: &[(&str, &str)] = &[
    ("tags", include_str!("../../queries/typescript/tags.scm")),
    (
        "spur-edges",
        include_str!("../../queries/typescript/spur-edges.scm"),
    ),
];

// TSX reuses the TypeScript tags/edges and adds JSX render edges. The JSX
// query references nodes that exist only in tree-sitter-typescript's TSX
// grammar, so it must never be compiled against the plain TypeScript grammar.
const TSX_QUERIES: &[(&str, &str)] = &[
    ("tags", include_str!("../../queries/typescript/tags.scm")),
    (
        "spur-edges",
        include_str!("../../queries/typescript/spur-edges.scm"),
    ),
    (
        "jsx-edges",
        include_str!("../../queries/typescript/jsx-edges.scm"),
    ),
];

const TYPESCRIPT_DEFINITION_KIND_MAP: &[(&str, NodeKind)] = &[
    ("definition.class", NodeKind::Class),
    ("definition.interface", NodeKind::Interface),
    ("definition.enum", NodeKind::Enum),
    ("definition.enum_variant", NodeKind::EnumVariant),
    ("definition.function", NodeKind::Function),
    ("definition.method", NodeKind::Method),
    ("definition.field", NodeKind::Field),
    ("definition.type_alias", NodeKind::TypeAlias),
    ("definition.constant", NodeKind::Constant),
    ("definition.module", NodeKind::Module),
];

fn typescript_config_for(
    language: TsLanguage,
    queries: &'static [(&'static str, &'static str)],
) -> LanguageConfig {
    LanguageConfig {
        language,
        inline_language: None,
        queries,
        definition_kind_map: TYPESCRIPT_DEFINITION_KIND_MAP,
        relation_kind_map: None,
        is_method: None,
    }
}

pub(crate) fn typescript_config() -> LanguageConfig {
    typescript_config_for(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        TYPESCRIPT_QUERIES,
    )
}

pub(crate) fn tsx_config() -> LanguageConfig {
    typescript_config_for(tree_sitter_typescript::LANGUAGE_TSX.into(), TSX_QUERIES)
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
            ("definition.enum_variant", NodeKind::EnumVariant),
            ("definition.function", NodeKind::Function),
            ("definition.method", NodeKind::Method),
            ("definition.type_alias", NodeKind::TypeAlias),
            ("definition.macro", NodeKind::Macro),
            ("definition.field", NodeKind::Field),
            ("definition.constant", NodeKind::Constant),
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
            language: Language::Rust,
            label: "rust",
            extensions: &["rs"],
        },
        LanguageDescriptor {
            matcher: python_matcher,
            factory: python_config,
            language: Language::Python,
            label: "python",
            extensions: &["py"],
        },
        LanguageDescriptor {
            matcher: typescript_matcher,
            factory: typescript_config,
            language: Language::TypeScript,
            label: "typescript",
            extensions: &["ts"],
        },
        LanguageDescriptor {
            matcher: tsx_matcher,
            factory: tsx_config,
            language: Language::Tsx,
            label: "tsx",
            extensions: &["tsx"],
        },
        LanguageDescriptor {
            matcher: cpp_matcher,
            factory: cpp_config,
            language: Language::Cpp,
            label: "cpp",
            extensions: CPP_EXTENSIONS,
        },
        LanguageDescriptor {
            matcher: markdown_matcher,
            factory: markdown_config,
            language: Language::Markdown,
            label: "markdown",
            extensions: &["md"],
        },
    ]
}

pub fn all_supported_extensions() -> Vec<&'static str> {
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
    emit_definitions_with_parents(
        config,
        builder,
        relative_path,
        file_id,
        file_node_id,
        source,
        captures,
        &[],
    )
}

#[expect(clippy::too_many_arguments)]
pub(crate) fn emit_definitions_with_parents<'tree>(
    config: &LanguageConfig,
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node_id: NodeId,
    source: &str,
    captures: &[CaptureHit<'tree>],
    parent_bindings: &[DefinitionBinding<'tree>],
) -> Vec<DefinitionBinding<'tree>> {
    let definitions = definition_candidates(config, source, captures);

    let mut bindings = parent_bindings.to_vec();
    let mut emitted = Vec::new();
    for (kind, node, label) in definitions {
        // Language adapters must provide an inner @name capture for every definition.
        // If a grammar edge case lacks one, skip it until the query is extended.
        let Some(label) = label else {
            continue;
        };
        let parent = nearest_parent(file_node_id, &bindings, node);
        let fqn = scoped_name(parent.fqn.unwrap_or(""), &fqn_segment(kind, &label));
        let node_id = builder.add_node(relative_path, label, fqn.clone(), kind, file_id, node);
        builder.add_edge(parent.node_id, Some(node_id), RelationKind::Contains, None);
        let binding = DefinitionBinding { node, node_id, fqn };
        bindings.push(binding.clone());
        emitted.push(binding);
    }
    emitted
}

pub(crate) fn extracted_symbols<'tree>(
    config: &LanguageConfig,
    source: &str,
    captures: &[CaptureHit<'tree>],
) -> Vec<ExtractedSymbol> {
    let definitions = definition_candidates(config, source, captures);
    let mut bindings: Vec<ExtractedDefinitionBinding<'tree>> = Vec::new();
    let mut symbols = Vec::new();

    for (kind, node, label) in definitions {
        let Some(label) = label else {
            continue;
        };
        let parent = nearest_extracted_parent(&bindings, node);
        let enclosing_scope = parent.map(|parent| parent.fqn.clone());
        let fqn = scoped_name(
            parent.map(|parent| parent.fqn.as_str()).unwrap_or(""),
            &fqn_segment(kind, &label),
        );
        let range = node.range();
        let symbol_text = child_text(node, source);
        symbols.push(ExtractedSymbol {
            entity_name: symbol_entity_name(&label),
            symbol_kind: symbol_kind(kind).to_owned(),
            enclosing_scope,
            byte_range: [range.start_byte, range.end_byte],
            line_range: [range.start_point.row + 1, range.end_point.row + 1],
            anchor_hash: compute_anchor_hash(symbol_text).to_string(),
            tokens: symbol_tokens(node, source, &label),
        });
        bindings.push(ExtractedDefinitionBinding { node, fqn });
    }

    symbols
}

pub(crate) fn emit_edges(
    config: &LanguageConfig,
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    definitions: &[DefinitionBinding<'_>],
    captures: &[CaptureHit<'_>],
) {
    let typed_bindings_by_scope = typed_bindings_by_scope(definitions, source);
    for capture in captures {
        match capture.name.as_str() {
            "import" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                let relation = relation_kind_for_capture(config, "import", RelationKind::Imports);
                for imported in contained_capture_text(capture, source, captures, "import.name") {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: imported,
                        relation,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: None,
                        scope_text: None,
                    });
                }
            }
            "implements" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for trait_name in
                    contained_capture_text(capture, source, captures, "implements.name")
                {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: trait_name,
                        relation: RelationKind::Implements,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: None,
                        scope_text: None,
                    });
                }
            }
            "extends" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for super_name in contained_capture_text(capture, source, captures, "extends.name")
                {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: super_name,
                        relation: RelationKind::Extends,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: None,
                        scope_text: None,
                    });
                }
            }
            "call" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                let receiver_text =
                    contained_capture_text(capture, source, captures, "call.receiver")
                        .into_iter()
                        .next();
                let scope_text = contained_capture_text(capture, source, captures, "call.scope")
                    .into_iter()
                    .next();
                let inferred_scope_text = receiver_text.as_deref().and_then(|receiver| {
                    receiver_scope_text(source_id, receiver, &typed_bindings_by_scope)
                });
                let scope_text = scope_text.or(inferred_scope_text);
                for callee in contained_capture_text(capture, source, captures, "call.name") {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: callee,
                        relation: RelationKind::Calls,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: receiver_text.clone(),
                        scope_text: scope_text.clone(),
                    });
                }
            }
            "jsx_call" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for callee in contained_capture_text(capture, source, captures, "jsx_call.name") {
                    // React naming rule: a JSX tag whose name begins with an
                    // uppercase letter is a component reference; lowercase tags
                    // (`div`, `span`, …) are intrinsic host elements and must
                    // not produce call edges.
                    if !callee.chars().next().is_some_and(char::is_uppercase) {
                        continue;
                    }
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: callee,
                        relation: RelationKind::Calls,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: None,
                        scope_text: None,
                    });
                }
            }
            "macro_call" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for callee in contained_capture_text(capture, source, captures, "macro_call.name") {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: callee,
                        relation: RelationKind::Calls,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::MacroBody,
                        receiver_text: None,
                        scope_text: None,
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
                    target_name: target_name.to_owned(),
                    relation: RelationKind::References,
                    edge_kind: Some(GraphEdgeKind::ReferencesHof),
                    origin: crate::extract::tree_sitter::CallOrigin::Expression,
                    receiver_text: None,
                    scope_text: None,
                });
            }
            _ => {}
        }
    }
}

fn typed_bindings_by_scope(
    definitions: &[DefinitionBinding<'_>],
    source: &str,
) -> HashMap<NodeId, HashMap<String, String>> {
    let mut bindings_by_scope = HashMap::new();
    for definition in definitions {
        let mut bindings = HashMap::new();
        collect_typed_bindings(definition.node, definition.node, source, &mut bindings);
        if !bindings.is_empty() {
            bindings_by_scope.insert(definition.node_id, bindings);
        }
    }
    bindings_by_scope
}

fn collect_typed_bindings(
    root: Node<'_>,
    node: Node<'_>,
    source: &str,
    bindings: &mut HashMap<String, String>,
) {
    if node != root && is_definition_node(node) {
        return;
    }

    if matches!(node.kind(), "parameter" | "let_declaration") {
        if let Some((name, type_text)) = typed_binding(node, source) {
            bindings.insert(name, receiver_type_scope_text(&type_text));
        }
    }

    for index in 0..node.named_child_count() {
        if let Some(child) = node.named_child(index) {
            collect_typed_bindings(root, child, source, bindings);
        }
    }
}

fn typed_binding(node: Node<'_>, source: &str) -> Option<(String, String)> {
    let pattern = node.child_by_field_name("pattern")?;
    let name = single_identifier_text(pattern, source)?;
    let type_node = node.child_by_field_name("type")?;
    Some((name, child_text(type_node, source).trim().to_owned()))
}

fn receiver_scope_text(
    source_id: NodeId,
    receiver: &str,
    typed_bindings_by_scope: &HashMap<NodeId, HashMap<String, String>>,
) -> Option<String> {
    let receiver = receiver.trim();
    if receiver == "self" {
        return Some("Self".to_owned());
    }
    typed_bindings_by_scope
        .get(&source_id)
        .and_then(|bindings| bindings.get(receiver))
        .cloned()
}

fn receiver_type_scope_text(type_text: &str) -> String {
    let mut ty = type_text.trim();
    while let Some(rest) = ty.strip_prefix('&') {
        ty = rest.trim_start();
        if let Some(rest) = ty.strip_prefix("mut ") {
            ty = rest.trim_start();
        }
        if let Some(rest) = strip_lifetime_prefix(ty) {
            ty = rest.trim_start();
            if let Some(rest) = ty.strip_prefix("mut ") {
                ty = rest.trim_start();
            }
        }
    }
    ty.to_owned()
}

fn strip_lifetime_prefix(type_text: &str) -> Option<&str> {
    let rest = type_text.strip_prefix('\'')?;
    let end = rest
        .find(char::is_whitespace)
        .map(|index| index + 1)
        .unwrap_or(type_text.len());
    Some(&type_text[end..])
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
            origin: crate::extract::tree_sitter::CallOrigin::Expression,
            receiver_text: Some(receiver),
            scope_text: None,
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
    Some(primary.to_owned())
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
        child_text(receiver, source).trim().to_owned(),
        child_text(method, source).trim().to_owned(),
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
        return Some(child_text(node, source).trim().to_owned());
    }

    let identifiers = named_descendants_by_kind(node, "identifier");
    match identifiers.as_slice() {
        [identifier] => Some(child_text(*identifier, source).trim().to_owned()),
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

fn definition_candidates<'tree>(
    config: &LanguageConfig,
    source: &str,
    captures: &[CaptureHit<'tree>],
) -> Vec<(NodeKind, Node<'tree>, Option<String>)> {
    let mut definitions: Vec<_> = captures
        .iter()
        .filter_map(|capture| {
            definition_kind(config, capture.name.as_str(), capture.node).map(|kind| {
                (
                    kind,
                    capture.node,
                    definition_name(kind, capture, source, captures),
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
    definitions
}

fn definition_name(
    kind: NodeKind,
    definition: &CaptureHit<'_>,
    source: &str,
    captures: &[CaptureHit<'_>],
) -> Option<String> {
    if kind == NodeKind::Impl {
        let self_type = contained_capture_text(definition, source, captures, "impl.self")
            .into_iter()
            .next()?;
        let trait_type = contained_capture_text(definition, source, captures, "impl.trait")
            .into_iter()
            .next();
        return Some(match trait_type {
            Some(trait_type) => format!("{trait_type} for {self_type}"),
            None => self_type,
        });
    }

    contained_capture_text(definition, source, captures, "name")
        .into_iter()
        .next()
}

fn nearest_parent<'a>(
    file_node_id: NodeId,
    definitions: &'a [DefinitionBinding<'_>],
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
        NodeKind::EnumVariant => 5,
        NodeKind::Trait => 6,
        NodeKind::Impl => 7,
        NodeKind::Method => 8,
        NodeKind::Function => 9,
        NodeKind::Section => 10,
        _ => 10,
    }
}

#[derive(Debug, Clone)]
struct ExtractedDefinitionBinding<'tree> {
    node: Node<'tree>,
    fqn: String,
}

fn nearest_extracted_parent<'a, 'tree>(
    definitions: &'a [ExtractedDefinitionBinding<'tree>],
    node: Node<'_>,
) -> Option<&'a ExtractedDefinitionBinding<'tree>> {
    definitions
        .iter()
        .rev()
        .find(|definition| contains(definition.node, node))
}

fn symbol_kind(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::Function => "function",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Struct => "struct",
        NodeKind::Impl => "impl",
        NodeKind::Trait => "trait",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Method => "method",
        NodeKind::Field => "field",
        NodeKind::Constant => "constant",
        NodeKind::TypeAlias => "type_alias",
        NodeKind::Macro => "macro",
        NodeKind::Section => "section",
        _ => "symbol",
    }
}

fn symbol_entity_name(label: &str) -> String {
    label.strip_prefix("impl ").unwrap_or(label).to_owned()
}

fn fqn_segment(kind: NodeKind, label: &str) -> String {
    match kind {
        NodeKind::Impl => format!("impl {}", symbol_entity_name(label)),
        _ => symbol_entity_name(label),
    }
}

fn symbol_tokens(node: Node<'_>, source: &str, own_name: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    collect_symbol_tokens(node, source, own_name, &mut tokens);
    tokens.into_iter().collect()
}

fn collect_symbol_tokens(
    node: Node<'_>,
    source: &str,
    own_name: &str,
    tokens: &mut BTreeSet<String>,
) {
    let kind = node.kind();
    if is_literal_kind(kind) || (node.named_child_count() == 0 && is_identifier_kind(kind)) {
        let text = child_text(node, source).trim();
        if !text.is_empty() && text != own_name {
            tokens.insert(text.to_owned());
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbol_tokens(child, source, own_name, tokens);
    }
}

fn is_identifier_kind(kind: &str) -> bool {
    kind == "identifier" || kind.contains("identifier") || kind == "primitive_type"
}

fn is_literal_kind(kind: &str) -> bool {
    kind.contains("literal")
        || matches!(
            kind,
            "string" | "integer" | "float" | "number" | "true" | "false" | "null" | "none" | "nil"
        )
}

#[cfg(test)]
mod gate_contract {
    use std::collections::{BTreeMap, BTreeSet};

    use tree_sitter::Query;

    use super::*;

    #[test]
    fn every_registered_language_satisfies_query_contract() {
        assert_registry_extensions_are_unique();

        for descriptor in language_registry() {
            let language = descriptor.language;
            let config = (descriptor.factory)();
            let label = language.label();

            let tags_source = config
                .queries
                .iter()
                .find_map(|(name, source)| (*name == "tags").then_some(*source))
                .unwrap_or_else(|| panic!("{label}: missing `tags` query"));
            assert!(
                !tags_source.trim().is_empty(),
                "{label}: `tags` query source must not be empty"
            );

            let definition_captures = compiled_definition_captures(label, &config);
            let mapped_definitions: BTreeSet<_> = config
                .definition_kind_map
                .iter()
                .map(|(capture_name, _)| (*capture_name).to_owned())
                .collect();

            assert!(
                !mapped_definitions.is_empty(),
                "{label}: definition_kind_map must not be empty"
            );

            let orphan_captures: BTreeSet<_> = definition_captures
                .difference(&mapped_definitions)
                .cloned()
                .collect();
            assert!(
                orphan_captures.is_empty(),
                "{label}: @definition.* captures missing from definition_kind_map: {orphan_captures:?}"
            );

            let unused_mappings: BTreeSet<_> = mapped_definitions
                .difference(&definition_captures)
                .cloned()
                .collect();
            assert!(
                unused_mappings.is_empty(),
                "{label}: definition_kind_map entries missing from compiled queries: {unused_mappings:?}"
            );

            for (capture_name, kind) in config.definition_kind_map {
                assert_ne!(
                    symbol_kind(*kind),
                    "symbol",
                    "{label}: `{capture_name}` maps to {kind:?}, but symbol_kind() falls back to `symbol`"
                );
            }

            let expected_definitions: BTreeSet<_> = expected_definition_captures(language)
                .iter()
                .map(|capture_name| (*capture_name).to_owned())
                .collect();
            let missing_expected: BTreeSet<_> = expected_definitions
                .difference(&definition_captures)
                .cloned()
                .collect();
            assert!(
                missing_expected.is_empty(),
                "{label}: expected definition captures missing from compiled queries: {missing_expected:?}"
            );
        }
    }

    fn assert_registry_extensions_are_unique() {
        let mut owners: BTreeMap<&str, Language> = BTreeMap::new();
        for descriptor in language_registry() {
            assert!(
                !descriptor.extensions.is_empty(),
                "{}: registry entry must declare at least one extension",
                descriptor.label
            );
            for extension in descriptor.extensions {
                assert!(
                    !extension.is_empty(),
                    "{}: registry extension must not be empty",
                    descriptor.label
                );
                if let Some(previous) = owners.insert(*extension, descriptor.language) {
                    assert_eq!(
                        previous,
                        descriptor.language,
                        "extension `{extension}` is registered for both {} and {}",
                        previous.label(),
                        descriptor.language.label()
                    );
                }
            }
        }
    }

    fn compiled_definition_captures(
        language_label: &str,
        config: &LanguageConfig,
    ) -> BTreeSet<String> {
        let mut captures = BTreeSet::new();
        for (name, source) in config.queries {
            let query_language = query_language_for(config, name, language_label);
            let query = Query::new(query_language, source).unwrap_or_else(|err| {
                panic!("failed to compile tree-sitter query `{name}` for `{language_label}`: {err}")
            });
            captures.extend(
                query
                    .capture_names()
                    .iter()
                    .filter(|capture_name| capture_name.starts_with("definition."))
                    .map(|capture_name| (*capture_name).to_owned()),
            );
        }
        captures
    }

    fn query_language_for<'a>(
        config: &'a LanguageConfig,
        query_name: &str,
        language_label: &str,
    ) -> &'a TsLanguage {
        match query_name {
            "inline-spur-edges" => config.inline_language.as_ref().unwrap_or_else(|| {
                panic!("{language_label}: query `inline-spur-edges` requires an inline language")
            }),
            _ => &config.language,
        }
    }

    fn expected_definition_captures(language: Language) -> &'static [&'static str] {
        match language {
            Language::Rust => &[
                "definition.module",
                "definition.function",
                "definition.method",
                "definition.struct",
                "definition.enum",
                "definition.enum_variant",
                "definition.field",
                "definition.trait",
                "definition.impl",
                "definition.type_alias",
                "definition.constant",
                "definition.macro",
            ],
            Language::Python => &[
                "definition.function",
                "definition.class",
                "definition.constant",
            ],
            Language::TypeScript | Language::Tsx => &[
                "definition.class",
                "definition.interface",
                "definition.enum",
                "definition.enum_variant",
                "definition.function",
                "definition.method",
                "definition.field",
                "definition.type_alias",
                "definition.constant",
                "definition.module",
            ],
            Language::Cpp => &[
                "definition.module",
                "definition.class",
                "definition.struct",
                "definition.enum",
                "definition.enum_variant",
                "definition.function",
                "definition.method",
                "definition.type_alias",
                "definition.macro",
                "definition.field",
                "definition.constant",
            ],
            Language::Markdown => &["definition.section"],
        }
    }
}

fn contained_capture_text(
    parent: &CaptureHit<'_>,
    source: &str,
    captures: &[CaptureHit<'_>],
    capture_name: &str,
) -> Vec<String> {
    captures
        .iter()
        .filter(|capture| {
            capture.name == capture_name
                && capture.pattern_index == parent.pattern_index
                && capture.match_index == parent.match_index
        })
        .map(|capture| child_text(capture.node, source).trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

fn child_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn scoped_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}::{name}")
    }
}
