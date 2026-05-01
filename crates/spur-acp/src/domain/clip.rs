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
    // Hard cap: if max_bytes can't fit even the ellipsis (3 bytes for "…"),
    // return empty rather than violating max_bytes by emitting "…".
    if max_bytes < ELLIPSIS.len() {
        return (Some(String::new()), true);
    }
    if max_bytes == ELLIPSIS.len() {
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
        DelegationStatus::SetupFailed { error } => match error {
            crate::domain::delegation::AttemptSetupError::SnapshotFailed { error }
            | crate::domain::delegation::AttemptSetupError::WorktreeFailed { error }
            | crate::domain::delegation::AttemptSetupError::InitFailed { error }
            | crate::domain::delegation::AttemptSetupError::SessionFailed { error } => {
                *error = clip_with_ellipsis(Some(std::mem::take(error)), max_bytes)
                    .0
                    .unwrap_or_default();
            }
            crate::domain::delegation::AttemptSetupError::OverlayConflict { files, .. } => {
                clip_string_vec(files, 16, 128);
            }
        },
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
        // Avoid the unconditional heap allocation that
        // `to_string_lossy().into_owned()` triggers for every path.
        // Cow::Borrowed paths short-circuit when under cap.
        let cow = p.to_string_lossy();
        if cow.len() > max_path_bytes {
            let (clipped, _) = clip_with_ellipsis(Some(cow.into_owned()), max_path_bytes);
            *p = PathBuf::from(clipped.unwrap_or_default());
        }
    }
}

fn clip_string_vec(v: &mut Vec<String>, max_count: usize, max_bytes: usize) {
    if v.len() > max_count {
        v.truncate(max_count);
    }
    for s in v.iter_mut() {
        if s.len() > max_bytes {
            *s = clip_with_ellipsis(Some(std::mem::take(s)), max_bytes)
                .0
                .unwrap_or_default();
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

    #[test]
    fn clip_with_ellipsis_handles_none_input() {
        let (out, trunc) = clip_with_ellipsis(None, 100);
        assert!(out.is_none());
        assert!(!trunc);
    }

    #[test]
    fn clip_with_ellipsis_handles_zero_max_bytes() {
        let (out, trunc) = clip_with_ellipsis(Some("anything".into()), 0);
        assert!(trunc);
        // Below ELLIPSIS.len() returns empty rather than violating max_bytes.
        assert_eq!(out.as_deref(), Some(""));
    }

    #[test]
    fn clip_with_ellipsis_handles_max_bytes_below_ellipsis() {
        // ELLIPSIS is "…" = 3 bytes. max_bytes=2 must NOT emit "…".
        let (out, trunc) = clip_with_ellipsis(Some("longer".into()), 2);
        assert!(trunc);
        assert_eq!(out.as_deref(), Some(""));
    }

    #[test]
    fn clip_with_ellipsis_handles_max_bytes_exactly_ellipsis() {
        let (out, trunc) = clip_with_ellipsis(Some("longer".into()), 3);
        assert!(trunc);
        assert_eq!(out.as_deref(), Some("…"));
    }

    #[test]
    fn clip_with_ellipsis_handles_empty_string() {
        let (out, trunc) = clip_with_ellipsis(Some(String::new()), 10);
        assert_eq!(out.as_deref(), Some(""));
        assert!(!trunc);
    }

    #[test]
    fn clip_status_strings_clips_modified_reviewer_note() {
        let s = DelegationStatus::Modified {
            reviewer_note: "y".repeat(1000),
        };
        let clipped = clip_status_strings(&s, 128);
        if let DelegationStatus::Modified { reviewer_note } = clipped {
            assert!(reviewer_note.len() <= 128);
            assert!(reviewer_note.ends_with(ELLIPSIS));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_status_strings_clips_cancelled_reason() {
        let s = DelegationStatus::Cancelled {
            reason: "z".repeat(800),
        };
        let clipped = clip_status_strings(&s, 64);
        if let DelegationStatus::Cancelled { reason } = clipped {
            assert!(reason.len() <= 64);
            assert!(reason.ends_with(ELLIPSIS));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_status_strings_clips_rejected_reason() {
        let s = DelegationStatus::Rejected {
            reason: "r".repeat(500),
        };
        let clipped = clip_status_strings(&s, 50);
        if let DelegationStatus::Rejected { reason } = clipped {
            assert!(reason.len() <= 50);
            assert!(reason.ends_with(ELLIPSIS));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_status_strings_clips_timed_out_reject_reason() {
        use crate::domain::delegation::TimeoutFallback;
        use std::time::Duration;
        let s = DelegationStatus::TimedOut {
            waited_for: Duration::from_secs(60),
            fallback: TimeoutFallback::Reject {
                reason: "t".repeat(2000),
            },
        };
        let clipped = clip_status_strings(&s, 100);
        if let DelegationStatus::TimedOut { fallback, .. } = clipped {
            if let TimeoutFallback::Reject { reason } = fallback {
                assert!(reason.len() <= 100);
                assert!(reason.ends_with(ELLIPSIS));
            } else {
                panic!("inner fallback changed");
            }
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_status_strings_clips_conflict_files() {
        let files: Vec<PathBuf> = (0..32)
            .map(|i| PathBuf::from("p".repeat(300) + &i.to_string()))
            .collect();
        let s = DelegationStatus::Conflict { files };
        let clipped = clip_status_strings(&s, 256);
        if let DelegationStatus::Conflict { files } = clipped {
            assert_eq!(files.len(), 16);
            for p in &files {
                assert!(p.to_string_lossy().len() <= 128);
            }
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_status_strings_passes_through_success_and_timeout() {
        let s = clip_status_strings(&DelegationStatus::Success, 64);
        assert!(matches!(s, DelegationStatus::Success));
        let t = clip_status_strings(&DelegationStatus::Timeout, 64);
        assert!(matches!(t, DelegationStatus::Timeout));
    }
}
