//! Brain-side clobber detector for plan task review.
//!
//! Fires `PotentialClobber` signals when a worker creates or modifies a
//! file that overlaps non-trivially with an already-approved upstream
//! task's tip. See `docs/superpowers/specs/2026-05-01-bd-1dwm-design.md`
//! Phase 0 (D - clobber detector).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use uuid::Uuid;

use crate::plan::signals::WorkerSignal;

/// One upstream approved task's tip used as a clobber-baseline.
#[derive(Debug, Clone)]
pub struct PriorTip {
    pub task_id: String,
    pub branch_name: String,
    pub tip_oid: String,
}

/// Result of running the detector against a worker branch.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorReport {
    pub signals: Vec<WorkerSignal>,
}

/// Minimum byte-size of overlap required to flag a file as a potential
/// clobber. Files smaller than this on the upstream tip are ignored
/// (avoids false positives for trivial files like `mod.rs` re-exports).
pub const MIN_NONTRIVIAL_BYTES: usize = 64;

/// Run the clobber detector against a worker branch.
///
/// Compares files in the worker branch against the same file on each prior
/// approved tip. Emits a signal when:
/// - the file exists on a prior approved tip with content size >=
///   MIN_NONTRIVIAL_BYTES
/// - AND the worker's content for that file differs from the prior tip's
///   content, judged by blob OID inequality.
pub fn run(repo: &Path, worker_branch: &str, priors: &[PriorTip]) -> DetectorReport {
    let mut signals = Vec::new();
    let worker_files = worker_candidate_files(repo, worker_branch);
    let worker_tip =
        git_rev_parse(repo, worker_branch).unwrap_or_else(|_| worker_branch.to_string());

    for prior in priors {
        let upstream_files = git_ls_tree_files(repo, &prior.branch_name).unwrap_or_default();
        for file in &worker_files {
            let Some(upstream_file) = upstream_files.get(file) else {
                continue;
            };
            if upstream_file.size < MIN_NONTRIVIAL_BYTES {
                continue;
            }

            let Ok(upstream_blob) = git_blob_oid(repo, &prior.branch_name, file) else {
                continue;
            };
            let Ok(worker_blob) = git_blob_oid(repo, worker_branch, file) else {
                continue;
            };
            if upstream_blob == worker_blob {
                continue;
            }

            signals.push(WorkerSignal::PotentialClobber {
                signal_id: Uuid::new_v4(),
                conflicting_task_id: prior.task_id.clone(),
                file: file.clone(),
                upstream_tip: prior.tip_oid.clone(),
                worker_tip: worker_tip.clone(),
            });
        }
    }

    DetectorReport { signals }
}

#[derive(Debug)]
struct GitFileEntry {
    size: usize,
}

fn worker_candidate_files(repo: &Path, worker_branch: &str) -> Vec<String> {
    // Find the fork point relative to the project's default branch.
    // Try common defaults; emit warning + fall back to HEAD if none resolve so
    // the detector is observable when running on non-conventional repos.
    let fork_point = ["main", "master", "trunk"]
        .iter()
        .find_map(|candidate| git_merge_base(repo, candidate, worker_branch).ok())
        .or_else(|| {
            tracing::warn!(
                worker_branch = %worker_branch,
                "clobber_detector: no merge-base against main/master/trunk; falling back to HEAD (detector may emit zero signals if HEAD is the worker tip)",
            );
            git_rev_parse(repo, "HEAD").ok()
        })
        .unwrap_or_default();
    if fork_point.is_empty() {
        return Vec::new();
    }
    git_diff_name_only(repo, &fork_point, worker_branch).unwrap_or_default()
}

fn git_merge_base(repo: &Path, a: &str, b: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["merge-base", a, b])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git merge-base failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git merge-base {a} {b} exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_diff_name_only(repo: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["diff", "--name-only", base, head])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git diff exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_ls_tree_files(repo: &Path, rev: &str) -> Result<HashMap<String, GitFileEntry>, String> {
    let out = Command::new("git")
        .args(["ls-tree", "-l", "-r", rev])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git ls-tree failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-tree exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let mut map = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let size = meta
            .split_whitespace()
            .nth(3)
            .and_then(|size| size.parse::<usize>().ok())
            .unwrap_or(0);
        if !path.is_empty() {
            map.insert(path.to_string(), GitFileEntry { size });
        }
    }

    Ok(map)
}

fn git_blob_oid(repo: &Path, rev: &str, path: &str) -> Result<String, String> {
    let spec = format!("{rev}:{path}");
    git_rev_parse(repo, &spec)
}

fn git_rev_parse(repo: &Path, spec: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["rev-parse", spec])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("git rev-parse failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README"), "init\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn commit_file(repo: &std::path::Path, branch: &str, path: &str, content: &str) -> String {
        run_git(repo, &["checkout", "-q", "-B", branch, "main"]);
        std::fs::write(repo.join(path), content).unwrap();
        run_git(repo, &["add", path]);
        run_git(repo, &["commit", "-q", "-m", &format!("add {path}")]);
        run_git(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn detector_flags_overlapping_nontrivial_file() {
        let dir = init_repo();
        let upstream_tip = commit_file(dir.path(), "upstream", "foo.rs", &"a".repeat(200));
        let _worker_tip = commit_file(dir.path(), "worker", "foo.rs", &"b".repeat(200));
        let priors = vec![PriorTip {
            task_id: "upstream".into(),
            branch_name: "upstream".into(),
            tip_oid: upstream_tip,
        }];
        let report = run(dir.path(), "worker", &priors);
        assert_eq!(report.signals.len(), 1);
        match &report.signals[0] {
            WorkerSignal::PotentialClobber {
                conflicting_task_id,
                file,
                ..
            } => {
                assert_eq!(conflicting_task_id, "upstream");
                assert_eq!(file, "foo.rs");
            }
            _ => panic!("expected PotentialClobber"),
        }
    }

    #[test]
    fn detector_ignores_trivial_files() {
        let dir = init_repo();
        let upstream_tip = commit_file(dir.path(), "upstream", "tiny.rs", "x");
        let _worker_tip = commit_file(dir.path(), "worker", "tiny.rs", "y");
        let priors = vec![PriorTip {
            task_id: "upstream".into(),
            branch_name: "upstream".into(),
            tip_oid: upstream_tip,
        }];
        let report = run(dir.path(), "worker", &priors);
        assert!(report.signals.is_empty(), "trivial files should not flag");
    }

    #[test]
    fn detector_ignores_disjoint_files() {
        let dir = init_repo();
        let upstream_tip = commit_file(dir.path(), "upstream", "foo.rs", &"a".repeat(200));
        let _worker_tip = commit_file(dir.path(), "worker", "bar.rs", &"b".repeat(200));
        let priors = vec![PriorTip {
            task_id: "upstream".into(),
            branch_name: "upstream".into(),
            tip_oid: upstream_tip,
        }];
        let report = run(dir.path(), "worker", &priors);
        assert!(report.signals.is_empty(), "disjoint files should not flag");
    }

    #[test]
    fn detector_ignores_identical_content() {
        let dir = init_repo();
        let same = "a".repeat(200);
        let upstream_tip = commit_file(dir.path(), "upstream", "foo.rs", &same);
        let _worker_tip = commit_file(dir.path(), "worker", "foo.rs", &same);
        let priors = vec![PriorTip {
            task_id: "upstream".into(),
            branch_name: "upstream".into(),
            tip_oid: upstream_tip,
        }];
        let report = run(dir.path(), "worker", &priors);
        assert!(
            report.signals.is_empty(),
            "identical content should not flag"
        );
    }

    #[test]
    fn detector_emits_one_signal_per_clobbering_prior() {
        let dir = init_repo();

        let _t1 = commit_file(dir.path(), "task1", "foo.rs", &"T1 content ".repeat(50));
        let t1_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let _t2 = commit_file(dir.path(), "task2", "foo.rs", &"T2 content ".repeat(50));
        let t2_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let _w = commit_file(
            dir.path(),
            "worker",
            "foo.rs",
            &"Worker content ".repeat(50),
        );

        let priors = vec![
            PriorTip {
                task_id: "task1".into(),
                branch_name: "task1".into(),
                tip_oid: t1_tip,
            },
            PriorTip {
                task_id: "task2".into(),
                branch_name: "task2".into(),
                tip_oid: t2_tip,
            },
        ];
        let report = run(dir.path(), "worker", &priors);

        assert_eq!(
            report.signals.len(),
            2,
            "expected one signal per clobbering prior"
        );
        let conflicting_ids: std::collections::HashSet<&str> = report
            .signals
            .iter()
            .filter_map(|s| match s {
                WorkerSignal::PotentialClobber {
                    conflicting_task_id,
                    ..
                } => Some(conflicting_task_id.as_str()),
                _ => None,
            })
            .collect();
        assert!(conflicting_ids.contains("task1"));
        assert!(conflicting_ids.contains("task2"));
    }
}
