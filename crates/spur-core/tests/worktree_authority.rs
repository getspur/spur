//! End-to-end test: SelfHeldSet maintenance prevents authority from
//! sweeping its own active sessions.

use std::time::Duration;

use spur_acp::session_liveness::SelfHeldSet;
use spur_acp::{BrainSessionId, SessionId};
use spur_core::event_funnel;
use spur_core::{AuthorityConfig, WorktreeAuthority};
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

fn brain_id(s: &str) -> BrainSessionId {
    BrainSessionId::new(SessionId(s.into()))
}

fn authority(
    td: &TempDir,
    self_held: SelfHeldSet,
    quarantine_grace: Duration,
) -> WorktreeAuthority {
    let (funnel, _rx) = event_funnel::test_channel();
    WorktreeAuthority::new(
        td.path().to_path_buf(),
        self_held,
        funnel,
        AuthorityConfig {
            quarantine_grace,
            ..AuthorityConfig::default()
        },
    )
}

#[tokio::test]
async fn v2_worker_survives_authority_sweep_when_session_alive() {
    let td = TempDir::new().unwrap();
    seed_repo(&td).await;

    let brain = brain_id("550e8400-e29b-41d4-a716-446655440000");
    let worker = SessionId("deadbeef-1111-2222-3333-444455556666".into());
    let mut manager = WorktreeManager::new(td.path().to_path_buf());
    let info = manager
        .create_worktree_v2(&brain, &worker, "codex", "main")
        .await
        .expect("create v2 worktree");

    let self_held = SelfHeldSet::new();
    self_held.insert(brain);
    let auth = authority(&td, self_held, Duration::ZERO);
    let report = auth.sweep_once().await.expect("sweep");

    assert_eq!(report.skipped_self, 1);
    assert_eq!(report.swept, 0);
    assert!(info.path.exists(), "active v2 worktree must survive");
}

#[tokio::test]
async fn orchestrator_restart_does_not_wipe_in_flight_v2_workers() {
    let td = TempDir::new().unwrap();
    seed_repo(&td).await;

    let brain = brain_id("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    let worker = SessionId("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into());
    let mut manager = WorktreeManager::new(td.path().to_path_buf());
    let info = manager
        .create_worktree_v2(&brain, &worker, "codex", "main")
        .await
        .expect("create v2 worktree");

    let restarted_self_held = SelfHeldSet::new();
    restarted_self_held.insert(brain);
    let restarted_auth = authority(&td, restarted_self_held, Duration::ZERO);
    let report = restarted_auth.sweep_once().await.expect("sweep");

    assert_eq!(report.skipped_self, 1);
    assert_eq!(report.swept, 0);
    assert!(info.path.exists(), "restarted brain must retain v2 worker");
}

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

#[tokio::test]
async fn self_held_remove_allows_sweep_to_reclaim() {
    use tokio::process::Command;

    let td = TempDir::new().unwrap();
    // Set up repo + v2 worktree.
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

    // No lockfile -> SessionLivenessProbeResult::Missing -> reclaim eligible.
    let self_held = SelfHeldSet::new();
    let bid = spur_acp::BrainSessionId::new(spur_acp::SessionId(brain.into()));
    self_held.insert(bid.clone());
    self_held.remove(&bid); // simulate retire-side cleanup

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
    // No lockfile means probe sees Missing; with quarantine_grace=ZERO, sweep reclaims.
    // (The remove is the precondition; this test verifies the post-remove behavior
    // does NOT skip via Self_.)
    assert_eq!(
        report.skipped_self, 0,
        "after remove, must NOT skip via Self_"
    );
}

#[tokio::test]
async fn two_orchestrators_do_not_sweep_each_others_worktrees() {
    use fs4::fs_std::FileExt;
    use std::fs::OpenOptions;
    use tokio::process::Command;

    let td = TempDir::new().unwrap();

    // Set up a repo with TWO v2 worktrees, owned by different brains.
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

    let brain_a = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    let brain_b = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    let worker_a = "11111111-1111-1111-1111-111111111111";
    let worker_b = "22222222-2222-2222-2222-222222222222";
    let branch_a = format!("spur/worker/v2/codex/{brain_a}/{worker_a}");
    let branch_b = format!("spur/worker/v2/codex/{brain_b}/{worker_b}");
    let wt_a = td.path().join(".spur/worktrees/wa");
    let wt_b = td.path().join(".spur/worktrees/wb");
    let _ = Command::new("git")
        .args([
            "worktree",
            "add",
            wt_a.to_str().unwrap(),
            "-b",
            &branch_a,
            "main",
        ])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();
    let _ = Command::new("git")
        .args([
            "worktree",
            "add",
            wt_b.to_str().unwrap(),
            "-b",
            &branch_b,
            "main",
        ])
        .current_dir(td.path())
        .output()
        .await
        .unwrap();

    // Simulate orchestrator A: holds session A's lockfile.
    std::fs::create_dir_all(td.path().join(".spur/sessions")).unwrap();
    let lock_a = td
        .path()
        .join(".spur/sessions")
        .join(format!("{brain_a}.lock"));
    std::fs::write(&lock_a, b"").unwrap();
    let held_a = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_a)
        .unwrap();
    held_a.try_lock_exclusive().unwrap();

    // Orchestrator B (does NOT hold A's lock) sweeps.
    let self_held_b = SelfHeldSet::new();
    self_held_b.insert(spur_acp::BrainSessionId::new(spur_acp::SessionId(
        brain_b.into(),
    )));
    let (funnel, _rx) = event_funnel::test_channel();
    let auth_b = WorktreeAuthority::new(
        td.path().to_path_buf(),
        self_held_b,
        funnel,
        AuthorityConfig {
            quarantine_grace: Duration::ZERO,
            ..AuthorityConfig::default()
        },
    );
    let report = auth_b.sweep_once().await.expect("sweep");

    assert_eq!(report.skipped_live, 1, "B must see A's session as Live");
    assert_eq!(report.skipped_self, 1, "B must skip its own session");
    assert_eq!(report.swept, 0, "B must not delete anything");
    assert!(wt_a.exists(), "A's worktree must still exist on disk");
    assert!(wt_b.exists(), "B's worktree must still exist on disk");

    drop(held_a);
}
