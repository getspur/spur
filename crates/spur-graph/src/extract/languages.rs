use std::collections::BTreeSet;

use tree_sitter::{Language as TsLanguage, Node};

use crate::extract::tree_sitter::{CaptureHit, ExtractedSymbol, FactBuilder, PendingEdge};
use crate::validation::compute_anchor_hash;
use crate::{FileId, NodeId, NodeKind, RelationKind};

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
}

impl Language {
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        language_registry()
            .iter()
            .find_map(|descriptor| (descriptor.matcher)(path).then_some(descriptor.language))
    }

    pub fn tree_sitter_language(self) -> TsLanguage {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),
        }
    }

    pub(crate) fn config(self) -> LanguageConfig {
        match self {
            Language::Rust => rust_config(),
            Language::Python => python_config(),
            Language::TypeScript => typescript_config(),
            Language::Tsx => tsx_config(),
            Language::Markdown => markdown_config(),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Markdown => "markdown",
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
    node: Node<'tree>,
    node_id: NodeId,
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

fn typescript_config_for(language: TsLanguage) -> LanguageConfig {
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
            matcher: markdown_matcher,
            factory: markdown_config,
            language: Language::Markdown,
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
    let definitions = definition_candidates(config, source, captures);

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
        let enclosing_scope = parent.map(extracted_enclosing_scope);
        let fqn = scoped_name(
            parent.map(|parent| parent.fqn.as_str()).unwrap_or(""),
            &label,
        );
        let range = node.range();
        let symbol_text = child_text(node, source);
        symbols.push(ExtractedSymbol {
            entity_name: symbol_entity_name(&label),
            symbol_kind: symbol_kind(kind).to_string(),
            enclosing_scope,
            byte_range: [range.start_byte, range.end_byte],
            line_range: [range.start_point.row + 1, range.end_point.row + 1],
            anchor_hash: compute_anchor_hash(symbol_text).to_string(),
            tokens: symbol_tokens(node, source, &label),
        });
        bindings.push(ExtractedDefinitionBinding {
            node,
            label,
            kind,
            fqn,
        });
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
                    });
                }
            }
            _ => {}
        }
    }
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
    definitions
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

#[derive(Debug, Clone)]
struct ExtractedDefinitionBinding<'tree> {
    node: Node<'tree>,
    label: String,
    kind: NodeKind,
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

fn extracted_enclosing_scope(parent: &ExtractedDefinitionBinding<'_>) -> String {
    match parent.kind {
        NodeKind::Impl => format!("impl {}", parent.label),
        _ => parent.label.clone(),
    }
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
        NodeKind::Method => "method",
        NodeKind::TypeAlias => "type_alias",
        NodeKind::Section => "section",
        _ => "symbol",
    }
}

fn symbol_entity_name(label: &str) -> String {
    label.strip_prefix("impl ").unwrap_or(label).to_string()
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
            tokens.insert(text.to_string());
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
