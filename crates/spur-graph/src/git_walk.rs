use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::schema::WalkStrategy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWalkConfig {
    pub target_refs: Vec<String>,
    pub walk_strategy: WalkStrategy,
    pub allow_replace_refs: bool,
}

impl Default for GitWalkConfig {
    fn default() -> Self {
        Self {
            target_refs: vec!["main".to_string()],
            walk_strategy: WalkStrategy::Reachable,
            allow_replace_refs: false,
        }
    }
}

pub fn snapshot_refs(worktree: &Path, refs: &[&str]) -> Result<BTreeMap<String, String>> {
    ensure_not_shallow(worktree)?;
    let mut snapshot = BTreeMap::new();

    for target_ref in refs {
        let ref_name = format!("refs/heads/{target_ref}");
        let stdout =
            run_git(worktree, &["rev-parse", "--verify", &ref_name]).with_context(|| {
                format!("target ref `{target_ref}` does not exist; refusing to fall back")
            })?;
        snapshot.insert((*target_ref).to_string(), stdout.trim().to_string());
    }

    Ok(snapshot)
}

pub fn ensure_not_shallow(worktree: &Path) -> Result<()> {
    let stdout = run_git(worktree, &["rev-parse", "--is-shallow-repository"]).with_context(
        || {
            format!(
                "spur-graph: could not determine whether `{}` is a shallow repository; refusing to walk",
                worktree.display()
            )
        },
    )?;

    if stdout.trim() == "true" {
        bail!(
            "spur-graph: refusing to index shallow clone at `{}`; symbol history would be silently truncated. Run `git fetch --unshallow` first.",
            worktree.display()
        );
    }

    Ok(())
}

pub fn check_replace_refs(worktree: &Path, allow: bool) -> Result<()> {
    if allow {
        return Ok(());
    }

    let replace_refs = run_git(
        worktree,
        &["for-each-ref", "--format=%(refname)", "refs/replace"],
    )
    .with_context(|| {
        format!(
            "spur-graph: could not inspect git replace refs at `{}`; refusing to walk",
            worktree.display()
        )
    })?;
    let grafts_path = git_dir(worktree)?.join("info/grafts");

    if !replace_refs.trim().is_empty() || grafts_path.exists() {
        bail!(
            "spur-graph: git replace refs or grafts detected at `{}`; refusing to walk. Set GitWalkConfig.allow_replace_refs = true to override.",
            worktree.display()
        );
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: FileChangeKind,
    pub parent_sha: Option<String>,
}

pub fn file_changes_for_commit(worktree: &Path, sha: &str) -> Result<Vec<FileChange>> {
    let parents = commit_parents(worktree, sha)?;
    if parents.is_empty() {
        return root_commit_changes(worktree, sha);
    }

    let mut changes = Vec::new();
    for parent in parents {
        let stdout = run_git_bytes(
            worktree,
            &[
                "diff-tree",
                "-r",
                "-z",
                "--name-status",
                "--find-renames",
                &parent,
                sha,
            ],
        )?;
        parse_name_status(&stdout, Some(parent), &mut changes)?;
    }

    Ok(changes)
}

fn commit_parents(worktree: &Path, sha: &str) -> Result<Vec<String>> {
    let stdout = run_git(worktree, &["rev-list", "--parents", "-n", "1", sha])?;
    let mut fields = stdout.split_whitespace();
    fields.next();
    Ok(fields.map(str::to_string).collect())
}

fn root_commit_changes(worktree: &Path, sha: &str) -> Result<Vec<FileChange>> {
    let stdout = run_git_bytes(worktree, &["ls-tree", "-r", "-z", "--name-only", sha])?;

    Ok(nul_fields(&stdout)
        .map(|path| FileChange {
            path: pathbuf_from_git_bytes(path),
            kind: FileChangeKind::Added,
            parent_sha: None,
        })
        .collect())
}

fn parse_name_status(
    stdout: &[u8],
    parent_sha: Option<String>,
    changes: &mut Vec<FileChange>,
) -> Result<()> {
    let mut fields = nul_fields(stdout);

    while let Some(field) = fields.next() {
        let (status, first_path) = status_and_optional_path(field);
        let path1 = match first_path {
            Some(path) => path,
            None => fields.next().with_context(|| {
                format!(
                    "git diff-tree emitted status `{}` without a path",
                    String::from_utf8_lossy(status)
                )
            })?,
        };

        let status = std::str::from_utf8(status)
            .with_context(|| format!("git diff-tree emitted non-UTF-8 status {status:?}"))?;
        let status_kind = status.as_bytes().first().copied().unwrap_or_default();
        let kind = match status_kind {
            b'A' => FileChangeKind::Added,
            b'M' | b'T' => FileChangeKind::Modified,
            b'D' => FileChangeKind::Deleted,
            b'R' => FileChangeKind::Renamed {
                from: pathbuf_from_git_bytes(path1),
            },
            other => bail!(
                "unexpected diff status `{}` in `{status}`",
                char::from(other)
            ),
        };
        let path = match &kind {
            FileChangeKind::Renamed { .. } => {
                let path2 = fields.next().with_context(|| {
                    format!("git diff-tree emitted rename `{status}` without a destination path")
                })?;
                pathbuf_from_git_bytes(path2)
            }
            FileChangeKind::Added | FileChangeKind::Modified | FileChangeKind::Deleted => {
                pathbuf_from_git_bytes(path1)
            }
        };

        changes.push(FileChange {
            path,
            kind,
            parent_sha: parent_sha.clone(),
        });
    }

    Ok(())
}

fn status_and_optional_path(field: &[u8]) -> (&[u8], Option<&[u8]>) {
    match field.iter().position(|b| *b == b'\t') {
        Some(tab) => (&field[..tab], Some(&field[tab + 1..])),
        None => (field, None),
    }
}

fn nul_fields(stdout: &[u8]) -> impl Iterator<Item = &[u8]> {
    stdout.split(|b| *b == 0).filter(|field| !field.is_empty())
}

#[cfg(unix)]
fn pathbuf_from_git_bytes(path: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;

    PathBuf::from(std::ffi::OsStr::from_bytes(path))
}

#[cfg(not(unix))]
fn pathbuf_from_git_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

fn git_dir(worktree: &Path) -> Result<std::path::PathBuf> {
    let stdout = run_git(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    )?;
    Ok(std::path::PathBuf::from(stdout.trim()))
}

pub(crate) fn run_git_bytes(worktree: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(worktree)
        .args(args)
        .output()
        .with_context(|| format!("spawn git {args:?} in `{}`", worktree.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "git {:?} failed in `{}`: {}",
            args,
            worktree.display(),
            stderr.trim()
        ));
    }

    Ok(output.stdout)
}

pub(crate) fn run_git(worktree: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(worktree)
        .args(args)
        .output()
        .with_context(|| format!("spawn git {args:?} in `{}`", worktree.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "git {:?} failed in `{}`: {}",
            args,
            worktree.display(),
            stderr.trim()
        ));
    }

    String::from_utf8(output.stdout)
        .with_context(|| format!("git {args:?} emitted non-UTF-8 stdout"))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "T"],
        ] {
            run_git(dir, &args).unwrap();
        }
    }

    fn commit(dir: &std::path::Path, msg: &str) -> String {
        run_git(dir, &["add", "-A"]).unwrap();
        run_git(dir, &["commit", "-q", "--allow-empty", "-m", msg]).unwrap();
        run_git(dir, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn snapshot_refs_returns_main_tip() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let sha = commit(dir.path(), "init");

        let snap = snapshot_refs(dir.path(), &["main"]).unwrap();

        assert_eq!(snap.get("main"), Some(&sha));
    }

    #[test]
    fn fail_closed_on_shallow_clone() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let sha = commit(dir.path(), "init");
        std::fs::write(dir.path().join(".git/shallow"), format!("{sha}\n")).unwrap();

        let err = ensure_not_shallow(dir.path()).unwrap_err();

        assert!(
            err.to_string().contains("refusing to index shallow clone"),
            "{err:#}"
        );
    }

    #[test]
    fn fail_closed_on_missing_target_ref() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());

        let err = snapshot_refs(dir.path(), &["main"]).unwrap_err();

        assert!(
            err.to_string().contains("target ref `main` does not exist"),
            "{err:#}"
        );
    }

    #[test]
    fn file_diff_initial_commit_marks_all_added() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), b"fn a() {}").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"hi").unwrap();
        let sha = commit(dir.path(), "init");

        let changes = file_changes_for_commit(dir.path(), &sha).unwrap();

        let mut paths: Vec<_> = changes.iter().map(|c| (&c.path, &c.kind)).collect();
        paths.sort_by_key(|(p, _)| p.to_string_lossy().to_string());
        assert_eq!(paths.len(), 2);
        assert!(matches!(paths[0].1, FileChangeKind::Added));
    }

    #[test]
    fn file_diff_rename_detected() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("old.rs"), b"fn x() {}").unwrap();
        commit(dir.path(), "init");
        std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
        let sha = commit(dir.path(), "rename");

        let changes = file_changes_for_commit(dir.path(), &sha).unwrap();

        let r = changes.iter().find(|c| c.path.ends_with("new.rs")).unwrap();
        assert!(matches!(&r.kind, FileChangeKind::Renamed { from } if from.ends_with("old.rs")));
    }
}
