//! Project-root discovery for SPUR writers.
//!
//! Every SPUR writer (`event_sink`, ACP stderr capture, tracing log dir,
//! orphan-sweeper, etc.) anchors its on-disk artifacts under
//! `<repo_root>/.spur/`. Historically several callers computed
//! `.spur/...` relative to `std::env::current_dir()` directly, which
//! produced a nested `.spur/.spur/...` tree when SPUR was launched from
//! inside one of those subdirectories (e.g. a hook, a test harness, or
//! a developer who `cd`'d into `.spur/events/` to tail a file).
//!
//! `discover()` walks upward from a caller-supplied start directory,
//! skipping any `.spur` ancestor segments and stopping at the nearest
//! ancestor that already contains a `.spur/` child. Falls back to
//! `git rev-parse --show-toplevel` and finally to the rewritten start
//! dir itself.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Discover the project root anchored to `start_dir`. Pure on the
/// filesystem; reads no environment variables, never panics.
///
/// Contract:
/// 1. If any `Component::Normal` segment of `start_dir` is exactly
///    `.spur`, the function rewinds the cursor to the parent of that
///    component before searching. The returned root is guaranteed not
///    to have a `.spur` segment anywhere in its path.
/// 2. From the (possibly-rewritten) cursor, walks ancestors and
///    returns the nearest one that already has a `.spur/` child dir.
/// 3. Falls back to `git rev-parse --show-toplevel` run from the
///    rewritten cursor.
/// 4. Final fallback: the rewritten cursor itself.
///
/// Returns `Err` if the *final* resolved root still contains `.spur`
/// as a path component — the belt-and-suspenders guard that protects
/// any future writer which bypasses the project_root helper.
pub fn discover(start_dir: &Path) -> Result<PathBuf, DiscoverError> {
    let rewritten = strip_spur_components(start_dir);
    let root = discover_from(&rewritten);
    if has_bad_spur_component(&root) {
        tracing::error!(
            start = %start_dir.display(),
            rewritten = %rewritten.display(),
            resolved = %root.display(),
            "project_root::discover resolved to a path containing a `.spur` component; refusing to initialize",
        );
        return Err(DiscoverError::ContainsSpurComponent { path: root });
    }
    Ok(root)
}

/// Warn once at startup if prior buggy runs left nested SPUR artifacts
/// under canonical artifact directories.
///
/// Returns the number of nested locations detected, so callers can unit-test
/// detection without installing a tracing collector.
pub fn warn_on_nested_layout(repo_root: &Path) -> usize {
    let mut count = 0;
    for location in [
        NestedLayoutLocation {
            name: "events",
            glob: "*.ndjson",
        },
        NestedLayoutLocation {
            name: "logs",
            glob: "*",
        },
    ] {
        let canonical_dir = repo_root.join(".spur").join(location.name);
        let nested_spur = canonical_dir.join(".spur");
        if !nested_spur.exists() {
            continue;
        }

        count += 1;
        let nested_artifact_dir = nested_spur.join(location.name);
        let move_from = nested_artifact_dir.join(location.glob);
        let suggestion = format!(
            "mv {} {}/ && rmdir {} {}",
            move_from.display(),
            canonical_dir.display(),
            nested_artifact_dir.display(),
            nested_spur.display()
        );
        tracing::warn!(
            nested_path = %nested_spur.display(),
            suggestion = %suggestion,
            "nested SPUR layout detected: a prior SPUR run launched with CWD inside .spur/; events from that run are not visible to the live picker until moved"
        );
    }
    count
}

struct NestedLayoutLocation {
    name: &'static str,
    glob: &'static str,
}

/// Error returned by [`discover`] when the resolved root would still
/// be nested inside a `.spur/` segment — should be structurally
/// impossible given the rewrite, but treated as a hard refusal for
/// belt-and-suspenders.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("resolved project root still contains a `.spur` path component: {}", path.display())]
    ContainsSpurComponent { path: PathBuf },
}

fn discover_from(start: &Path) -> PathBuf {
    // (b) walk ancestors looking for an existing `.spur` dir
    for ancestor in start.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        if ancestor.join(".spur").is_dir() {
            return ancestor.to_path_buf();
        }
    }
    // (c) fall back to git toplevel
    if let Some(top) = git_toplevel(start) {
        let stripped = strip_spur_components(&top);
        if !has_bad_spur_component(&stripped) {
            return stripped;
        }
    }
    // (d) final fallback: the rewritten start itself
    start.to_path_buf()
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Rewrite `p` to a prefix that strictly precedes the first `.spur`
/// normal component, if any. If `p` has no `.spur` component, returns
/// `p` unchanged. If `p`'s only component is `.spur`, returns the
/// empty `PathBuf` — callers should treat that as cwd.
fn strip_spur_components(p: &Path) -> PathBuf {
    if !has_spur_component(p) {
        return p.to_path_buf();
    }
    let comps: Vec<_> = p.components().collect();
    let mut out = PathBuf::new();
    for (i, comp) in comps.iter().enumerate() {
        if let Component::Normal(seg) = comp {
            if *seg == std::ffi::OsStr::new(".spur") {
                // Look ahead one component. If it's `worktrees`, this is
                // a legitimate worktree path — keep traversing so the
                // ancestor walk can stop at the worktree's own `.spur/`.
                let next_is_worktrees = comps.get(i + 1).is_some_and(|c| {
                    matches!(c, Component::Normal(s) if *s == std::ffi::OsStr::new("worktrees"))
                });
                if !next_is_worktrees {
                    break;
                }
            }
        }
        out.push(comp.as_os_str());
    }
    out
}

fn has_spur_component(p: &Path) -> bool {
    p.components()
        .any(|c| matches!(c, Component::Normal(seg) if seg == std::ffi::OsStr::new(".spur")))
}

/// Returns true if `p` contains a `.spur` component that is NOT
/// immediately followed by `worktrees`. Legitimate worktree paths like
/// `<outer>/.spur/worktrees/<uuid>` are allowed; everything else
/// (`.spur/events`, `.spur/logs`, `.spur` as a leaf, etc.) is rejected.
fn has_bad_spur_component(p: &Path) -> bool {
    let comps: Vec<_> = p.components().collect();
    for (i, c) in comps.iter().enumerate() {
        if let Component::Normal(seg) = c {
            if *seg == std::ffi::OsStr::new(".spur") {
                let next_is_worktrees = comps.get(i + 1).is_some_and(|n| {
                    matches!(n, Component::Normal(s) if *s == std::ffi::OsStr::new("worktrees"))
                });
                if !next_is_worktrees {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (a) start_dir already at repo root with `.spur/` present.
    #[test]
    fn discover_at_root_with_spur_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".spur")).unwrap();
        let got = discover(&root).unwrap();
        assert_eq!(got, root);
    }

    /// (b) start_dir nested inside `.spur/events`.
    #[test]
    fn discover_strips_spur_events_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let nested = root.join(".spur").join("events");
        std::fs::create_dir_all(&nested).unwrap();
        let got = discover(&nested).unwrap();
        assert_eq!(got, root);
        assert!(!has_spur_component(&got));
    }

    /// (c) start_dir several levels deep in a normal subdirectory.
    #[test]
    fn discover_climbs_ancestors_to_spur_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".spur")).unwrap();
        let deep = root.join("crates").join("foo").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        let got = discover(&deep).unwrap();
        assert_eq!(got, root);
    }

    /// (d) no `.spur/` and no git root → falls back to start dir.
    #[test]
    fn discover_falls_back_to_start_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let got = discover(&dir).unwrap();
        assert_eq!(got, dir);
    }

    /// (e) `.spur` as a path component is stripped even at the leaf.
    #[test]
    fn strip_spur_returns_empty_when_only_component_is_spur() {
        let p = PathBuf::from(".spur");
        let stripped = strip_spur_components(&p);
        assert_eq!(stripped, PathBuf::new());
        assert!(!has_spur_component(&stripped));
    }

    /// Worktree case: `.spur/worktrees/<uuid>/.spur/` must resolve to
    /// the worktree's own root, NOT climb into the outer project.
    #[test]
    fn worktree_resolves_to_own_root_not_parent_project() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(outer.join(".spur")).unwrap();
        let worktree = outer.join(".spur").join("worktrees").join("uuid-xyz");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir(worktree.join(".spur")).unwrap();

        let got = discover(&worktree).unwrap();
        assert_eq!(
            got, worktree,
            "must resolve to worktree's own root, not parent project"
        );
    }

    /// Regression: the worktree carve-out must not also disable the
    /// original bug fix for stale CWDs inside `.spur/events`.
    #[test]
    fn discover_strips_when_inside_spur_events_not_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".spur").join("events")).unwrap();
        let got = discover(&root.join(".spur").join("events")).unwrap();
        assert_eq!(got, root);
    }

    #[test]
    fn discover_rejects_root_with_spur_component_belt_and_suspenders() {
        // Direct test of the guard via internals: simulate a final root
        // that still has `.spur` (unreachable from `discover()` because
        // `strip_spur_components` runs first, but the guard is the
        // safety net for future refactors).
        let bad = PathBuf::from("/tmp/.spur/inside");
        let ok_worktree = PathBuf::from("/tmp/.spur/worktrees/uuid");
        assert!(has_bad_spur_component(&bad));
        assert!(!has_bad_spur_component(&ok_worktree));
    }

    #[test]
    fn strip_spur_preserves_path_before_spur() {
        let p = PathBuf::from("/a/b/.spur/events/foo");
        let stripped = strip_spur_components(&p);
        assert_eq!(stripped, PathBuf::from("/a/b"));
    }

    #[test]
    fn warn_on_nested_layout_detects_nested_events_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(
            root.join(".spur")
                .join("events")
                .join(".spur")
                .join("events"),
        )
        .unwrap();
        std::fs::write(
            root.join(".spur")
                .join("events")
                .join(".spur")
                .join("events")
                .join("foo.ndjson"),
            "{}\n",
        )
        .unwrap();

        assert_eq!(warn_on_nested_layout(&root), 1);
    }
}
