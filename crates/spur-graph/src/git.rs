use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCtx {
    pub worktree_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub head_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedEntry {
    pub path: String,
    pub oid: String,
    pub mode: String,
    pub content_oid: String,
    pub is_gitlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyEntry {
    pub path: String,
    pub status: String,
}

pub fn detect(worktree_root: &Path) -> Option<GitCtx> {
    let root = rev_parse_worktree_root(worktree_root).ok()?;
    let git_common_dir = rev_parse_common_dir(worktree_root).ok()?;
    let head_oid = rev_parse_head(worktree_root).ok()?;
    Some(GitCtx {
        worktree_root: root,
        git_common_dir,
        head_oid,
    })
}

pub fn rev_parse_head(root: &Path) -> anyhow::Result<String> {
    git_stdout(root, &["rev-parse", "HEAD"]).map(|out| out.trim_end().to_string())
}

pub fn rev_parse_common_dir(root: &Path) -> anyhow::Result<PathBuf> {
    let out = git_stdout(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(PathBuf::from(out.trim_end()))
}

pub fn ls_files_with_oids(root: &Path) -> anyhow::Result<Vec<TrackedEntry>> {
    let sparse_paths = sparse_paths(root)?;
    let out = git_stdout_bytes(root, &["ls-files", "-s", "-z"])?;
    let mut by_path = BTreeMap::new();

    for record in out
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record = std::str::from_utf8(record).context("git ls-files emitted non-UTF-8 path")?;
        let Some((meta, path)) = record.split_once('\t') else {
            tracing::warn!(record, "spur-graph: skipping malformed git ls-files record");
            continue;
        };
        let mut parts = meta.split_whitespace();
        let Some(mode) = parts.next() else {
            continue;
        };
        let Some(oid) = parts.next() else {
            continue;
        };
        let Some(stage) = parts.next() else {
            continue;
        };

        if stage != "0" {
            tracing::warn!(path, stage, "spur-graph: skipping unmerged git index entry");
            continue;
        }
        if mode == "120000" {
            continue;
        }
        if sparse_paths.contains(path) {
            continue;
        }

        let is_gitlink = mode == "160000";
        let content_oid = if is_gitlink {
            format!("gitlink:{oid}")
        } else {
            oid.to_string()
        };
        by_path.insert(
            path.to_string(),
            TrackedEntry {
                path: path.to_string(),
                oid: oid.to_string(),
                mode: mode.to_string(),
                content_oid,
                is_gitlink,
            },
        );
    }

    Ok(by_path.into_values().collect())
}

pub fn status_dirty_paths(root: &Path) -> anyhow::Result<Vec<DirtyEntry>> {
    let out = git_stdout_bytes(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut entries = Vec::new();
    let mut chunks = out
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty());

    while let Some(record) = chunks.next() {
        let record = std::str::from_utf8(record).context("git status emitted non-UTF-8 path")?;
        if record.len() < 4 {
            tracing::warn!(record, "spur-graph: skipping malformed git status record");
            continue;
        }
        let status = record[..2].to_string();
        let path = record[3..].to_string();
        if status.starts_with('R') || status.starts_with('C') {
            let _ = chunks.next();
        }
        entries.push(DirtyEntry { path, status });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.status.cmp(&b.status)));
    entries.dedup_by(|a, b| a.path == b.path && a.status == b.status);
    Ok(entries)
}

fn rev_parse_worktree_root(root: &Path) -> anyhow::Result<PathBuf> {
    let out = git_stdout(root, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out.trim_end()))
}

fn sparse_paths(root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let out = git_stdout_bytes(root, &["ls-files", "-t", "-z"])?;
    let mut paths = BTreeSet::new();
    for record in out
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record =
            std::str::from_utf8(record).context("git ls-files -t emitted non-UTF-8 path")?;
        if let Some(path) = record.strip_prefix("S ") {
            paths.insert(path.to_string());
        }
    }
    Ok(paths)
}

fn git_stdout(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let bytes = git_stdout_bytes(root, args)?;
    String::from_utf8(bytes).with_context(|| format!("git {args:?} emitted non-UTF-8 stdout"))
}

fn git_stdout_bytes(root: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {args:?} in `{}`", root.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed in `{}`: {}",
            args,
            root.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::{
        detect, ls_files_with_oids, rev_parse_common_dir, rev_parse_head, status_dirty_paths,
    };

    #[test]
    fn detect_returns_context_inside_repo_and_none_outside() {
        let repo = init_repo();
        commit_file(repo.path(), "src/lib.rs", "pub fn run() {}\n");

        let nested = repo.path().join("src");
        let ctx = detect(&nested).expect("detect repo from nested directory");
        assert_eq!(
            ctx.worktree_root.canonicalize().unwrap(),
            repo.path().canonicalize().unwrap()
        );
        assert_eq!(
            ctx.git_common_dir,
            rev_parse_common_dir(repo.path()).unwrap()
        );
        assert_eq!(ctx.head_oid, rev_parse_head(repo.path()).unwrap());

        let outside = TempDir::new().unwrap();
        assert!(detect(outside.path()).is_none());
    }

    #[test]
    fn ls_files_filters_symlinks_and_unmerged_entries() {
        let repo = init_repo();
        commit_file(repo.path(), "base.txt", "base\n");
        let base_branch = current_branch(repo.path());
        run_git(repo.path(), &["checkout", "-b", "left"]);
        commit_file(repo.path(), "conflict.txt", "left\n");
        run_git(repo.path(), &["checkout", &base_branch]);
        commit_file(repo.path(), "conflict.txt", "right\n");
        let _ = Command::new("git")
            .args(["merge", "left"])
            .current_dir(repo.path())
            .output()
            .unwrap();

        let symlink_path = repo.path().join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink("base.txt", &symlink_path).unwrap();
        #[cfg(not(unix))]
        fs::write(&symlink_path, "not a symlink\n").unwrap();
        run_git(repo.path(), &["add", "link.txt"]);

        let entries = ls_files_with_oids(repo.path()).unwrap();
        assert!(entries.iter().any(|entry| entry.path == "base.txt"));
        assert!(!entries.iter().any(|entry| entry.path == "conflict.txt"));
        #[cfg(unix)]
        assert!(!entries.iter().any(|entry| entry.path == "link.txt"));
    }

    #[test]
    fn status_dirty_paths_reports_modified_and_untracked_paths() {
        let repo = init_repo();
        commit_file(repo.path(), "tracked.rs", "pub fn one() {}\n");
        fs::write(repo.path().join("tracked.rs"), "pub fn two() {}\n").unwrap();
        fs::write(repo.path().join("untracked.rs"), "pub fn three() {}\n").unwrap();

        let dirty = status_dirty_paths(repo.path()).unwrap();
        let paths: Vec<_> = dirty.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&"tracked.rs"));
        assert!(paths.contains(&"untracked.rs"));
    }

    #[test]
    fn ls_files_filters_sparse_entries() {
        let repo = init_repo();
        commit_file(repo.path(), "keep.rs", "pub fn keep() {}\n");
        commit_file(repo.path(), "skip.rs", "pub fn skip() {}\n");

        run_git(repo.path(), &["update-index", "--skip-worktree", "skip.rs"]);

        let entries = ls_files_with_oids(repo.path()).unwrap();
        assert!(entries.iter().any(|entry| entry.path == "keep.rs"));
        assert!(!entries.iter().any(|entry| entry.path == "skip.rs"));
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        dir
    }

    fn commit_file(repo: &Path, path: &str, contents: &str) {
        let full_path = repo.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, contents).unwrap();
        run_git(repo, &["add", path]);
        run_git(repo, &["commit", "-m", "commit"]);
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn current_branch(repo: &Path) -> String {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(repo)
            .output()
            .expect("git branch --show-current");
        assert!(
            output.status.success(),
            "git branch --show-current failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("branch name UTF-8")
            .trim_end()
            .to_string()
    }
}
