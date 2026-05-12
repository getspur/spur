use std::fs;

use spur_acp::ContentBlock;
use spur_tui::commands::submit_router::assemble_blocks_with_code_mentions;
use spur_tui::components::input_bar::{ProtectedRange, RangeKind};
use spur_tui::mentions::code_graph::expansion::{expand, ExpandedMention, ReplacedWith};
use spur_tui::mentions::code_graph::validation::{
    compute_anchor_hash, validate_symbol, ValidationOutcome,
};
use spur_tui::mentions::code_graph::GraphSymbolArtifact;
use spur_tui::mentions::{CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec};

#[test]
fn symbol_expansion_includes_context_header_and_mcp_affordance() {
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
        "@run",
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

    assert!(text.contains("MENTION @run"), "{text}");
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
    assert!(
        text.contains("source:\n    pub fn run(&self) {\n        println!(\"run\");\n"),
        "{text}"
    );
    assert!(text.contains("topology_available_via_mcp:\n- get_callers(\"graph://symbol/symbol-run\")\n- get_callees(\"graph://symbol/symbol-run\")\n- get_subgraph(\"graph://file/src/lib.rs\", radius=1)"), "{text}");
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
        .and_then(|rest| rest.split("\nsource:\n").next())
        .expect("context header");

    assert!(header.len() <= 1500, "header too large: {}", header.len());
    assert!(header.ends_with("# … context truncated"), "{header:?}");
    assert!(std::str::from_utf8(header.as_bytes()).is_ok());
}

#[test]
fn body_exceeding_per_mention_budget_fails_with_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = format!("pub fn huge() {{\n{}\n}}\n", "    work();\n".repeat(900));
    write_source(dir.path(), "src/lib.rs", &body);
    let payload = symbol_payload_from_source(
        "@huge",
        "graph://symbol/symbol-huge",
        "src/lib.rs",
        &body,
        "pub fn huge",
        "\n",
        "huge",
        "fn",
        [1, 903],
    );

    let ExpandedMention::Warning {
        text,
        replaced_with,
    } = expand(&payload, dir.path())
    else {
        panic!("expected warning expansion");
    };

    assert_eq!(replaced_with, ReplacedWith::FileMention);
    assert!(text.contains("MENTION_WARNING @huge"), "{text}");
    assert!(text.contains("failure_reason: body_too_large"), "{text}");
    assert!(text.contains("replaced_with:  file_mention"), "{text}");
    assert!(text.contains("MENTION src/lib.rs"), "{text}");
}

#[test]
fn file_shrink_after_validation_reports_file_missing_not_body_too_large() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "pub fn run() {\n    println!(\"run\");\n}\n";
    write_source(dir.path(), "src/lib.rs", source);
    let payload = symbol_payload_from_source(
        "@run",
        "graph://symbol/symbol-run",
        "src/lib.rs",
        source,
        "pub fn run",
        "\n",
        "run",
        "fn",
        [1, 3],
    );
    let CodeMentionValidationSpec::SymbolRange {
        path,
        line_range,
        byte_range,
        entity_name,
        anchor_hash,
    } = &payload.authoritative.validation
    else {
        panic!("expected symbol payload");
    };
    let symbol = GraphSymbolArtifact {
        stable_symbol_id: "symbol-run".to_string(),
        file_path: path.clone(),
        byte_range: *byte_range,
        line_range: *line_range,
        entity_name: entity_name.clone(),
        symbol_kind: "fn".to_string(),
        anchor_hash: anchor_hash.clone(),
        enclosing_scope: None,
    };

    assert_eq!(
        validate_symbol(&symbol, dir.path()),
        ValidationOutcome::Pass
    );

    fs::write(dir.path().join("src/lib.rs"), "pub fn").expect("truncate source");

    let ExpandedMention::Warning {
        text,
        replaced_with,
    } = expand(&payload, dir.path())
    else {
        panic!("expected warning expansion");
    };

    assert_eq!(replaced_with, ReplacedWith::FileMention);
    assert!(text.contains("failure_reason: file_missing"), "{text}");
    assert!(!text.contains("failure_reason: body_too_large"), "{text}");
}

#[test]
fn degraded_symbol_mentions_emit_warning_with_t2_failure_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "pub fn other() {}\n";
    write_source(dir.path(), "src/lib.rs", source);
    let payload = symbol_payload(
        "@run",
        "graph://symbol/symbol-run",
        "src/lib.rs",
        [0, source.len()],
        [1, 1],
        "run",
        "fn",
        compute_anchor_hash(source),
    );

    let ExpandedMention::Warning {
        text,
        replaced_with,
    } = expand(&payload, dir.path())
    else {
        panic!("expected warning expansion");
    };

    assert_eq!(replaced_with, ReplacedWith::FileMention);
    assert!(text.contains("MENTION_WARNING @run"), "{text}");
    assert!(
        text.contains("intended_uri:   graph://symbol/symbol-run"),
        "{text}"
    );
    assert!(text.contains("failure_reason: name_not_found"), "{text}");
    assert!(text.contains("replaced_with:  file_mention"), "{text}");
    assert!(text.contains("MENTION src/lib.rs"), "{text}");
}

#[test]
fn degraded_symbol_mentions_preserve_all_validation_failure_reasons() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "pub fn run() {}\n";
    write_source(dir.path(), "src/lib.rs", source);
    let cases = [
        (
            symbol_payload(
                "@run",
                "graph://symbol/symbol-oob",
                "src/lib.rs",
                [0, source.len() + 1],
                [1, 1],
                "run",
                "fn",
                0,
            ),
            "range_out_of_bounds",
            "file_mention",
        ),
        (
            symbol_payload(
                "@run",
                "graph://symbol/symbol-anchor",
                "src/lib.rs",
                [0, source.len()],
                [1, 1],
                "run",
                "fn",
                42,
            ),
            "anchor_hash_mismatch",
            "file_mention",
        ),
        (
            symbol_payload(
                "@café",
                "graph://symbol/symbol-utf8",
                "src/lib.rs",
                [0, "pub fn café".len() - 1],
                [1, 1],
                "café",
                "fn",
                0,
            ),
            "utf8_boundary",
            "file_mention",
        ),
        (
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
        ),
    ];
    write_source(dir.path(), "src/utf8.rs", "pub fn café() {}\n");

    for (mut payload, reason, replacement) in cases {
        if reason == "utf8_boundary" {
            payload.authoritative.file_path = "src/utf8.rs".to_string();
            payload.authoritative.validation = CodeMentionValidationSpec::SymbolRange {
                path: "src/utf8.rs".to_string(),
                line_range: [1, 1],
                byte_range: [0, "pub fn café".len() - 1],
                entity_name: "café".to_string(),
                anchor_hash: "0".to_string(),
            };
        }
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

    for i in 0..5 {
        let name = format!("@big{i}");
        let uri = format!("graph://symbol/symbol-big-{i}");
        let source = format!("pub fn big{i}() {{\n{}\n}}\n", "    work();\n".repeat(520));
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
            [1, 523],
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
        flattened.contains("MENTION_OMITTED graph://symbol/symbol-big-4 (per-prompt cap)"),
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

fn file_payload(display: &str, uri: &str, file_path: &str) -> CodeMentionPayload {
    CodeMentionPayload {
        authoritative: spur_tui::mentions::code_graph::CodeMentionAuthoritative {
            display: display.to_string(),
            uri: uri.to_string(),
            kind: CodeMentionKind::File,
            file_path: file_path.to_string(),
            validation: CodeMentionValidationSpec::FileExists {
                path: file_path.to_string(),
            },
        },
        extraction_hints: spur_tui::mentions::code_graph::CodeMentionExtractionHints {
            line_range: None,
            byte_range: None,
            symbol_kind: None,
            entity_name: None,
        },
        display_meta: spur_tui::mentions::code_graph::CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: "test-version".to_string(),
        },
    }
}

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
        authoritative: spur_tui::mentions::code_graph::CodeMentionAuthoritative {
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
        extraction_hints: spur_tui::mentions::code_graph::CodeMentionExtractionHints {
            line_range: Some(line_range),
            byte_range: Some(byte_range),
            symbol_kind: Some(symbol_kind.to_string()),
            entity_name: Some(entity_name.to_string()),
        },
        display_meta: spur_tui::mentions::code_graph::CodeMentionDisplayMeta {
            enclosing_scope: None,
            graph_index_version: "test-version".to_string(),
        },
    }
}
