use std::fs;

use spur_acp::ContentBlock;
use spur_graph::validation::compute_anchor_hash;
use spur_graph::{CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints};
use spur_tui::commands::submit_router::assemble_blocks_with_code_mentions;
use spur_tui::components::input_bar::{ProtectedRange, RangeKind};
use spur_tui::mentions::code_graph::expansion::{expand, ExpandedMention};
use spur_tui::mentions::{CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec};

#[test]
fn symbol_expansion_uses_bare_name_without_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = r#"#![allow(dead_code)]
use std::fmt;
use crate::model::Config;

pub struct GraphEngine;

impl GraphEngine {
    pub fn run(&self) {
        println!("run");
    }
}
"#;
    write_source(dir.path(), "src/lib.rs", source);
    let payload = symbol_payload_from_source(
        "run",
        "graph://symbol/symbol-run",
        "src/lib.rs",
        source,
        "    pub fn run",
        "    }\n}",
        "run",
        "fn",
        [8, 10],
    );

    let ExpandedMention::Body { text } = expand(&payload, dir.path()) else {
        panic!("expected body expansion");
    };

    assert!(text.contains("MENTION run"), "{text}");
    assert!(text.contains("kind:    symbol:fn"), "{text}");
    assert!(
        text.contains("id:      graph://symbol/symbol-run"),
        "{text}"
    );
    assert!(text.contains("file:    src/lib.rs"), "{text}");
    assert!(text.contains("lines:   8-10"), "{text}");
    assert!(text.contains("graph_index_version: test-version"), "{text}");
    assert!(
        text.contains(
            "context_header:\n#![allow(dead_code)]\nuse std::fmt;\nuse crate::model::Config;\n\nimpl GraphEngine {"
        ),
        "{text}"
    );
    assert!(!text.contains("source:\n"), "{text}");
    assert!(!text.contains("topology_available_via_mcp"), "{text}");
}

#[test]
fn symbol_expansion_uses_qualified_name_with_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "impl GraphEngine {\n    pub fn run(&self) {}\n}\n";
    write_source(dir.path(), "src/lib.rs", source);
    let mut payload = symbol_payload_from_source(
        "run",
        "graph://symbol/symbol-run",
        "src/lib.rs",
        source,
        "pub fn run",
        "\n",
        "run",
        "fn",
        [2, 2],
    );
    payload.display_meta.enclosing_scope = Some("GraphEngine".to_string());

    let ExpandedMention::Body { text } = expand(&payload, dir.path()) else {
        panic!("expected body expansion");
    };

    assert!(text.starts_with("MENTION GraphEngine::run\n"), "{text}");
    assert_eq!(payload.authoritative.display, "run");
}

#[test]
fn context_header_end_truncates_at_utf8_boundary_with_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut source = String::new();
    source.push_str("#![allow(dead_code)]\n");
    for i in 0..90 {
        source.push_str(&format!("use crate::module_{i}::Type{i}; // café\n"));
    }
    source.push_str("\npub fn target() {}\n");
    write_source(dir.path(), "src/lib.rs", &source);
    let payload = symbol_payload_from_source(
        "@target",
        "graph://symbol/symbol-target",
        "src/lib.rs",
        &source,
        "pub fn target",
        "\n",
        "target",
        "fn",
        [93, 93],
    );

    let ExpandedMention::Body { text } = expand(&payload, dir.path()) else {
        panic!("expected body expansion");
    };
    let header = text
        .split("context_header:\n")
        .nth(1)
        .expect("context header");

    assert!(header.len() <= 1500, "header too large: {}", header.len());
    assert!(header.contains("# … context truncated"), "{header:?}");
    assert!(std::str::from_utf8(header.as_bytes()).is_ok());
}

#[test]
fn prompt_assembly_emits_topology_hint_once_for_multiple_symbols() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_a = "pub fn first() {}\n";
    let source_b = "pub fn second() {}\n";
    write_source(dir.path(), "src/first.rs", source_a);
    write_source(dir.path(), "src/second.rs", source_b);
    let payloads = [
        symbol_payload_from_source(
            "first",
            "graph://symbol/symbol-first",
            "src/first.rs",
            source_a,
            "pub fn first",
            "\n",
            "first",
            "fn",
            [1, 1],
        ),
        symbol_payload_from_source(
            "second",
            "graph://symbol/symbol-second",
            "src/second.rs",
            source_b,
            "pub fn second",
            "\n",
            "second",
            "fn",
            [1, 1],
        ),
    ];
    let text = "@first @second";
    let ranges = [
        ProtectedRange {
            start: 0,
            end: "@first".len(),
            kind: RangeKind::Atom,
            uri: "graph://symbol/symbol-first".to_string(),
            name: "first".to_string(),
        },
        ProtectedRange {
            start: "@first ".len(),
            end: text.len(),
            kind: RangeKind::Atom,
            uri: "graph://symbol/symbol-second".to_string(),
            name: "second".to_string(),
        },
    ];

    let blocks = assemble_blocks_with_code_mentions(text, &ranges, &[], dir.path(), |uri| {
        payloads
            .iter()
            .find(|payload| payload.authoritative.uri == uri)
    });
    let flattened = flatten_text_blocks(&blocks);

    assert_eq!(
        count_occurrences(&flattened, "MENTION first"),
        1,
        "{flattened}"
    );
    assert_eq!(
        count_occurrences(&flattened, "MENTION second"),
        1,
        "{flattened}"
    );
    assert_eq!(
        count_occurrences(
            &flattened,
            "topology_available_via_mcp_for_above_symbols: get_callers / get_callees / get_subgraph (radius=1)"
        ),
        1,
        "{flattened}"
    );
    assert!(
        !flattened.contains("topology_available_via_mcp:\n- get_callers"),
        "{flattened}"
    );
}

#[test]
fn prompt_assembly_skips_topology_hint_for_file_only_mentions() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_source(dir.path(), "src/lib.rs", "pub fn run() {}\n");
    let payload = file_payload("src/lib.rs", "graph://file/file-lib", "src/lib.rs");
    let text = "@src/lib.rs";
    let ranges = [ProtectedRange {
        start: 0,
        end: text.len(),
        kind: RangeKind::Atom,
        uri: "graph://file/file-lib".to_string(),
        name: "src/lib.rs".to_string(),
    }];

    let blocks = assemble_blocks_with_code_mentions(text, &ranges, &[], dir.path(), |uri| {
        (uri == payload.authoritative.uri).then_some(&payload)
    });
    let flattened = flatten_text_blocks(&blocks);

    assert!(flattened.contains("MENTION src/lib.rs"), "{flattened}");
    assert!(
        !flattened.contains("topology_available_via_mcp"),
        "{flattened}"
    );
}

#[test]
fn degraded_symbol_mentions_preserve_all_validation_failure_reasons() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "pub fn run() {}\n";
    write_source(dir.path(), "src/lib.rs", source);
    let cases = [(
        symbol_payload(
            "@missing",
            "graph://symbol/symbol-missing",
            "src/missing.rs",
            [0, 10],
            [1, 1],
            "missing",
            "fn",
            0,
        ),
        "file_missing",
        "dropped",
    )];

    for (payload, reason, replacement) in cases {
        let ExpandedMention::Warning { text, .. } = expand(&payload, dir.path()) else {
            panic!("expected warning for {reason}");
        };
        assert!(
            text.contains(&format!("failure_reason: {reason}")),
            "{text}"
        );
        assert!(
            text.contains(&format!("replaced_with:  {replacement}")),
            "{text}"
        );
    }
}

#[test]
fn file_mentions_expand_to_header_only_full_file_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_source(dir.path(), "src/lib.rs", "pub fn run() {}\n");
    let payload = file_payload("@src/lib.rs", "graph://file/file-lib", "src/lib.rs");

    let ExpandedMention::Body { text } = expand(&payload, dir.path()) else {
        panic!("expected file expansion");
    };

    assert_eq!(
        text,
        "MENTION @src/lib.rs\nkind: file\nid:   graph://file/file-lib\nfile: src/lib.rs\nlines: full\n"
    );
}

#[test]
fn prompt_assembly_omits_excess_code_mentions_after_prompt_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut payloads = Vec::new();
    let mut text = String::new();
    let mut ranges = Vec::new();

    for i in 0..300 {
        let name = format!("@big{i}");
        let uri = format!("graph://symbol/symbol-big-{i}");
        let source = format!("pub fn big{i}() {{\n    work();\n}}\n");
        let path = format!("src/big{i}.rs");
        write_source(dir.path(), &path, &source);
        payloads.push(symbol_payload_from_source(
            &name,
            &uri,
            &path,
            &source,
            &format!("pub fn big{i}"),
            "\n",
            &format!("big{i}"),
            "fn",
            [1, 3],
        ));
        let start = text.len();
        text.push_str(&name);
        ranges.push(ProtectedRange {
            start,
            end: text.len(),
            kind: RangeKind::Atom,
            uri,
            name: name.trim_start_matches('@').to_string(),
        });
        text.push(' ');
    }

    let blocks = assemble_blocks_with_code_mentions(&text, &ranges, &[], dir.path(), |uri| {
        payloads
            .iter()
            .find(|payload| payload.authoritative.uri == uri)
    });
    let flattened = blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.as_str(),
            other => panic!("expected text-only code expansion, got {other:?}"),
        })
        .collect::<String>();

    assert!(
        flattened.contains("MENTION_OMITTED graph://symbol/symbol-big-"),
        "{flattened}"
    );
}

#[test]
fn prompt_assembly_warns_when_code_payload_missing_from_registry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text = "@Missing";
    let ranges = [ProtectedRange {
        start: 0,
        end: text.len(),
        kind: RangeKind::Atom,
        uri: "graph://symbol/missing".to_string(),
        name: "Missing".to_string(),
    }];

    let blocks = assemble_blocks_with_code_mentions(text, &ranges, &[], dir.path(), |_| None);

    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let ContentBlock::Text(text) = &blocks[0] else {
        panic!("expected synthetic warning text block, got {:?}", blocks[0]);
    };
    assert!(text.text.contains("MENTION_WARNING Missing"), "{text:?}");
    assert!(
        text.text.contains("intended_uri:   graph://symbol/missing"),
        "{text:?}"
    );
    assert!(
        text.text
            .contains("failure_reason: payload_not_in_registry"),
        "{text:?}"
    );
    assert!(text.text.contains("replaced_with:  dropped"), "{text:?}");
}

fn write_source(root: &std::path::Path, relative: &str, source: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, source).expect("write source");
}

fn flatten_text_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.as_str(),
            other => panic!("expected text-only code expansion, got {other:?}"),
        })
        .collect::<String>()
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn file_payload(display: &str, uri: &str, file_path: &str) -> CodeMentionPayload {
    CodeMentionPayload {
        authoritative: CodeMentionAuthoritative {
            display: display.to_string(),
            uri: uri.to_string(),
            kind: CodeMentionKind::File,
            file_path: file_path.to_string(),
            validation: CodeMentionValidationSpec::FileExists {
                path: file_path.to_string(),
            },
        },
        extraction_hints: CodeMentionExtractionHints {
            line_range: None,
            byte_range: None,
            symbol_kind: None,
            entity_name: None,
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: "test-version".to_string(),
        },
    }
}

#[allow(clippy::too_many_arguments)]
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
    let slice = &source[start..end];
    symbol_payload(
        display,
        uri,
        file_path,
        [start, end],
        line_range,
        entity_name,
        symbol_kind,
        compute_anchor_hash(slice),
    )
}

#[allow(clippy::too_many_arguments)]
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
            display: display.to_string(),
            uri: uri.to_string(),
            kind: CodeMentionKind::Symbol,
            file_path: file_path.to_string(),
            validation: CodeMentionValidationSpec::SymbolRange {
                path: file_path.to_string(),
                line_range,
                byte_range,
                entity_name: entity_name.to_string(),
                anchor_hash: anchor_hash.to_string(),
            },
        },
        extraction_hints: CodeMentionExtractionHints {
            line_range: Some(line_range),
            byte_range: Some(byte_range),
            symbol_kind: Some(symbol_kind.to_string()),
            entity_name: Some(entity_name.to_string()),
        },
        display_meta: CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: "test-version".to_string(),
        },
    }
}
