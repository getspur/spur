//! End-to-end test: SelfHeldSet maintenance prevents authority from
//! sweeping its own active sessions.

use std::time::Duration;

use spur_acp::session_liveness::SelfHeldSet;
use spur_core::event_funnel;
use spur_core::{AuthorityConfig, WorktreeAuthority};
use tempfile::TempDir;

#[tokio::test]
async fn self_held_session_prevents_sweep_during_active_use() {
    let td = TempDir::new().unwrap();
    use tokio::process::Command;
    let _ = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();
    let _ = Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();
    let _ = Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();
    tokio::fs::write(td.path().join("a"), b"x").await.unwrap();
    let _ = Command::new("git")
        .args(["add", "a"])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();
    let _ = Command::new("git")
        .args(["commit", "-q", "-m", "base"])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();

    let brain = "550e8400-e29b-41d4-a716-446655440000";
    let worker = "deadbeef-1111-2222-3333-444455556666";
    let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
    let wt = td.path().join(".spur/worktrees/abc");
    let _ = Command::new("git")
        .args([
            "worktree",
            "add",
            wt.to_str().unwrap(),
            "-b",
            &branch,
            "main",
        ])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();

    let self_held = SelfHeldSet::new();
    self_held.insert(spur_acp::BrainSessionId::new(spur_acp::SessionId(
        brain.into(),
    )));

    let (funnel, _rx) = event_funnel::test_channel();
    let auth = WorktreeAuthority::new(
        td.path().to_path_buf(),
        self_held,
        funnel,
        AuthorityConfig {
            quarantine_grace: Duration::ZERO,
            ..AuthorityConfig::default()
        },
    );
    let report = auth.sweep_once().await.expect("sweep");
    assert_eq!(report.skipped_self, 1, "must skip self-held session");
    assert_eq!(report.swept, 0, "must NOT sweep self-held session");
    assert!(wt.exists(), "worktree dir must still exist on disk");
}
