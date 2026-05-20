use std::fs;
use std::path::Path;

use spur_graph::{
    file_id_from_uri, path_in_worktree, validate_file, validate_symbol, CodeMentionKind,
    CodeMentionPayload, CodeMentionValidationSpec, FailureReason, GraphFileArtifact,
    GraphSymbolArtifact, ValidationOutcome,
};

pub const CONTEXT_HEADER_CAP_BYTES: usize = 1500;
pub const PER_PROMPT_CAP_BYTES: usize = 32 * 1024;

const CONTEXT_TRUNCATED_MARKER: &str = "# … context truncated\n";
const SIGNATURE_CAP_BYTES: usize = 200;
const SIGNATURE_TRUNCATED_MARKER: &str = "…";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandedMention {
    Body {
        text: String,
    },
    Warning {
        text: String,
        replaced_with: ReplacedWith,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacedWith {
    FileMention,
    Dropped,
}

pub fn expand(payload: &CodeMentionPayload, worktree_root: &Path) -> ExpandedMention {
    match &payload.authoritative.validation {
        CodeMentionValidationSpec::FileExists { path } => {
            let file_payload = GraphFileArtifact {
                stable_file_id: file_id_from_uri(&payload.authoritative.uri),
                file_path: path.clone(),
            };
            match validate_file(&file_payload, worktree_root) {
                ValidationOutcome::Pass => ExpandedMention::Body {
                    text: file_expansion(payload),
                },
                ValidationOutcome::Fail(reason) => {
                    warning_expansion(payload, reason, worktree_root)
                }
            }
        }
        CodeMentionValidationSpec::SymbolRange {
            line_range,
            byte_range,
            entity_name,
            anchor_hash,
            ..
        } => {
            let Some(path) = path_in_worktree(worktree_root, &payload.authoritative.file_path)
            else {
                return warning_expansion(payload, FailureReason::FileMissing, worktree_root);
            };
            let Ok(metadata) = fs::metadata(&path) else {
                return warning_expansion(payload, FailureReason::FileMissing, worktree_root);
            };
            if !metadata.is_file() {
                return warning_expansion(payload, FailureReason::FileMissing, worktree_root);
            }

            let symbol_payload = symbol_validation_payload(
                payload,
                *line_range,
                *byte_range,
                entity_name,
                anchor_hash,
            );
            match validate_symbol(&symbol_payload, worktree_root) {
                ValidationOutcome::Pass => {}
                ValidationOutcome::Fail(reason) => {
                    return warning_expansion(payload, reason, worktree_root);
                }
            }

            let (signature, context_header) = fs::read_to_string(&path)
                .ok()
                .map(|content| {
                    (
                        first_signature_line(&content, *byte_range, entity_name),
                        context_header(&content, byte_range[0]),
                    )
                })
                .unwrap_or_default();
            let text =
                symbol_expansion(payload, *line_range, signature.as_deref(), &context_header);
            ExpandedMention::Body { text }
        }
    }
}

fn symbol_validation_payload(
    payload: &CodeMentionPayload,
    line_range: [usize; 2],
    byte_range: [usize; 2],
    entity_name: &str,
    anchor_hash: &str,
) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: payload.authoritative.uri.clone(),
        file_path: payload.authoritative.file_path.clone(),
        byte_range,
        line_range,
        entity_name: entity_name.to_string(),
        symbol_kind: payload
            .extraction_hints
            .symbol_kind
            .clone()
            .unwrap_or_else(|| "symbol".to_string()),
        anchor_hash: anchor_hash.to_string(),
        enclosing_scope: payload.display_meta.enclosing_scope.clone(),
    }
}

fn symbol_expansion(
    payload: &CodeMentionPayload,
    line_range: [usize; 2],
    signature: Option<&str>,
    context_header: &str,
) -> String {
    let symbol_kind = payload
        .extraction_hints
        .symbol_kind
        .as_deref()
        .unwrap_or("symbol");
    let display_name = match payload.display_meta.enclosing_scope.as_deref() {
        Some(scope) => format!("{}::{}", scope, payload.authoritative.display),
        None => payload.authoritative.display.clone(),
    };

    let signature_line = signature
        .map(|signature| format!("signature: {signature}\n"))
        .unwrap_or_default();

    format!(
        "MENTION {}\nkind:    symbol:{}\nid:      {}\nfile:    {}\nlines:   {}-{}\n{}graph_index_version: {}\n\ncontext_header:\n{}",
        display_name,
        symbol_kind,
        payload.authoritative.uri,
        payload.authoritative.file_path,
        line_range[0],
        line_range[1],
        signature_line,
        payload.display_meta.graph_index_version,
        context_header,
    )
}

fn file_expansion(payload: &CodeMentionPayload) -> String {
    format!(
        "MENTION {}\nkind: file\nid:   {}\nfile: {}\nlines: full\n",
        payload.authoritative.display, payload.authoritative.uri, payload.authoritative.file_path
    )
}

fn warning_expansion(
    payload: &CodeMentionPayload,
    reason: FailureReason,
    worktree_root: &Path,
) -> ExpandedMention {
    let file_exists = path_in_worktree(worktree_root, &payload.authoritative.file_path)
        .and_then(|path| fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.is_file());
    let replaced_with = if file_exists {
        ReplacedWith::FileMention
    } else {
        ReplacedWith::Dropped
    };

    let mut text = format!(
        "MENTION_WARNING {}\nintended_uri:   {}\nfailure_reason: {}\nreplaced_with:  {}\n",
        payload.authoritative.display,
        payload.authoritative.uri,
        failure_reason_label(reason),
        replaced_with_label(replaced_with),
    );
    if replaced_with == ReplacedWith::FileMention {
        text.push_str(&file_expansion(&file_replacement_payload(payload)));
    }

    ExpandedMention::Warning {
        text,
        replaced_with,
    }
}

fn file_replacement_payload(payload: &CodeMentionPayload) -> CodeMentionPayload {
    let mut file_payload = payload.clone();
    file_payload.authoritative.kind = CodeMentionKind::File;
    file_payload.authoritative.display = payload.authoritative.file_path.clone();
    file_payload.authoritative.uri = file_graph_uri(payload);
    file_payload.authoritative.validation = CodeMentionValidationSpec::FileExists {
        path: payload.authoritative.file_path.clone(),
    };
    file_payload.extraction_hints.line_range = None;
    file_payload.extraction_hints.byte_range = None;
    file_payload.extraction_hints.symbol_kind = None;
    file_payload.extraction_hints.entity_name = None;
    file_payload
}

fn context_header(content: &str, symbol_start: usize) -> String {
    let mut lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            continue;
        }
        if is_top_context_line(trimmed) {
            lines.push(line.to_string());
            continue;
        }
        break;
    }

    if let Some(signature) = enclosing_impl_or_trait_signature(content, symbol_start) {
        if !lines.is_empty() && !lines.last().is_some_and(|line| line.is_empty()) {
            lines.push(String::new());
        }
        lines.push(signature);
    }

    let mut header = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    truncate_end_with_marker(
        &mut header,
        CONTEXT_HEADER_CAP_BYTES,
        CONTEXT_TRUNCATED_MARKER,
    );
    header
}

fn first_signature_line(
    content: &str,
    byte_range: [usize; 2],
    entity_name: &str,
) -> Option<String> {
    let [start, end] = byte_range;
    if start >= end || end > content.len() {
        return None;
    }

    let mut line_start = 0;
    for line_with_ending in content.split_inclusive('\n') {
        let line_end = line_start + line_with_ending.len();
        let line = line_with_ending
            .trim_end_matches('\n')
            .trim_end_matches('\r');
        let code_start = line_start + line.len() - line.trim_start().len();
        if code_start >= start && code_start < end && line.contains(entity_name) {
            return Some(truncate_signature_line(trim_signature_block_open(line)));
        }
        line_start = line_end;
    }

    None
}

fn trim_signature_block_open(line: &str) -> String {
    let trimmed = line.trim();
    trimmed
        .strip_suffix('{')
        .map(str::trim_end)
        .unwrap_or(trimmed)
        .to_string()
}

fn truncate_signature_line(mut line: String) -> String {
    if line.len() <= SIGNATURE_CAP_BYTES {
        return line;
    }
    let keep = previous_char_boundary(
        &line,
        SIGNATURE_CAP_BYTES.saturating_sub(SIGNATURE_TRUNCATED_MARKER.len()),
    );
    line.truncate(keep);
    line.push_str(SIGNATURE_TRUNCATED_MARKER);
    line
}

fn is_top_context_line(trimmed: &str) -> bool {
    trimmed.starts_with("#![")
        || trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("from ")
}

fn enclosing_impl_or_trait_signature(content: &str, symbol_start: usize) -> Option<String> {
    let prefix = content.get(..symbol_start)?;
    let mut balance = 0isize;
    for line in prefix.lines().rev() {
        let trimmed = line.trim();
        balance += trimmed.matches('}').count() as isize;
        if trimmed.contains('{') {
            balance -= trimmed.matches('{').count() as isize;
            if balance < 0 && is_impl_or_trait_opening(trimmed) {
                return Some(signature_line(line));
            }
        }
    }
    None
}

fn is_impl_or_trait_opening(trimmed: &str) -> bool {
    (trimmed.starts_with("impl ")
        || trimmed.starts_with("pub impl ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("pub trait "))
        && trimmed.ends_with('{')
}

fn signature_line(line: &str) -> String {
    match line.find('{') {
        Some(index) => line[..=index].trim().to_string(),
        None => line.trim().to_string(),
    }
}

fn truncate_end_with_marker(text: &mut String, cap: usize, marker: &str) {
    if text.len() <= cap {
        return;
    }
    if marker.len() >= cap {
        text.truncate(previous_char_boundary(text, cap));
        return;
    }
    let mut keep = previous_char_boundary(text, cap - marker.len());
    if keep > 0 && !text[..keep].ends_with('\n') {
        keep = text[..keep].rfind('\n').map(|index| index + 1).unwrap_or(0);
    }
    text.truncate(keep);
    text.push_str(marker);
}

fn previous_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub fn failure_reason_label(reason: FailureReason) -> &'static str {
    match reason {
        FailureReason::AnchorHashMismatch => "anchor_hash_mismatch",
        FailureReason::RangeOutOfBounds => "range_out_of_bounds",
        FailureReason::Utf8Boundary => "utf8_boundary",
        FailureReason::NameNotFound => "name_not_found",
        FailureReason::FileMissing => "file_missing",
        FailureReason::BodyTooLarge => "body_too_large",
    }
}

fn replaced_with_label(replaced_with: ReplacedWith) -> &'static str {
    match replaced_with {
        ReplacedWith::FileMention => "file_mention",
        ReplacedWith::Dropped => "dropped",
    }
}

fn file_graph_uri(payload: &CodeMentionPayload) -> String {
    if matches!(payload.authoritative.kind, CodeMentionKind::File) {
        return payload.authoritative.uri.clone();
    }
    format!("graph://file/{}", payload.authoritative.file_path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use spur_graph::validation::compute_anchor_hash;
    use spur_graph::{
        CodeMentionAuthoritative, CodeMentionDisplayMeta, CodeMentionExtractionHints,
    };

    use super::*;

    #[test]
    fn symbol_pass_path_expands_existing_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source =
            "use crate::Config;\n\npub fn run() -> Result<()> {\n    println!(\"run\");\n}\n";
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
            [3, 5],
        );

        let ExpandedMention::Body { text } = expand(&payload, dir.path()) else {
            panic!("expected body expansion");
        };

        assert_eq!(
            text,
            "MENTION @run\nkind:    symbol:fn\nid:      graph://symbol/symbol-run\nfile:    src/lib.rs\nlines:   3-5\nsignature: pub fn run() -> Result<()>\ngraph_index_version: test-version\n\ncontext_header:\nuse crate::Config;\n\n"
        );
    }

    #[test]
    fn first_signature_line_extracts_function_signature() {
        let source =
            "use crate::Config;\n\npub fn run(args: Args) -> Result<()> {\n    Ok(())\n}\n";
        let start = source.find("pub fn run").expect("start");
        let end = source.len();

        assert_eq!(
            first_signature_line(source, [start, end], "run"),
            Some("pub fn run(args: Args) -> Result<()>".to_string())
        );
    }

    #[test]
    fn first_signature_line_trims_trailing_block_open() {
        let source = "impl Engine {\n    fn run(&mut self) {\n        work();\n    }\n}\n";
        let start = source.find("fn run").expect("start");
        let end = source.find("        work").expect("end");

        assert_eq!(
            first_signature_line(source, [start, end], "run"),
            Some("fn run(&mut self)".to_string())
        );
    }

    #[test]
    fn first_signature_line_truncates_over_cap() {
        let source = format!(
            "pub fn run({}) -> Result<()> {{\n}}\n",
            "arg: Type, ".repeat(30)
        );
        let expected = {
            let mut line = source.lines().next().expect("signature").trim().to_string();
            line.pop();
            truncate_signature_line(line)
        };

        let actual =
            first_signature_line(&source, [0, source.len()], "run").expect("signature line");

        assert_eq!(actual, expected);
        assert!(actual.len() <= 200);
        assert!(actual.ends_with('…'), "{actual}");
    }

    #[test]
    fn first_signature_line_returns_none_when_entity_name_absent() {
        let source = "pub fn other() {}\n";

        assert_eq!(first_signature_line(source, [0, source.len()], "run"), None);
    }

    #[test]
    fn symbol_anchor_hash_mismatch_warns_and_replaces_with_file_mention() {
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
        assert!(text.contains("MENTION_WARNING @run"), "{text}");
        assert!(
            text.contains("failure_reason: anchor_hash_mismatch"),
            "{text}"
        );
        assert!(text.contains("replaced_with:  file_mention"), "{text}");
        assert!(text.contains("MENTION src/lib.rs\nkind: file"), "{text}");
    }

    #[test]
    fn symbol_range_out_of_bounds_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = "pub fn run() {}\n";
        write_source(dir.path(), "src/lib.rs", source);
        let payload = symbol_payload(
            "@run",
            "graph://symbol/symbol-run",
            "src/lib.rs",
            [0, source.len() + 1],
            [1, 1],
            "run",
            "fn",
            0,
        );

        let ExpandedMention::Warning { text, .. } = expand(&payload, dir.path()) else {
            panic!("expected warning expansion");
        };

        assert!(
            text.contains("failure_reason: range_out_of_bounds"),
            "{text}"
        );
    }

    #[test]
    fn symbol_name_not_found_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = "pub fn run() {}\n";
        write_source(dir.path(), "src/lib.rs", source);
        let payload = symbol_payload(
            "@missing",
            "graph://symbol/symbol-run",
            "src/lib.rs",
            [0, source.len()],
            [1, 1],
            "missing",
            "fn",
            compute_anchor_hash(source),
        );

        let ExpandedMention::Warning { text, .. } = expand(&payload, dir.path()) else {
            panic!("expected warning expansion");
        };

        assert!(text.contains("failure_reason: name_not_found"), "{text}");
    }

    #[test]
    fn symbol_file_missing_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload = symbol_payload(
            "@run",
            "graph://symbol/symbol-run",
            "src/missing.rs",
            [0, 15],
            [1, 1],
            "run",
            "fn",
            0,
        );

        let ExpandedMention::Warning {
            text,
            replaced_with,
        } = expand(&payload, dir.path())
        else {
            panic!("expected warning expansion");
        };

        assert_eq!(replaced_with, ReplacedWith::Dropped);
        assert!(text.contains("failure_reason: file_missing"), "{text}");
        assert!(text.contains("replaced_with:  dropped"), "{text}");
    }

    fn write_source(root: &Path, relative: &str, source: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, source).expect("write source");
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
}
