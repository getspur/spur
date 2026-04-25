//! Bounded-string clipping for continuation materialization.
//!
//! These helpers are part of the INV-D8 enforcement contract: the
//! materializer (`spur-mcp::OutcomeMaterializer`) and truncation-ladder
//! fallback (`spur-core::continuation_bridge`) both call into this module
//! to bound the lean payload's inline strings.
//!
//! **Do not call these helpers from random consumer code.** Adding a new
//! `BrainContinuation` producer that bypasses this module is a violation
//! of INV-D8 and will surface as oversized-drop failures in the merger.
//! New producers must route through `OutcomeMaterializer::materialize`.

use std::path::PathBuf;

use crate::domain::continuation::ArtifactRef;
use crate::domain::delegation::DelegationStatus;
use crate::domain::events::DiffSummary;

const ELLIPSIS: &str = "…";

pub fn clip_with_ellipsis(s: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    let Some(s) = s else {
        return (None, false);
    };
    if s.len() <= max_bytes {
        return (Some(s), false);
    }
    if max_bytes <= ELLIPSIS.len() {
        return (Some(ELLIPSIS.to_string()), true);
    }
    let mut end = max_bytes - ELLIPSIS.len();
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut clipped = s[..end].to_string();
    clipped.push_str(ELLIPSIS);
    (Some(clipped), true)
}

pub fn clip_status_strings(status: &DelegationStatus, max_bytes: usize) -> DelegationStatus {
    use crate::domain::delegation::TimeoutFallback;
    let mut s = status.clone();
    match &mut s {
        DelegationStatus::Failed { error } => {
            *error = clip_with_ellipsis(Some(std::mem::take(error)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::Conflict { files } => {
            clip_path_vec(files, 16, 128);
        }
        DelegationStatus::Rejected { reason } => {
            *reason = clip_with_ellipsis(Some(std::mem::take(reason)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::Modified { reviewer_note } => {
            *reviewer_note = clip_with_ellipsis(Some(std::mem::take(reviewer_note)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::Cancelled { reason } => {
            *reason = clip_with_ellipsis(Some(std::mem::take(reason)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::TimedOut { fallback, .. } => {
            if let TimeoutFallback::Reject { reason } = fallback {
                *reason = clip_with_ellipsis(Some(std::mem::take(reason)), max_bytes)
                    .0
                    .unwrap_or_default();
            }
        }
        DelegationStatus::Success | DelegationStatus::Timeout => {}
    }
    s
}

pub fn clip_diff_files(diff: &DiffSummary, max_files: usize) -> DiffSummary {
    let mut out = diff.clone();
    clip_path_vec(&mut out.files, max_files, 128);
    out
}

pub fn clip_artifact_ref_strings(art: &ArtifactRef, max_bytes: usize) -> ArtifactRef {
    let mut out = art.clone();
    let (uri_clipped, _) = clip_with_ellipsis(Some(std::mem::take(&mut out.uri)), max_bytes);
    out.uri = uri_clipped.unwrap_or_default();
    if let Some(s) = out.git_object_ref.take() {
        let (clipped, _) = clip_with_ellipsis(Some(s), max_bytes);
        out.git_object_ref = clipped;
    }
    out
}

fn clip_path_vec(v: &mut Vec<PathBuf>, max_count: usize, max_path_bytes: usize) {
    if v.len() > max_count {
        v.truncate(max_count);
    }
    for p in v.iter_mut() {
        let s = p.to_string_lossy().into_owned();
        if s.len() > max_path_bytes {
            let (clipped, _) = clip_with_ellipsis(Some(s), max_path_bytes);
            *p = PathBuf::from(clipped.unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::delegation::DelegationStatus;

    #[test]
    fn clip_with_ellipsis_under_cap_returns_unchanged() {
        let (out, trunc) = clip_with_ellipsis(Some("short".into()), 100);
        assert_eq!(out.as_deref(), Some("short"));
        assert!(!trunc);
    }

    #[test]
    fn clip_with_ellipsis_over_cap_appends_ellipsis() {
        let (out, trunc) = clip_with_ellipsis(Some("a".repeat(50)), 10);
        assert!(trunc);
        let s = out.expect("Some");
        assert!(s.ends_with(ELLIPSIS));
        assert!(s.len() <= 10);
    }

    #[test]
    fn clip_with_ellipsis_respects_utf8_boundary() {
        let s = "日本語日本語日本語".to_string();
        let (out, trunc) = clip_with_ellipsis(Some(s), 7);
        assert!(trunc);
        let out = out.expect("Some");
        assert!(out.is_char_boundary(out.len() - ELLIPSIS.len()));
    }

    #[test]
    fn clip_status_strings_failed_error_capped() {
        let s = DelegationStatus::Failed {
            error: "x".repeat(2000),
        };
        let clipped = clip_status_strings(&s, 256);
        if let DelegationStatus::Failed { error } = clipped {
            assert!(error.len() <= 256);
            assert!(error.ends_with(ELLIPSIS));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_diff_files_truncates_count_and_paths() {
        // DiffSummary has no Default impl - construct fully.
        let diff = DiffSummary {
            files_changed: 32,
            insertions: 0,
            deletions: 0,
            files: (0..32)
                .map(|i| PathBuf::from("a".repeat(200) + &i.to_string()))
                .collect(),
        };
        let out = clip_diff_files(&diff, 16);
        assert_eq!(out.files.len(), 16);
        for p in &out.files {
            assert!(p.to_string_lossy().len() <= 128);
        }
    }
}
