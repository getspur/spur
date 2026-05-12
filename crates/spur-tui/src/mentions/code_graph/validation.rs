use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{GraphFileArtifact, GraphSymbolArtifact};

pub type FilePayload = GraphFileArtifact;
pub type SymbolPayload = GraphSymbolArtifact;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationOutcome {
    Pass,
    Fail(FailureReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    AnchorHashMismatch,
    RangeOutOfBounds,
    Utf8Boundary,
    /// The recorded symbol name was absent from the slice, empty, or not a
    /// valid single identifier for this byte-level predicate.
    NameNotFound,
    FileMissing,
}

pub fn validate_file(payload: &FilePayload, worktree_root: &Path) -> ValidationOutcome {
    let Some(path) = path_in_worktree(worktree_root, &payload.file_path) else {
        return ValidationOutcome::Fail(FailureReason::FileMissing);
    };

    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => ValidationOutcome::Pass,
        _ => ValidationOutcome::Fail(FailureReason::FileMissing),
    }
}

pub fn validate_symbol(payload: &SymbolPayload, worktree_root: &Path) -> ValidationOutcome {
    let Some(path) = path_in_worktree(worktree_root, &payload.file_path) else {
        return ValidationOutcome::Fail(FailureReason::FileMissing);
    };

    let Ok(metadata) = fs::metadata(&path) else {
        return ValidationOutcome::Fail(FailureReason::FileMissing);
    };
    if !metadata.is_file() {
        return ValidationOutcome::Fail(FailureReason::FileMissing);
    }

    let Ok(bytes) = fs::read(&path) else {
        return ValidationOutcome::Fail(FailureReason::FileMissing);
    };
    let [start, end] = payload.byte_range;
    if end < start || end > bytes.len() {
        return ValidationOutcome::Fail(FailureReason::RangeOutOfBounds);
    }

    let Ok(content) = std::str::from_utf8(&bytes) else {
        return ValidationOutcome::Fail(FailureReason::Utf8Boundary);
    };
    if !content.is_char_boundary(start) || !content.is_char_boundary(end) {
        return ValidationOutcome::Fail(FailureReason::Utf8Boundary);
    }

    let slice = &content[start..end];
    if has_path_separator(&payload.entity_name)
        || payload.entity_name.is_empty()
        || !contains_whole_word(slice.as_bytes(), payload.entity_name.as_bytes())
    {
        return ValidationOutcome::Fail(FailureReason::NameNotFound);
    }

    let Ok(expected_hash) = payload.anchor_hash.parse::<u64>() else {
        return ValidationOutcome::Fail(FailureReason::AnchorHashMismatch);
    };
    if compute_anchor_hash(slice) != expected_hash {
        return ValidationOutcome::Fail(FailureReason::AnchorHashMismatch);
    }

    ValidationOutcome::Pass
}

/// Computes a stable u64 anchor hash from the first and last non-whitespace
/// lines of `slice`.
///
/// Selection treats a line as non-whitespace when `line.trim()` is non-empty,
/// but hashes the selected line text as-is after Rust `str::lines()` newline
/// normalization. The two selected lines are separated by a NUL byte before
/// SHA-256 hashing, and the first eight digest bytes are interpreted as a
/// big-endian `u64`.
pub fn compute_anchor_hash(slice: &str) -> u64 {
    let mut selected = slice.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = selected.next() else {
        return 0;
    };
    let last = selected.next_back().unwrap_or(first);

    let mut hasher = Sha256::new();
    hasher.update(first.as_bytes());
    hasher.update([0]);
    hasher.update(last.as_bytes());
    let digest = hasher.finalize();

    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 digest always has at least eight bytes"),
    )
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

fn has_path_separator(entity_name: &str) -> bool {
    entity_name.contains('/') || entity_name.contains('\\')
}

/// Byte-level whole-word match for the recorded symbol name.
///
/// We intentionally use ASCII identifier boundaries (`[A-Za-z0-9_]`) rather
/// than syntax-aware parsing here because submit-time validation only needs a
/// cheap predicate over the recorded byte slice.
fn contains_whole_word(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }

    haystack
        .windows(needle.len())
        .enumerate()
        .any(|(index, window)| {
            window == needle
                && !is_ascii_word_byte(haystack.get(index.wrapping_sub(1)).copied())
                && !is_ascii_word_byte(haystack.get(index + needle.len()).copied())
        })
}

fn is_ascii_word_byte(byte: Option<u8>) -> bool {
    matches!(byte, Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
}
