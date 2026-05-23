use std::path::Path;

use spur_graph::extract::languages::Language;
use spur_graph::extract::tree_sitter::{BytesExtractor, ExtractError};

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
