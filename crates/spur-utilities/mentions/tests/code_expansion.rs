//! sud-m4 code-feature expansion validation tests (RED-first).
//!
//! These pin the public contract of `spur_mentions::code::expansion` — the
//! module the 2026-08-27 mentions spec §"Shared crate" test matrix names:
//! "Expansion validation for missing files, changed symbol ranges, caps,
//! and warnings." They were written and run red against the pre-move tree
//! (the module did not exist behind the `code` feature yet) and drive the
//! relocation from `spur-tui` (decoupling spec §4.5).
//!
//! The file compiles empty without the `code` feature: the notebook's
//! `default-features = false` build must never see the code module
//! (feature-off gates: `clippy -p spur-mentions --no-default-features` and
//! `test -p spur-mentions --no-default-features`).

#![cfg(feature = "code")]

use std::fs;
use std::path::Path;

use spur_graph::validation::compute_anchor_hash;
use spur_graph::{
    CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints, CodeMentionKind,
    CodeMentionPayload, CodeMentionValidationSpec,
};
use spur_mentions::code::expansion::{
    expand, ExpandedMention, ReplacedWith, CONTEXT_HEADER_CAP_BYTES, PER_PROMPT_CAP_BYTES,
};

/// Public cap contract consumed by the TUI submit router (byte budgets for
/// one prompt's mention bodies and each symbol's context header).
#[test]
fn caps_are_part_of_the_public_contract() {
    assert_eq!(PER_PROMPT_CAP_BYTES, 32 * 1024);
    assert_eq!(CONTEXT_HEADER_CAP_BYTES, 1500);
}

/// A symbol whose top-of-file context exceeds the header cap must be
/// truncated at a line boundary inside the cap with the marker line —
/// never past the cap, never mid-marker.
#[test]
fn context_header_truncates_to_cap_with_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let use_block = (0..40)
        .map(|index| format!("use alpha_{index:02}::module_{index:02}::deeply_nested_path;"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!("{use_block}\n\npub fn big_context() {{}}\n");
    write_source(dir.path(), "src/lib.rs", &source);
    let payload = symbol_payload_from_source(
        "@big_context",
        "graph://symbol/symbol-big-context",
        "src/lib.rs",
        &source,
        "pub fn big_context",
        "\n",
        "big_context",
        "fn",
        [3, 3 + use_block.lines().count() + 3],
    );

    let ExpandedMention::Body { text } = expand(&payload, dir.path()) else {
        panic!("expected body expansion");
    };

    let header = text
        .split("context_header:\n")
        .nth(1)
        .unwrap_or_else(|| panic!("expected a context header in:\n{text}"));
    assert!(
        header.len() <= CONTEXT_HEADER_CAP_BYTES,
        "header is {} bytes, cap is {CONTEXT_HEADER_CAP_BYTES}",
        header.len()
    );
    assert!(
        header.contains("use alpha_00::"),
        "truncation must keep the leading context lines:\n{header}"
    );
    assert_eq!(
        header.matches("# … context truncated").count(),
        1,
        "exactly one truncation marker expected:\n{header}"
    );
    assert!(
        header.ends_with("# … context truncated\n"),
        "header must end with the marker line:\n{header}"
    );
}

/// A file mention whose file no longer exists warns and is dropped (no
/// file-mention replacement is possible without the file).
#[test]
fn file_mention_with_missing_file_warns_and_drops() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = file_payload("src/missing.rs", "graph://file/file-missing");

    let ExpandedMention::Warning {
        text,
        replaced_with,
    } = expand(&payload, dir.path())
    else {
        panic!("expected warning expansion");
    };

    assert_eq!(replaced_with, ReplacedWith::Dropped);
    assert!(text.contains("MENTION_WARNING src/missing.rs"), "{text}");
    assert!(text.contains("failure_reason: file_missing"), "{text}");
    assert!(text.contains("replaced_with:  dropped"), "{text}");
    assert!(!text.contains("kind: file\n"), "{text}");
}

/// A symbol whose anchor hash no longer matches (the file changed under a
/// stale range) warns and degrades to the file mention, embedding the
/// replacement body.
#[test]
fn changed_symbol_range_warns_and_replaces_with_file_mention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "pub fn run() {}\n";
    write_source(dir.path(), "src/lib.rs", source);
    let payload = symbol_payload(
        "@run",
        "graph://symbol/symbol-run",
        "src/lib.rs",
        [0, source.len()],
        [1, 1],
        "run",
        "fn",
        compute_anchor_hash(source).wrapping_add(1),
    );

    let ExpandedMention::Warning {
        text,
        replaced_with,
    } = expand(&payload, dir.path())
    else {
        panic!("expected warning expansion");
    };

    assert_eq!(replaced_with, ReplacedWith::FileMention);
    assert!(
        text.contains("failure_reason: anchor_hash_mismatch"),
        "{text}"
    );
    assert!(text.contains("replaced_with:  file_mention"), "{text}");
    assert!(text.contains("MENTION src/lib.rs\nkind: file"), "{text}");
}

fn write_source(root: &Path, relative: &str, source: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, source).expect("write source");
}

fn file_payload(file_path: &str, uri: &str) -> CodeMentionPayload {
    CodeMentionPayload {
        authoritative: CodeMentionAuthoritative {
            display: file_path.to_owned(),
            uri: uri.to_owned(),
            kind: CodeMentionKind::File,
            file_path: file_path.to_owned(),
            validation: CodeMentionValidationSpec::FileExists {
                path: file_path.to_owned(),
            },
        },
        extraction_hints: CodeMentionExtractionHints {
            line_range: None,
            byte_range: None,
            symbol_kind: None,
            entity_name: None,
            qualified_name: String::new(),
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: "test-version".to_owned(),
        },
    }
}

#[expect(clippy::too_many_arguments)]
fn symbol_payload(
    display: &str,
    uri: &str,
    file_path: &str,
    byte_range: [usize; 2],
    line_range: [usize; 2],
    entity_name: &str,
    symbol_kind: &str,
    anchor_hash: u64,
) -> CodeMentionPayload {
    CodeMentionPayload {
        authoritative: CodeMentionAuthoritative {
            display: display.to_owned(),
            uri: uri.to_owned(),
            kind: CodeMentionKind::Symbol,
            file_path: file_path.to_owned(),
            validation: CodeMentionValidationSpec::SymbolRange {
                path: file_path.to_owned(),
                line_range,
                byte_range,
                entity_name: entity_name.to_owned(),
                anchor_hash: anchor_hash.to_string(),
            },
        },
        extraction_hints: CodeMentionExtractionHints {
            line_range: Some(line_range),
            byte_range: Some(byte_range),
            symbol_kind: Some(symbol_kind.to_owned()),
            entity_name: Some(entity_name.to_owned()),
            qualified_name: String::new(),
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: "test-version".to_owned(),
        },
    }
}

#[expect(clippy::too_many_arguments)]
fn symbol_payload_from_source(
    display: &str,
    uri: &str,
    file_path: &str,
    source: &str,
    start_pattern: &str,
    end_pattern: &str,
    entity_name: &str,
    symbol_kind: &str,
    line_range: [usize; 2],
) -> CodeMentionPayload {
    let start = source.find(start_pattern).expect("start pattern");
    let end = if end_pattern == "\n" {
        source.len()
    } else {
        source[start..].find(end_pattern).expect("end pattern") + start
    };
    symbol_payload(
        display,
        uri,
        file_path,
        [start, end],
        line_range,
        entity_name,
        symbol_kind,
        compute_anchor_hash(&source[start..end]),
    )
}
