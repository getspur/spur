use std::path::{Path, PathBuf};
use std::sync::Mutex;

use spur_graph::resolve_worktree_root;

static CWD_LOCK: Mutex<()> = Mutex::new(());

struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    fn enter(path: &Path) -> Self {
        let original = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).expect("restore current dir");
    }
}

#[test]
fn worktree_root_resolves_git_directory() {
    let _lock = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let nested = tmp.path().join("crates/spur-tui");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git dir");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    let _cwd = CwdGuard::enter(&nested);

    let root = resolve_worktree_root();

    assert_eq!(root, tmp.path().canonicalize().expect("canonical tempdir"));
}

#[test]
fn worktree_root_resolves_git_file() {
    let _lock = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let nested = tmp.path().join("crates/spur-tui");
    std::fs::write(
        tmp.path().join(".git"),
        "gitdir: ../.git/worktrees/example\n",
    )
    .expect("create .git file");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    let _cwd = CwdGuard::enter(&nested);

    let root = resolve_worktree_root();

    assert_eq!(root, tmp.path().canonicalize().expect("canonical tempdir"));
}

#[test]
fn worktree_root_falls_back_to_current_dir_without_git_marker() {
    let _lock = CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let nested = tmp.path().join("crates/spur-tui");
    std::fs::create_dir_all(&nested).expect("create nested dir");
    let _cwd = CwdGuard::enter(&nested);

    let root = resolve_worktree_root();

    assert_eq!(root, std::env::current_dir().expect("current dir"));
}
