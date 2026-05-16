//! Integration test: when `WorktreeManager::create_worktree_v2` fails (e.g.
//! because the requested `base_branch` does not exist), the resulting
//! `anyhow::Error` carries the underlying git stderr in its source chain.
//! The orchestrator's `run_one_worker_attempt` must surface that stderr
//! into the operator-visible `AttemptSetupError::WorktreeFailed` payload
//! by formatting with `{e:#}` (chain-walking), not `{e}` / `.to_string()`.
//!
//! Pre-2026-05-17 the worker_attempt path used `e.to_string()`, which drops
//! the chain — repeated worktree-creation failures in production showed up
//! only as `Failed to create worktree: failed to create v2 worktree at <path>`
//! with no indication of WHY git failed. This test pins the contract so the
//! lossy pattern can't silently come back.

use spur_acp::{BrainSessionId, SessionId};
use spur_worktree::WorktreeManager;
use tempfile::TempDir;

async fn git(dir: &std::path::Path, args: &[&str]) {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn seed_repo(td: &TempDir) {
    git(td.path(), &["init", "-q", "-b", "main"]).await;
    git(td.path(), &["config", "user.email", "t@t"]).await;
    git(td.path(), &["config", "user.name", "t"]).await;
    tokio::fs::write(td.path().join("a"), b"x").await.unwrap();
    git(td.path(), &["add", "a"]).await;
    git(td.path(), &["commit", "-q", "-m", "base"]).await;
}

#[tokio::test]
async fn create_worktree_v2_failure_surfaces_underlying_git_stderr_when_formatted_with_chain() {
    let td = TempDir::new().expect("tempdir");
    seed_repo(&td).await;

    let mut wm = WorktreeManager::new(td.path().to_path_buf());
    let brain = BrainSessionId::new(SessionId("brain-test".into()));
    let worker = SessionId("worker-test-uuid".into());

    // Drive a failure: base_branch points at a ref that doesn't exist.
    // `git worktree add ... <missing-ref>` returns a non-zero exit with
    // stderr like "fatal: invalid reference: ..." or
    // "fatal: '...' is not a valid object name".
    let bad_base = "spur-test-definitely-no-such-ref-xyz";
    let err = match wm
        .create_worktree_v2(&brain, &worker, "codex", bad_base)
        .await
    {
        Err(e) => e,
        Ok(_info) => panic!("expected failure on nonexistent base ref but creation succeeded"),
    };

    // Top-level context (added by create_worktree_v2's .with_context).
    let top_only = format!("{err}");
    assert!(
        top_only.contains("failed to resolve base branch")
            || top_only.contains("failed to create v2 worktree"),
        "top-level context must mention the create_worktree_v2 call site; got: {top_only}"
    );

    // Chain-walking format must additionally include the underlying git
    // stderr (which lives in the source chain) so an operator can diagnose
    // the failure without re-running anything by hand.
    let chained = format!("{err:#}");
    let surfaces_git_stderr = chained.contains("fatal:")
        || chained.contains("invalid reference")
        || chained.contains("not a valid object name")
        || chained.contains("unknown revision");
    assert!(
        surfaces_git_stderr,
        "chain-walking format must include git stderr (`fatal: ...`); got: {chained}\n\
         If this assertion fires, someone may have removed the `{{e:#}}` formatter \
         from `worker_attempt.rs`'s `WorktreeFailed` callsites — restore it before \
         landing the change."
    );

    // And the chained format must strictly contain MORE information than the
    // top-only format (otherwise the chain walker is broken or there is no
    // chain to walk — both regressions worth surfacing immediately).
    assert!(
        chained.len() > top_only.len(),
        "chained format must include additional source-chain context beyond the \
         top-level message. top_only={top_only} chained={chained}"
    );
}
