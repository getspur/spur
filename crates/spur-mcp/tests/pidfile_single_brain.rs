//! T-I4: At most one brain session holds the pidfile.

use std::path::Path;
use tempfile::TempDir;

mod common;
fn br_available() -> bool {
    common::beads::br_available()
}

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

#[tokio::test]
async fn second_brain_acquisition_refuses() {
    if !br_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let pid_path = dir.path().join(".beads").join(".spur-brain.pid");
    let _g1 = spur_pm::pidfile::PidFileGuard::acquire(&pid_path).unwrap();
    let err = spur_pm::pidfile::PidFileGuard::acquire(&pid_path).unwrap_err();
    assert!(format!("{err}").contains("held by another"));
}

#[tokio::test]
async fn stale_pidfile_is_reacquirable_after_restart() {
    if !br_available() {
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]);
    let pid_path = dir.path().join(".beads").join(".spur-brain.pid");
    {
        let _g = spur_pm::pidfile::PidFileGuard::acquire(&pid_path).unwrap();
    }
    let _g2 = spur_pm::pidfile::PidFileGuard::acquire(&pid_path).unwrap();
}
