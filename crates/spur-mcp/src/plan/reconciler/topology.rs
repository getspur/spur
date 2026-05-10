//! bd-88r — Compile verified git topology for `PlanTaskBlockedOnSetupConflict`
//! continuations so the brain does not hallucinate commit parentage or diff
//! scope during conflict recovery.

use std::path::Path;

use spur_acp::domain::continuation::{ApprovedTaskGitNode, SetupConflictTopology};
use spur_acp::domain::events::DiffSummary;

use crate::plan::{PlanState, PlanTaskStatus};

/// Build a `SetupConflictTopology` by running git verification commands.
///
/// For each approved task (in topological order) we record:
/// - `tip_oid` and `parent_oid` (first parent) via `git rev-parse`
/// - cumulative diff stat (`base_oid..tip_oid`)
/// - incremental diff stat (`parent_oid..tip_oid`)
/// - a flattening heuristic (`cumulative >> incremental`)
///
/// The result is sent to the brain in the `ContinuationPayload` so it can
/// reason about `plan_truncate_and_restart` or `submit_plan_mutation`
/// without running its own (often hallucinated) git commands.
pub(super) async fn compile_setup_conflict_topology(
    plan: &PlanState,
    repo_root: &Path,
    blocked_task_id: &str,
    conflict_dep_task_id: &str,
    conflict_files: &[String],
) -> anyhow::Result<SetupConflictTopology> {
    let base_oid = resolve_base_oid(plan, repo_root).await?;

    let approved_in_topo_order: Vec<&crate::plan::PlanTaskEntry> = plan
        .topo_ordered_tasks()
        .into_iter()
        .filter(|entry| matches!(entry.status, PlanTaskStatus::Approved { .. }))
        .filter(|entry| entry.worker_branch.is_some() && entry.dispatched_base_oid.is_some())
        .collect();

    let mut approved_chain = Vec::with_capacity(approved_in_topo_order.len());

    for entry in &approved_in_topo_order {
        let task_id = entry.spec.task_id.clone();
        let worker_branch = entry
            .worker_branch
            .as_ref()
            .expect("filtered for worker_branch")
            .clone();
        let tip_oid = super::base_spec::git_rev_parse(repo_root, &worker_branch).await?;
        let parent_oid = git_first_parent(repo_root, &tip_oid).await?;

        let cumulative_diff_stat =
            git_diff_stat(repo_root, &format!("{base_oid}..{tip_oid}")).await?;
        let incremental_diff_stat =
            git_diff_stat(repo_root, &format!("{parent_oid}..{tip_oid}")).await?;

        // Flattening heuristic: when the incremental diff is non-empty but
        // substantially smaller than the cumulative diff, the tip likely
        // bundles content from transitive deps (v0 engine behaviour).
        let cumulative_total = cumulative_diff_stat.insertions + cumulative_diff_stat.deletions;
        let incremental_total = incremental_diff_stat.insertions + incremental_diff_stat.deletions;
        let appears_flattened = incremental_total > 0
            && incremental_total < cumulative_total
            && (cumulative_total - incremental_total) > 50;

        approved_chain.push(ApprovedTaskGitNode {
            task_id,
            worker_branch,
            tip_oid,
            parent_oid,
            cumulative_diff_stat,
            incremental_diff_stat,
            appears_flattened,
        });
    }

    Ok(SetupConflictTopology {
        base_oid,
        blocked_task_id: blocked_task_id.to_string(),
        conflict_dep_task_id: conflict_dep_task_id.to_string(),
        conflict_files: conflict_files.to_vec(),
        approved_chain,
    })
}

async fn resolve_base_oid(plan: &PlanState, repo_root: &Path) -> anyhow::Result<String> {
    if let Some(ref oid) = plan.base_snapshot_oid {
        if super::base_spec::is_hex_oid(oid) {
            return Ok(oid.clone());
        }
        return super::base_spec::git_rev_parse(repo_root, oid).await;
    }
    if let Some(ref branch) = plan.base_snapshot_branch {
        return super::base_spec::git_rev_parse(repo_root, branch).await;
    }
    super::base_spec::git_rev_parse(repo_root, "HEAD").await
}

async fn git_first_parent(repo_root: &Path, commit: &str) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", &format!("{commit}^")])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|error| anyhow::anyhow!("failed to execute git rev-parse {commit}^: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        anyhow::bail!(
            "git rev-parse {commit}^ failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

/// Parse `git diff --stat` output into a `DiffSummary`.
///
/// Example last line:
///   `2 files changed, 7 insertions(+), 6 deletions(-)`
/// or:
///   `1 file changed, 3 insertions(+)`
async fn git_diff_stat(repo_root: &Path, range: &str) -> anyhow::Result<DiffSummary> {
    let output = tokio::process::Command::new("git")
        .args([
            "diff",
            "--stat-width=1000",
            "--stat-count=1000",
            "--stat",
            range,
        ])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|error| anyhow::anyhow!("failed to execute git diff --stat {range}: {error}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff --stat {range} failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut files_changed = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    if let Some(summary_line) = lines.last() {
        // Collect file paths from all non-summary, non-empty lines.
        for line in &lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Summary line contains "changed" and does not have a pipe.
            if line.contains("changed") && !line.contains(" | ") {
                continue;
            }
            // A file line looks like: " path/to/file | 12 +++---"
            if let Some(pipe_pos) = line.find(" | ") {
                let path_part = line[..pipe_pos].trim();
                if !path_part.is_empty() {
                    files.push(path_part.into());
                }
            }
        }

        let summary = summary_line.trim();
        // Parse: "N file(s) changed, I insertion(s)(+), D deletion(s)(-)"
        for part in summary.split(", ") {
            let part = part.trim();
            if let Some(n) = parse_stat_number(part, "file changed") {
                files_changed = n;
            } else if let Some(n) = parse_stat_number(part, "insertion(+)") {
                insertions = n;
            } else if let Some(n) = parse_stat_number(part, "deletion(-)") {
                deletions = n;
            }
        }
    }

    Ok(DiffSummary {
        files_changed,
        insertions,
        deletions,
        files,
    })
}

fn parse_stat_number(part: &str, keyword: &str) -> Option<usize> {
    // git diff --stat uses both singular and plural forms.
    let suffixes: &[&str] = match keyword {
        "file changed" => &["file changed", "files changed"],
        "insertion(+)" => &["insertion(+)", "insertions(+)"],
        "deletion(-)" => &["deletion(-)", "deletions(-)"],
        _ => &[keyword],
    };
    for suffix in suffixes {
        if part.ends_with(suffix) {
            return part.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_diff_stat_summary_parses_all_components() {
        let summary = "2 files changed, 7 insertions(+), 6 deletions(-)";
        for part in summary.split(", ") {
            if parse_stat_number(part, "file changed").is_some() {
                assert_eq!(parse_stat_number(part, "file changed"), Some(2));
            }
            if parse_stat_number(part, "insertion(+)").is_some() {
                assert_eq!(parse_stat_number(part, "insertion(+)"), Some(7));
            }
            if parse_stat_number(part, "deletion(-)").is_some() {
                assert_eq!(parse_stat_number(part, "deletion(-)"), Some(6));
            }
        }
    }

    #[test]
    fn parse_diff_stat_summary_single_file() {
        let summary = "1 file changed, 3 insertions(+)";
        let mut found_file = None;
        let mut found_ins = None;
        let mut found_del = None;
        for part in summary.split(", ") {
            found_file = found_file.or(parse_stat_number(part, "file changed"));
            found_ins = found_ins.or(parse_stat_number(part, "insertion(+)"));
            found_del = found_del.or(parse_stat_number(part, "deletion(-)"));
        }
        assert_eq!(found_file, Some(1));
        assert_eq!(found_ins, Some(3));
        assert_eq!(found_del, None);
    }

    #[test]
    fn parse_diff_stat_summary_no_changes() {
        let summary = "0 files changed";
        let mut found_file = None;
        let mut found_ins = None;
        let mut found_del = None;
        for part in summary.split(", ") {
            found_file = found_file.or(parse_stat_number(part, "file changed"));
            found_ins = found_ins.or(parse_stat_number(part, "insertion(+)"));
            found_del = found_del.or(parse_stat_number(part, "deletion(-)"));
        }
        assert_eq!(found_file, Some(0));
        assert_eq!(found_ins, None);
        assert_eq!(found_del, None);
    }
}
