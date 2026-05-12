use std::fs;
use std::path::{Component, Path, PathBuf};

use super::validation::{validate_file, validate_symbol, FailureReason, ValidationOutcome};
use super::{
    CodeMentionKind, CodeMentionPayload, CodeMentionValidationSpec, GraphFileArtifact,
    GraphSymbolArtifact,
};

pub const PER_MENTION_CAP_BYTES: usize = 8 * 1024;
pub const CONTEXT_HEADER_CAP_BYTES: usize = 1500;
pub const PER_PROMPT_CAP_BYTES: usize = 32 * 1024;

const CONTEXT_TRUNCATED_MARKER: &str = "# … context truncated\n";

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
            path,
            line_range,
            byte_range,
            entity_name,
            anchor_hash,
        } => {
            let symbol_payload = GraphSymbolArtifact {
                stable_symbol_id: symbol_id_from_uri(&payload.authoritative.uri),
                file_path: path.clone(),
                byte_range: *byte_range,
                line_range: *line_range,
                entity_name: entity_name.clone(),
                symbol_kind: payload
                    .extraction_hints
                    .symbol_kind
                    .clone()
                    .unwrap_or_else(|| "symbol".to_string()),
                anchor_hash: anchor_hash.clone(),
                enclosing_scope: payload.display_meta.enclosing_scope.clone(),
            };
            match validate_symbol(&symbol_payload, worktree_root) {
                ValidationOutcome::Pass => {
                    match symbol_expansion(payload, worktree_root, *byte_range, *line_range) {
                        Some(text) if text.len() <= PER_MENTION_CAP_BYTES => {
                            ExpandedMention::Body { text }
                        }
                        _ => warning_expansion(payload, FailureReason::BodyTooLarge, worktree_root),
                    }
                }
                ValidationOutcome::Fail(reason) => {
                    warning_expansion(payload, reason, worktree_root)
                }
            }
        }
    }
}

fn symbol_expansion(
    payload: &CodeMentionPayload,
    worktree_root: &Path,
    byte_range: [usize; 2],
    line_range: [usize; 2],
) -> Option<String> {
    let file_path = path_in_worktree(worktree_root, &payload.authoritative.file_path)?;
    let content = fs::read_to_string(file_path).ok()?;
    let source = content.get(byte_range[0]..byte_range[1])?;
    let context_header = context_header(&content, byte_range[0]);
    let symbol_kind = payload
        .extraction_hints
        .symbol_kind
        .as_deref()
        .unwrap_or("symbol");

    Some(format!(
        "MENTION {}\nkind:    symbol:{}\nid:      {}\nfile:    {}\nlines:   {}-{}\ngraph_index_version: {}\n\ncontext_header:\n{}source:\n{}\n\ntopology_available_via_mcp:\n- get_callers(\"{}\")\n- get_callees(\"{}\")\n- get_subgraph(\"{}\", radius=1)\n",
        payload.authoritative.display,
        symbol_kind,
        payload.authoritative.uri,
        payload.authoritative.file_path,
        line_range[0],
        line_range[1],
        payload.display_meta.graph_index_version,
        context_header,
        source,
        payload.authoritative.uri,
        payload.authoritative.uri,
        file_graph_uri(payload),
    ))
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

fn file_id_from_uri(uri: &str) -> String {
    uri.strip_prefix("graph://file/").unwrap_or(uri).to_string()
}

fn symbol_id_from_uri(uri: &str) -> String {
    uri.strip_prefix("graph://symbol/")
        .unwrap_or(uri)
        .to_string()
}

fn path_in_worktree(worktree_root: &Path, file_path: &str) -> Option<PathBuf> {
    let relative = Path::new(file_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return None;
    }

    Some(worktree_root.join(relative))
}
