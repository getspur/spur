use super::*;
use sha2::{Digest, Sha256};

/// Tail-weighted, UTF-8-safe truncation for worker summaries.
///
/// Why tail-weighted: LLM worker output opens with task restatement
/// and closes with a crisp conclusion + file list. The middle holds
/// verbose tool-call transcripts with low decision-density. Brain-
/// relevant information is concentrated at the tail.
///
/// Returns `text` unchanged if `text.len() <= cap`. Otherwise keeps
/// `cap/4` head bytes and `cap - cap/4` tail bytes (both aligned to
/// char boundaries), joined by an omission marker.
pub(crate) fn truncate_summary(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let head_budget = cap / 4;
    let tail_budget = cap - head_budget;

    let head_end = {
        let mut i = head_budget.min(text.len());
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let tail_start = {
        let mut i = text.len().saturating_sub(tail_budget);
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    };

    // Clamp degenerate case where head and tail would overlap.
    let tail_start = tail_start.max(head_end);

    // Use char count (not byte diff) so the marker is meaningful for
    // multi-byte input — the very case this helper is designed to handle.
    let omitted = text[head_end..tail_start].chars().count();
    format!(
        "{}\n\n[... {} chars omitted ...]\n\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

/// The effective summary cap in bytes, read from `SPUR_SUMMARY_MAX_BYTES`
/// (default 4000). Single source of truth for both `truncate_summary`
/// and artifact-persistence predicates.
pub(crate) fn summary_cap_bytes() -> usize {
    std::env::var("SPUR_SUMMARY_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000)
}

pub(crate) fn truncate_summary_env_default(text: &str) -> String {
    truncate_summary(text, summary_cap_bytes())
}

pub(crate) fn sha256_hex_for_outcome(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

/// Apply the calibrated artifact-vs-transport failure rule. Pure.
///
/// Inputs:
/// - `worker_success`: did the worker itself report success?
/// - `persist_result`: outcome of `WorktreeManager::persist_artifact`.
///   `None` means the orchestrator skipped persistence (output under cap).
///   `Some(Ok)` / `Some(Err)` are the persistence outcomes.
/// - `original_error_status`: the status the caller would have returned
///   if persistence hadn't been attempted. Only consulted on the
///   `!worker_success` branch; the helper composes with existing error
///   extraction so this path keeps the worker's original error.
///
/// Returns: `(status, artifact, summary_annotation)`.
/// - `status` is the final `DelegationStatus`.
/// - `artifact` is `Some` on successful persistence (regardless of
///   worker success — failing workers still get diagnostic artifacts).
/// - `summary_annotation`, if `Some`, must be appended to the truncated
///   summary tail by the caller.
///
/// Failure rule:
/// - worker_success + Ok  -> Success + Some(artifact) + no note
/// - worker_success + Err -> Failed { "artifact persistence failed: ..." } + None + note
/// - !worker_success + Ok -> original_error_status + Some(artifact) + no note
/// - !worker_success + Err -> original_error_status + None + note
pub(crate) fn decide_artifact_handling(
    worker_success: bool,
    persist_result: Option<Result<spur_acp::WorkerArtifact, String>>,
    original_error_status: Option<DelegationStatus>,
) -> (
    DelegationStatus,
    Option<spur_acp::WorkerArtifact>,
    Option<String>,
) {
    match (worker_success, persist_result) {
        (true, Some(Ok(art))) => (DelegationStatus::Success, Some(art), None),
        (true, Some(Err(e))) => {
            let msg = format!("artifact persistence failed: {e}");
            (
                DelegationStatus::Failed { error: msg.clone() },
                None,
                Some(format!("[orchestrator: {msg}]")),
            )
        }
        (false, Some(Ok(art))) => (
            original_error_status.unwrap_or(DelegationStatus::Failed {
                error: "worker failed".into(),
            }),
            Some(art),
            None,
        ),
        (false, Some(Err(e))) => (
            original_error_status.unwrap_or(DelegationStatus::Failed {
                error: "worker failed".into(),
            }),
            None,
            Some(format!("[orchestrator: artifact persistence failed: {e}]")),
        ),
        // No persist attempt — caller's responsibility.
        (true, None) => (DelegationStatus::Success, None, None),
        (false, None) => (
            original_error_status.unwrap_or(DelegationStatus::Failed {
                error: "worker failed".into(),
            }),
            None,
            None,
        ),
    }
}

/// Compute a `DiffSummary` for a worktree via `git diff --numstat <basis>`.
///
/// `basis` must match what `collect_diff` used for the raw diff — either
/// "HEAD" or "<base_commit>..HEAD" (rendered with the actual SHA). Otherwise
/// the raw diff text and the structured summary disagree.
///
/// Preferred over regex-parsing the unified diff text because numstat
/// emits tab-separated stats directly and handles binary files (`-\t-\tpath`),
/// renames, and mode-only changes without ambiguity.
///
/// Cost: ~10-100ms. Same budget as `collect_diff`.
pub(crate) async fn build_diff_summary(
    worktree_path: &std::path::Path,
    basis: &str,
) -> anyhow::Result<spur_acp::DiffSummary> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .arg(basis)
        .current_dir(worktree_path)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files_changed = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut files = Vec::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("");
        let del = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        // Rename notation: "old => new" (top-level) or "dir/{old => new}" (nested).
        // Extract destination path so downstream consumers see a real filename.
        let path = if let Some(arrow_pos) = path.find(" => ") {
            let after_arrow = &path[arrow_pos + 4..];
            // Nested form: "dir/{old => new}/tail" — strip the trailing '}' and
            // reconstruct as "dir/" + destination + "/tail". For the simple
            // top-level form "old => new" there are no braces and this just
            // returns `new`.
            if let Some(brace_pos) = path[..arrow_pos].rfind('{') {
                let prefix = &path[..brace_pos];
                let dest = after_arrow.trim_end_matches('}');
                // Handle "dir/{old => new}/tail" — find where the '}' lived.
                let (dest_clean, tail) = match dest.find('}') {
                    Some(i) => (&dest[..i], &dest[i + 1..]),
                    None => (dest, ""),
                };
                format!("{}{}{}", prefix, dest_clean, tail)
            } else {
                after_arrow.to_string()
            }
        } else {
            path.to_string()
        };
        files_changed += 1;
        // numstat emits "-" for binary files. Non-"-" values parse as usize.
        insertions += ins.parse::<usize>().unwrap_or(0);
        deletions += del.parse::<usize>().unwrap_or(0);
        files.push(std::path::PathBuf::from(&path));
    }

    Ok(spur_acp::DiffSummary {
        files_changed,
        insertions,
        deletions,
        files,
    })
}

#[cfg(test)]
mod truncate_summary_tests {
    use super::truncate_summary;

    // Serializes all env-mutating tests in this module. `SPUR_SUMMARY_MAX_BYTES`
    // is process-global; without this lock the tests race under the default
    // parallel harness.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn under_cap_returns_unchanged() {
        let input = "short text";
        assert_eq!(truncate_summary(input, 4000), "short text");
    }

    #[test]
    fn exact_cap_returns_unchanged() {
        let input = "x".repeat(100);
        assert_eq!(truncate_summary(&input, 100), input);
    }

    #[test]
    fn over_cap_preserves_head_and_tail_with_marker() {
        let input: String = (0..5000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let cap = 4000;
        let out = truncate_summary(&input, cap);
        assert!(out.len() < input.len(), "output must be shorter than input");
        assert!(out.contains("chars omitted"), "omission marker must appear");
        let tail_start = input.len() - 3000;
        assert!(
            out.ends_with(&input[tail_start..]),
            "output must end with the last 3000 chars of input"
        );
        assert!(
            out.starts_with(&input[..1000]),
            "output must start with the first 1000 chars of input"
        );
    }

    #[test]
    fn utf8_boundary_does_not_panic() {
        let input = "—".repeat(20);
        let out = truncate_summary(&input, 10);
        assert!(out.chars().count() > 0);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(truncate_summary("", 4000), "");
    }

    #[test]
    fn summary_cap_bytes_respects_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", "1234") };
        let got = super::summary_cap_bytes();
        match prev {
            Some(v) => unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) },
            None => unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") },
        }
        assert_eq!(got, 1234);
    }

    #[test]
    fn summary_cap_bytes_defaults_to_4000_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") };
        let got = super::summary_cap_bytes();
        if let Some(v) = prev {
            unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) };
        }
        assert_eq!(got, 4000);
    }

    #[test]
    fn env_var_overrides_default_cap() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // This test mutates process-global env state. It is safe only
        // because no other test in this binary reads SPUR_SUMMARY_MAX_BYTES
        // concurrently. If that changes (future Task 6 integration test,
        // etc.), gate with #[serial] from the serial_test crate.
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", "50") };
        let input = "x".repeat(200);
        let out = super::truncate_summary_env_default(&input);
        assert!(out.len() < input.len());
        assert!(
            out.len() <= 100,
            "output must respect env override, got {}",
            out.len()
        );
        match prev {
            Some(v) => unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) },
            None => unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") },
        }
    }
}

#[cfg(test)]
mod build_diff_summary_tests {
    use super::build_diff_summary;
    use spur_acp::DiffSummary;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        fn git(path: &std::path::Path, args: &[&str]) {
            let out = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let dir = tempdir().unwrap();
        let path = dir.path();
        git(path, &["init"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), "hello\nworld\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn clean_worktree_returns_zero_summary() {
        let dir = init_repo();
        let summary: DiffSummary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 0);
        assert_eq!(summary.insertions, 0);
        assert_eq!(summary.deletions, 0);
        assert!(summary.files.is_empty());
    }

    #[tokio::test]
    async fn modified_file_produces_expected_stats() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\nnew line\n").unwrap();
        let summary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 1);
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("a.txt")]);
    }

    #[tokio::test]
    async fn binary_file_is_counted_but_numbers_stay_zero() {
        let dir = init_repo();
        // numstat emits "-\t-\tpath" for binary files.
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3, 0xFF]).unwrap();
        Command::new("git")
            .args(["add", "b.bin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "bin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("b.bin"), [9u8, 8, 7]).unwrap();
        let summary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(
            summary.insertions, 0,
            "binary diff reports '-' for line counts"
        );
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("b.bin")]);
    }

    #[tokio::test]
    async fn renamed_file_reports_destination_path() {
        let dir = init_repo();
        let path = dir.path();
        // Create a second file to make git rename-detection engage reliably.
        std::fs::write(path.join("a.txt"), "hello\nworld\nextra\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "grow"])
            .current_dir(path)
            .output()
            .unwrap();
        // Rename a.txt -> b.txt with a small tweak so line counts are non-zero.
        std::fs::rename(path.join("a.txt"), path.join("b.txt")).unwrap();
        std::fs::write(path.join("b.txt"), "hello\nworld\nextra\nrenamed\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();

        let summary = build_diff_summary(path, "HEAD").await.unwrap();
        // Either git reports a rename (1 entry, path=b.txt) OR a delete+add pair
        // (2 entries, both a.txt and b.txt). Both are acceptable — the key
        // invariant is: no path contains " => " after our rename-stripping.
        assert!(
            summary
                .files
                .iter()
                .all(|p| !p.to_string_lossy().contains(" => ")),
            "rename notation leaked into path: {:?}",
            summary.files
        );
        // b.txt must appear in the file list under either shape.
        assert!(
            summary
                .files
                .iter()
                .any(|p| p.file_name().and_then(|s| s.to_str()) == Some("b.txt")),
            "b.txt not in file list: {:?}",
            summary.files
        );
    }
}

#[cfg(test)]
mod artifact_decision_tests {
    use super::*;
    use spur_acp::{ArtifactKind, DelegationStatus, WorkerArtifact};

    #[test]
    fn outcome_sha256_hex_uses_lowercase_content_digest() {
        assert_eq!(
            sha256_hex_for_outcome(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn sample_artifact() -> WorkerArtifact {
        WorkerArtifact {
            object_ref: "refs/spur/artifacts/s1".into(),
            blob_sha: "a".repeat(40),
            size_bytes: 1_024,
            kind: ArtifactKind::Output,
        }
    }

    #[test]
    fn success_with_persist_ok_is_success_and_carries_artifact() {
        let (status, artifact, note) = decide_artifact_handling(
            /* worker_success */ true,
            /* persist_result */ Some(Ok(sample_artifact())),
            /* original_error_status */ None,
        );
        assert!(matches!(status, DelegationStatus::Success));
        assert!(artifact.is_some());
        assert!(note.is_none());
    }

    #[test]
    fn success_with_persist_err_escalates_to_failed() {
        let (status, artifact, note) =
            decide_artifact_handling(true, Some(Err("disk full".into())), None);
        match status {
            DelegationStatus::Failed { error } => {
                assert!(
                    error.contains("artifact persistence failed"),
                    "got: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(artifact.is_none());
        assert!(note.is_some());
    }

    #[test]
    fn failure_with_persist_err_preserves_original_error_and_annotates() {
        let original = DelegationStatus::Failed {
            error: "compile error".into(),
        };
        let (status, artifact, note) = decide_artifact_handling(
            /* worker_success */ false,
            Some(Err("ref locked".into())),
            Some(original.clone()),
        );
        assert_eq!(status, original);
        assert!(artifact.is_none());
        let n = note.expect("failure path must annotate");
        assert!(n.contains("orchestrator"));
        assert!(n.contains("artifact persistence failed"));
    }

    #[test]
    fn failure_with_persist_ok_preserves_original_error_and_carries_artifact() {
        let original = DelegationStatus::Failed {
            error: "panic".into(),
        };
        let (status, artifact, note) = decide_artifact_handling(
            false,
            Some(Ok(WorkerArtifact {
                kind: ArtifactKind::Diagnostic,
                ..sample_artifact()
            })),
            Some(original.clone()),
        );
        assert_eq!(status, original);
        let a = artifact.expect("diagnostic artifact must be surfaced on failed worker");
        assert_eq!(a.kind, ArtifactKind::Diagnostic);
        assert!(note.is_none());
    }

    #[test]
    fn under_cap_path_is_unchanged() {
        // When we never attempted persistence (output_text.len() <= cap),
        // the helper is not called. Document the caller's contract: no
        // call -> no annotation -> no escalation. This is asserted by
        // the absence of the call site at the appropriate branch.
        // See `run_one_worker_attempt` for the guard.
    }
}
