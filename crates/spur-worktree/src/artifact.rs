//! Side-channel artifact persistence for worker stdout that exceeds
//! the summary cap.

use anyhow::{anyhow, Context, Result};
use spur_acp::{ArtifactKind, WorkerArtifact};
use std::path::Path;

/// Maximum bytes we will ever hand to `git hash-object --stdin`.
/// OOM defense: a pathological worker streaming gigabytes must be
/// refused before we allocate.
pub const ARTIFACT_PHYSICAL_MAX_BYTES: usize = 5 * 1024 * 1024; // 5 MiB

/// Effective stored cap. Inputs larger than this are tail-weighted
/// truncated (same shape as `orchestrator::truncate_summary`) before
/// persistence, so the blob is bounded regardless of what the worker
/// produced.
pub fn artifact_stored_cap_bytes() -> usize {
    std::env::var("SPUR_ARTIFACT_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(524_288) // 512 KiB
}

/// Persist `output` as a git blob on `worktree_path`, pointed at by
/// `refs/spur/artifacts/<session_id>`. Returns a `WorkerArtifact` on
/// success.
///
/// Errors (returned as `Err`, not logged):
/// - `output.len() > ARTIFACT_PHYSICAL_MAX_BYTES` — OOM guard; refuses before I/O.
/// - `git hash-object` or `git update-ref` fails — propagates with context.
///
/// Applies stored-cap truncation transparently; caller does not need
/// to pre-truncate.
pub async fn persist(
    worktree_path: &Path,
    session_id: &str,
    output: &str,
    kind: ArtifactKind,
) -> Result<WorkerArtifact> {
    if output.len() > ARTIFACT_PHYSICAL_MAX_BYTES {
        return Err(anyhow!(
            "artifact size {} exceeds physical cap {}",
            output.len(),
            ARTIFACT_PHYSICAL_MAX_BYTES
        ));
    }
    let stored = maybe_truncate_for_storage(output);
    let stored_len = stored.len();

    let blob_sha = hash_object(worktree_path, stored.as_ref()).await?;
    let object_ref = format!("refs/spur/artifacts/{session_id}");
    update_ref(worktree_path, &object_ref, &blob_sha).await?;

    Ok(WorkerArtifact {
        object_ref,
        blob_sha,
        size_bytes: stored_len,
        kind,
    })
}

/// Tail-weighted truncation identical in shape to
/// `orchestrator::truncate_summary` but with a distinct cap. Returns
/// an owned `String` so the caller can hand bytes to stdin.
fn maybe_truncate_for_storage(text: &str) -> std::borrow::Cow<'_, str> {
    let cap = artifact_stored_cap_bytes();
    if text.len() <= cap {
        return std::borrow::Cow::Borrowed(text);
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
    }
    .max(head_end);
    let omitted = text[head_end..tail_start].chars().count();
    std::borrow::Cow::Owned(format!(
        "{}\n\n[... {} chars omitted ...]\n\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    ))
}

async fn hash_object(worktree: &Path, content: &str) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(worktree)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git hash-object")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content.as_bytes())
            .await
            .context("failed to write stdin to git hash-object")?;
        // Dropping stdin closes it so hash-object can finish.
        drop(stdin);
    }
    let out = child
        .wait_with_output()
        .await
        .context("git hash-object exited with error")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git hash-object failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let sha = String::from_utf8(out.stdout)
        .context("git hash-object returned non-utf8 sha")?
        .trim()
        .to_string();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("git hash-object returned malformed sha: {sha:?}"));
    }
    Ok(sha)
}

async fn update_ref(worktree: &Path, ref_name: &str, sha: &str) -> Result<()> {
    let out = tokio::process::Command::new("git")
        .args(["update-ref", ref_name, sha])
        .current_dir(worktree)
        .output()
        .await
        .context("failed to run git update-ref")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git update-ref failed for {ref_name} -> {sha}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(unsafe_code)] // std::env::set_var is unsafe in Rust 2024; test-only setup.
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::process::Command;

    static ARTIFACT_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn init_repo(dir: &Path) {
        Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@spur"])
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "spur-test"])
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        // Need at least one commit for HEAD to resolve; git hash-object doesn't
        // need HEAD but update-ref may complain otherwise.
        std::fs::write(dir.join("seed"), "seed").unwrap();
        Command::new("git")
            .args(["add", "seed"])
            .current_dir(dir)
            .status()
            .await
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "seed"])
            .current_dir(dir)
            .status()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn persist_small_output_stores_full_content() {
        let td = tempdir().unwrap();
        init_repo(td.path()).await;
        let body = "hello research\n".repeat(20);
        // Lock because we read SPUR_ARTIFACT_MAX_BYTES and another test
        // in this module temporarily mutates it; without this guard the
        // stored cap can leak across parallel tests.
        let _guard = ARTIFACT_ENV_LOCK.lock().await;
        let art = persist(td.path(), "sess-small", &body, ArtifactKind::Output)
            .await
            .unwrap();
        assert!(art.object_ref.starts_with("refs/spur/artifacts/"));
        assert_eq!(art.size_bytes, body.len());
        assert_eq!(art.kind, ArtifactKind::Output);
        // Retrieve and verify.
        let out = Command::new("git")
            .args(["cat-file", "-p", &art.blob_sha])
            .current_dir(td.path())
            .output()
            .await
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap(), body);
    }

    #[tokio::test]
    async fn persist_rejects_over_physical_cap() {
        let td = tempdir().unwrap();
        init_repo(td.path()).await;
        let body = "x".repeat(ARTIFACT_PHYSICAL_MAX_BYTES + 1);
        let err = persist(td.path(), "sess-huge", &body, ArtifactKind::Output)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exceeds physical cap"));
    }

    #[tokio::test]
    async fn persist_truncates_over_stored_cap_with_marker() {
        let td = tempdir().unwrap();
        init_repo(td.path()).await;
        // Temporarily lower the stored cap for the duration of this test.
        let _guard = ARTIFACT_ENV_LOCK.lock().await;
        unsafe { std::env::set_var("SPUR_ARTIFACT_MAX_BYTES", "100") };
        let body = "y".repeat(1000);
        let art = persist(td.path(), "sess-trunc", &body, ArtifactKind::Diagnostic)
            .await
            .unwrap();
        unsafe { std::env::remove_var("SPUR_ARTIFACT_MAX_BYTES") };
        assert!(
            art.size_bytes <= 200,
            "stored artifact must be bounded by stored cap plus marker overhead"
        );
        let out = Command::new("git")
            .args(["cat-file", "-p", &art.blob_sha])
            .current_dir(td.path())
            .output()
            .await
            .unwrap();
        let stored = String::from_utf8(out.stdout).unwrap();
        assert!(
            stored.contains("chars omitted"),
            "stored artifact must carry a truncation marker"
        );
    }

    #[tokio::test]
    async fn persist_idempotent_on_same_session_overwrites_ref() {
        let td = tempdir().unwrap();
        init_repo(td.path()).await;
        // Lock to avoid racing with the stored-cap test's env mutation.
        let _guard = ARTIFACT_ENV_LOCK.lock().await;
        let a1 = persist(td.path(), "sess-same", "first", ArtifactKind::Output)
            .await
            .unwrap();
        let a2 = persist(td.path(), "sess-same", "second", ArtifactKind::Output)
            .await
            .unwrap();
        assert_eq!(a1.object_ref, a2.object_ref);
        assert_ne!(a1.blob_sha, a2.blob_sha);
    }
}
