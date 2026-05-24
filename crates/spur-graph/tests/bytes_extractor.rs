use std::path::Path;

use pretty_assertions::assert_eq;
use spur_graph::extract::languages::Language;
use spur_graph::extract::tree_sitter::{BytesExtractor, ExtractError, ExtractedSymbol};
use spur_graph::store::build::artifact_from_facts;
use spur_graph::{build_facts, GraphSymbolArtifact};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SymbolKey {
    symbol_kind: String,
    entity_name: String,
    enclosing_scope: Option<String>,
    line_range: [usize; 2],
}

impl From<&ExtractedSymbol> for SymbolKey {
    fn from(symbol: &ExtractedSymbol) -> Self {
        Self {
            symbol_kind: symbol.symbol_kind.clone(),
            entity_name: symbol.entity_name.clone(),
            enclosing_scope: symbol.enclosing_scope.clone(),
            line_range: symbol.line_range,
        }
    }
}

impl From<&GraphSymbolArtifact> for SymbolKey {
    fn from(symbol: &GraphSymbolArtifact) -> Self {
        Self {
            symbol_kind: symbol.symbol_kind.clone(),
            entity_name: symbol.entity_name.clone(),
            enclosing_scope: symbol.enclosing_scope.clone(),
            line_range: symbol.line_range,
        }
    }
}

fn snapshot_symbol_keys(
    language: Language,
    logical_path: &Path,
    bytes: &[u8],
) -> std::collections::BTreeSet<SymbolKey> {
    let mut extractor = BytesExtractor::for_language(language).expect("create extractor");
    extractor
        .extract(logical_path, bytes)
        .expect("extract snapshot symbols")
        .iter()
        .map(SymbolKey::from)
        .collect()
}

fn structural_symbol_keys(root: &Path, file_path: &str) -> std::collections::BTreeSet<SymbolKey> {
    let facts = build_facts(root).expect("extract graph facts").0;
    let artifact = artifact_from_facts(&facts, root).expect("artifact");
    artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.file_path == file_path)
        .map(SymbolKey::from)
        .collect()
}

#[test]
fn extracts_rust_function_from_bytes() {
    let mut extractor = BytesExtractor::for_language(Language::Rust).unwrap();
    let bytes = b"pub fn hello() -> i32 { 42 }\n";

    let symbols = extractor
        .extract(Path::new("src/lib.rs"), bytes)
        .expect("extract");

    assert_eq!(symbols.len(), 1);
    let symbol = &symbols[0];
    assert_eq!(symbol.entity_name, "hello");
    assert_eq!(symbol.symbol_kind, "function");
}

#[test]
fn reusing_extractor_across_blobs_works() {
    let mut extractor = BytesExtractor::for_language(Language::Rust).unwrap();

    let a = extractor
        .extract(Path::new("a.rs"), b"fn a() {}\n")
        .unwrap();
    let b = extractor
        .extract(Path::new("b.rs"), b"fn b() {}\n")
        .unwrap();

    assert_eq!(a[0].entity_name, "a");
    assert_eq!(b[0].entity_name, "b");
}

#[test]
fn invalid_utf8_returns_error_not_panic() {
    let mut extractor = BytesExtractor::for_language(Language::Rust).unwrap();
    let bytes = &[0xff, 0xfe, 0xfd];

    let result = extractor.extract(Path::new("x.rs"), bytes);

    assert!(matches!(result, Err(ExtractError::InvalidUtf8(_))));
}

#[test]
fn extractor_returns_err_on_invalid_tree_sitter_input() {
    let mut extractor = BytesExtractor::for_language(Language::Rust).unwrap();
    let corrupt_blob: Vec<u8> = (0..4096)
        .map(|index| if index % 2 == 0 { 0xff } else { b'a' })
        .collect();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extractor.extract(Path::new("src/corrupt.rs"), &corrupt_blob)
    }));

    assert!(result.is_ok(), "corrupt input must not panic");
    assert!(matches!(result.unwrap(), Err(ExtractError::InvalidUtf8(_))));
}

#[test]
fn extracted_symbol_tokens_include_leaf_identifiers_and_literals_except_own_name() {
    let mut extractor = BytesExtractor::for_language(Language::Rust).unwrap();
    let bytes = b"fn hello(user_id: i32) -> i32 { let answer = 42; user_id + answer }\n";

    let symbols = extractor
        .extract(Path::new("src/lib.rs"), bytes)
        .expect("extract");

    let tokens = &symbols[0].tokens;
    assert!(tokens.contains(&"user_id".to_string()));
    assert!(tokens.contains(&"answer".to_string()));
    assert!(tokens.contains(&"i32".to_string()));
    assert!(tokens.contains(&"42".to_string()));
    assert!(!tokens.contains(&"hello".to_string()));
}

#[test]
fn snapshot_symbol_captures_match_structural_tags_for_signature_methods() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");

    let rust_source = br#"
pub trait Runner {
    fn run(&self);
}

pub struct App;

impl App {
    pub fn run(&self) {}
}
"#;
    std::fs::write(root.join("src/lib.rs"), rust_source).expect("write rust fixture");

    let typescript_source = br#"
export interface Runner {
  run(): void;
}

export class App implements Runner {
  run(): void {}
}
"#;
    std::fs::write(root.join("src/app.ts"), typescript_source).expect("write typescript fixture");

    let rust_structural = structural_symbol_keys(root, "src/lib.rs");
    let rust_snapshot = snapshot_symbol_keys(Language::Rust, Path::new("src/lib.rs"), rust_source);
    assert_eq!(rust_snapshot, rust_structural);
    assert!(
        rust_snapshot.contains(&SymbolKey {
            symbol_kind: "method".to_string(),
            entity_name: "run".to_string(),
            enclosing_scope: Some("Runner".to_string()),
            line_range: [3, 3],
        }),
        "rust trait function signatures must be present in snapshots"
    );

    let typescript_structural = structural_symbol_keys(root, "src/app.ts");
    let typescript_snapshot = snapshot_symbol_keys(
        Language::TypeScript,
        Path::new("src/app.ts"),
        typescript_source,
    );
    assert_eq!(typescript_snapshot, typescript_structural);
    assert!(
        typescript_snapshot.contains(&SymbolKey {
            symbol_kind: "method".to_string(),
            entity_name: "run".to_string(),
            enclosing_scope: Some("Runner".to_string()),
            line_range: [3, 3],
        }),
        "typescript interface method signatures must be present in snapshots"
    );
}
