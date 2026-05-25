//! Parity audit: compare CLI `git diff-tree --raw --find-renames` output against
//! `gix` tree diffs with rename detection, across every commit reachable from
//! HEAD in a target repository.
//!
//! Run with:
//!   cargo run -p spur-graph --example gix_parity_audit -- /path/to/repo
//!
//! Outputs:
//!   - stdout: per-1k-commits progress and the final summary
//!   - <repo>/.spur/gix_parity_report.json (machine-readable)
//!   - <repo>/.spur/gix_parity_report.md  (human-readable summary)
//!
//! gix feature notes:
//! - `revision` is kept from the migration spike request for commit APIs.
//! - `blob-diff` is required in gix 0.77 for `Tree::changes()` and rewrite
//!   tracking (`gix::diff::Rewrites`).
//! - `max-performance-safe` enables the fast safe object-access feature set.
//!
//! Replace-ref notes:
//! - The repository is opened with `core.useReplaceRefs=true` as a CLI override.
//!   In gix 0.77 this maps to the same replacement-object bypass path as
//!   `GIT_NO_REPLACE_OBJECTS`, so no `refs/replace/*` mappings are installed in
//!   the object database.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use gix::object::tree::diff::{Action, Change};
use gix::objs::tree::EntryMode;
use serde::Serialize;
use spur_graph::git_walk::{file_changes_for_commit, FileChange, FileChangeKind};
use spur_graph::schema::GitPath;

const REPORT_JSON: &str = "gix_parity_report.json";
const REPORT_MD: &str = "gix_parity_report.md";
const SAMPLE_COMMITS_PER_CATEGORY: usize = 5;
const SAMPLE_DIFFS_PER_COMMIT: usize = 12;

fn main() -> Result<()> {
    let repo_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: gix_parity_audit <repo_path>");
    let repo_path = repo_path
        .canonicalize()
        .with_context(|| format!("canonicalize repo path `{}`", repo_path.display()))?;

    let head = git_stdout(&repo_path, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let commits = list_commits(&repo_path)?;
    println!(
        "auditing {} commits in {} at HEAD {}",
        commits.len(),
        repo_path.display(),
        short_sha(&head)
    );

    let mut repo = gix::open_opts(
        &repo_path,
        gix::open::Options::default().cli_overrides(["core.useReplaceRefs=true"]),
    )
    .with_context(|| format!("open gix repository `{}`", repo_path.display()))?;
    repo.object_cache_size_if_unset(128 * 1024 * 1024);

    let mut commit_reports = Vec::with_capacity(commits.len());
    let mut identical_so_far = 0usize;

    for (index, sha) in commits.iter().enumerate() {
        let metadata = gix_commit_metadata(&repo, sha).unwrap_or_else(|err| CommitMetadata {
            parent_count: 0,
            subject: format!("metadata error: {err:#}"),
        });

        let cli_result = file_changes_for_commit(&repo_path, sha)
            .with_context(|| format!("CLI file changes for {}", short_sha(sha)));
        let gix_result = gix_file_changes_for_commit(&repo, sha)
            .with_context(|| format!("gix file changes for {}", short_sha(sha)));

        let report = match (cli_result, gix_result) {
            (Ok(cli_changes), Ok(gix_changes)) => {
                let differences = compare_changes(&cli_changes, &gix_changes);
                let identical = differences.is_empty();
                if identical {
                    identical_so_far += 1;
                }
                CommitParity {
                    sha: sha.clone(),
                    subject: metadata.subject,
                    parent_count: metadata.parent_count,
                    cli_count: cli_changes.len(),
                    gix_count: gix_changes.len(),
                    identical,
                    differences,
                    error: None,
                }
            }
            (cli_result, gix_result) => {
                let cli_count = cli_result.as_ref().map_or(0, Vec::len);
                let gix_count = gix_result.as_ref().map_or(0, Vec::len);
                let mut errors = Vec::new();
                if let Err(err) = cli_result {
                    errors.push(format!("cli: {err:#}"));
                }
                if let Err(err) = gix_result {
                    errors.push(format!("gix: {err:#}"));
                }
                CommitParity {
                    sha: sha.clone(),
                    subject: metadata.subject,
                    parent_count: metadata.parent_count,
                    cli_count,
                    gix_count,
                    identical: false,
                    differences: Vec::new(),
                    error: Some(errors.join("; ")),
                }
            }
        };

        commit_reports.push(report);
        let scanned = index + 1;
        if scanned % 1000 == 0 || scanned == commits.len() {
            println!(
                "scanned {scanned}/{} commits; identical so far: {} ({:.2}%)",
                commits.len(),
                identical_so_far,
                percent(identical_so_far, scanned)
            );
        }
    }

    let report = build_report(repo_path.clone(), head, commit_reports);
    let report_dir = repo_path.join(".spur");
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("create report dir `{}`", report_dir.display()))?;
    let json_path = report_dir.join(REPORT_JSON);
    let md_path = report_dir.join(REPORT_MD);
    fs::write(&json_path, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("write `{}`", json_path.display()))?;
    fs::write(&md_path, render_markdown(&report))
        .with_context(|| format!("write `{}`", md_path.display()))?;

    print_summary(&report, &json_path, &md_path);
    Ok(())
}

fn list_commits(repo_path: &Path) -> Result<Vec<String>> {
    let stdout = git_stdout(
        repo_path,
        &["rev-list", "--topo-order", "--reverse", "HEAD"],
    )?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn git_stdout(repo_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(args)
        .output()
        .with_context(|| format!("spawn git {args:?} in `{}`", repo_path.display()))?;
    if !output.status.success() {
        bail!(
            "git {:?} failed with status {}: {}",
            args,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("git {args:?} emitted non-UTF-8 stdout"))
}

struct CommitMetadata {
    parent_count: usize,
    subject: String,
}

fn gix_commit_metadata(repo: &gix::Repository, sha: &str) -> Result<CommitMetadata> {
    let oid = gix::ObjectId::from_hex(sha.as_bytes())?;
    let commit = repo.find_commit(oid)?;
    let parent_count = commit.parent_ids().count();
    Ok(CommitMetadata {
        parent_count,
        subject: commit_subject(&commit),
    })
}

fn gix_file_changes_for_commit(repo: &gix::Repository, sha: &str) -> Result<Vec<FileChange>> {
    let oid = gix::ObjectId::from_hex(sha.as_bytes())?;
    let commit = repo.find_commit(oid)?;
    let parent_ids = commit.parent_ids().map(gix::Id::detach).collect::<Vec<_>>();

    if parent_ids.is_empty() {
        return gix_root_commit_changes(&commit);
    }

    let commit_tree = commit.tree()?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;
    let mut out = Vec::new();

    for parent_id in parent_ids {
        let parent = repo.find_commit(parent_id)?;
        let parent_tree = parent.tree()?;
        let parent_sha = parent_id.to_hex().to_string();
        let mut platform = parent_tree.changes()?;
        platform.options(|opts| {
            opts.track_path()
                .track_rewrites(Some(gix::diff::Rewrites::default()));
        });
        platform.for_each_to_obtain_tree_with_cache(
            &commit_tree,
            &mut resource_cache,
            |change| -> std::result::Result<Action, std::convert::Infallible> {
                if let Some(change) = gix_change_to_file_change(change, &parent_sha) {
                    out.push(change);
                }
                Ok(Action::Continue)
            },
        )?;
        resource_cache.clear_resource_cache_keep_allocation();
    }

    Ok(out)
}

fn gix_root_commit_changes(commit: &gix::Commit<'_>) -> Result<Vec<FileChange>> {
    let tree = commit.tree()?;
    let entries = tree.traverse().breadthfirst.files()?;
    Ok(entries
        .into_iter()
        .filter(|entry| !entry.mode.is_tree())
        .map(|entry| {
            let kind = if is_gitlink(entry.mode) {
                FileChangeKind::Gitlink {
                    old_oid: None,
                    new_oid: Some(entry.oid.to_hex().to_string()),
                }
            } else {
                FileChangeKind::Added
            };
            FileChange {
                path: GitPath::from_bytes(entry.filepath.to_vec()),
                kind,
                parent_sha: None,
            }
        })
        .collect())
}

fn gix_change_to_file_change(change: Change<'_, '_, '_>, parent_sha: &str) -> Option<FileChange> {
    let parent_sha = Some(parent_sha.to_string());
    match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return None;
            }
            let kind = if is_gitlink(entry_mode) {
                FileChangeKind::Gitlink {
                    old_oid: None,
                    new_oid: Some(id.to_hex().to_string()),
                }
            } else {
                FileChangeKind::Added
            };
            Some(FileChange {
                path: GitPath::from_bytes(location.to_vec()),
                kind,
                parent_sha,
            })
        }
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return None;
            }
            let kind = if is_gitlink(entry_mode) {
                FileChangeKind::Gitlink {
                    old_oid: Some(id.to_hex().to_string()),
                    new_oid: None,
                }
            } else {
                FileChangeKind::Deleted
            };
            Some(FileChange {
                path: GitPath::from_bytes(location.to_vec()),
                kind,
                parent_sha,
            })
        }
        Change::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            if previous_entry_mode.is_tree() || entry_mode.is_tree() {
                return None;
            }
            let kind = if is_gitlink(previous_entry_mode) || is_gitlink(entry_mode) {
                FileChangeKind::Gitlink {
                    old_oid: Some(previous_id.to_hex().to_string()),
                    new_oid: Some(id.to_hex().to_string()),
                }
            } else {
                FileChangeKind::Modified
            };
            Some(FileChange {
                path: GitPath::from_bytes(location.to_vec()),
                kind,
                parent_sha,
            })
        }
        Change::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            location,
            ..
        } => {
            if source_entry_mode.is_tree() || entry_mode.is_tree() {
                return None;
            }
            let kind = if is_gitlink(source_entry_mode) || is_gitlink(entry_mode) {
                FileChangeKind::Gitlink {
                    old_oid: Some(source_id.to_hex().to_string()),
                    new_oid: Some(id.to_hex().to_string()),
                }
            } else {
                FileChangeKind::Renamed {
                    from: GitPath::from_bytes(source_location.to_vec()),
                }
            };
            Some(FileChange {
                path: GitPath::from_bytes(location.to_vec()),
                kind,
                parent_sha,
            })
        }
    }
}

fn is_gitlink(mode: EntryMode) -> bool {
    mode.value() == 0o160000
}

fn commit_subject(commit: &gix::Commit<'_>) -> String {
    let message = commit.message_raw_sloppy();
    let first_line = message
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(message, |index| &message[..index]);
    String::from_utf8_lossy(first_line).into_owned()
}

#[derive(Debug, Clone)]
struct CommitParity {
    sha: String,
    subject: String,
    parent_count: usize,
    cli_count: usize,
    gix_count: usize,
    identical: bool,
    differences: Vec<Difference>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
enum Difference {
    OnlyInCli {
        change: FileChange,
    },
    OnlyInGix {
        change: FileChange,
    },
    KindDiffers {
        parent_sha: Option<String>,
        path: GitPath,
        cli_kind: FileChangeKind,
        gix_kind: FileChangeKind,
    },
    RenameFromDiffers {
        parent_sha: Option<String>,
        to: GitPath,
        cli_from: GitPath,
        gix_from: GitPath,
    },
}

fn compare_changes(cli_changes: &[FileChange], gix_changes: &[FileChange]) -> Vec<Difference> {
    let mut cli = cli_changes.to_vec();
    let mut gix = gix_changes.to_vec();
    cli.sort_by(compare_file_change);
    gix.sort_by(compare_file_change);

    if cli == gix {
        return Vec::new();
    }

    let mut differences = Vec::new();
    let mut used_gix = vec![false; gix.len()];

    for cli_change in &cli {
        if let Some(index) = gix
            .iter()
            .enumerate()
            .position(|(index, gix_change)| !used_gix[index] && gix_change == cli_change)
        {
            used_gix[index] = true;
            continue;
        }

        if let Some(index) = gix.iter().enumerate().position(|(index, gix_change)| {
            !used_gix[index] && same_parent_and_path(cli_change, gix_change)
        }) {
            used_gix[index] = true;
            differences.push(pair_difference(cli_change, &gix[index]));
        } else {
            differences.push(Difference::OnlyInCli {
                change: cli_change.clone(),
            });
        }
    }

    for (index, gix_change) in gix.into_iter().enumerate() {
        if !used_gix[index] {
            differences.push(Difference::OnlyInGix { change: gix_change });
        }
    }

    differences
}

fn compare_file_change(left: &FileChange, right: &FileChange) -> Ordering {
    left.parent_sha
        .cmp(&right.parent_sha)
        .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
        .then_with(|| kind_rank(&left.kind).cmp(&kind_rank(&right.kind)))
        .then_with(|| kind_detail(&left.kind).cmp(&kind_detail(&right.kind)))
}

fn same_parent_and_path(left: &FileChange, right: &FileChange) -> bool {
    left.parent_sha == right.parent_sha && left.path == right.path
}

fn pair_difference(cli: &FileChange, gix: &FileChange) -> Difference {
    match (&cli.kind, &gix.kind) {
        (
            FileChangeKind::Renamed { from: cli_from },
            FileChangeKind::Renamed { from: gix_from },
        ) if cli_from != gix_from => Difference::RenameFromDiffers {
            parent_sha: cli.parent_sha.clone(),
            to: cli.path.clone(),
            cli_from: cli_from.clone(),
            gix_from: gix_from.clone(),
        },
        _ => Difference::KindDiffers {
            parent_sha: cli.parent_sha.clone(),
            path: cli.path.clone(),
            cli_kind: cli.kind.clone(),
            gix_kind: gix.kind.clone(),
        },
    }
}

fn kind_rank(kind: &FileChangeKind) -> u8 {
    match kind {
        FileChangeKind::Added => 0,
        FileChangeKind::Modified => 1,
        FileChangeKind::Deleted => 2,
        FileChangeKind::Renamed { .. } => 3,
        FileChangeKind::Gitlink { .. } => 4,
    }
}

fn kind_detail(kind: &FileChangeKind) -> Vec<u8> {
    match kind {
        FileChangeKind::Added | FileChangeKind::Modified | FileChangeKind::Deleted => Vec::new(),
        FileChangeKind::Renamed { from } => from.as_bytes().to_vec(),
        FileChangeKind::Gitlink { old_oid, new_oid } => {
            let mut out = Vec::new();
            if let Some(old_oid) = old_oid {
                out.extend_from_slice(old_oid.as_bytes());
            }
            out.push(0);
            if let Some(new_oid) = new_oid {
                out.extend_from_slice(new_oid.as_bytes());
            }
            out
        }
    }
}

#[derive(Serialize)]
struct Report {
    repo: String,
    head: String,
    total_commits: usize,
    identical_commits: usize,
    divergent_commits: usize,
    errored_commits: usize,
    aggregates: Aggregates,
    commits: Vec<CommitParityReport>,
}

#[derive(Serialize)]
struct Aggregates {
    identical_percent: f64,
    divergent_percent: f64,
    breakdown: DivergenceBreakdown,
}

#[derive(Default)]
struct CategoryCounts {
    rename_vs_add_delete: usize,
    rename_from_differs: usize,
    kind_differs: usize,
    count_differs: usize,
    other: usize,
}

#[derive(Default)]
struct CategorySamples {
    rename_vs_add_delete: Vec<CategorySample>,
    rename_from_differs: Vec<CategorySample>,
    kind_differs: Vec<CategorySample>,
    count_differs: Vec<CategorySample>,
    other: Vec<CategorySample>,
}

#[derive(Serialize)]
struct DivergenceBreakdown {
    rename_vs_add_delete: CategoryBreakdown,
    rename_from_differs: CategoryBreakdown,
    kind_differs: CategoryBreakdown,
    count_differs: CategoryBreakdown,
    other: CategoryBreakdown,
}

#[derive(Serialize)]
struct CategoryBreakdown {
    commits: usize,
    percent_of_total: f64,
    samples: Vec<CategorySample>,
}

#[derive(Serialize, Clone)]
struct CategorySample {
    sha: String,
    subject: String,
    differences: Vec<DifferenceReport>,
}

#[derive(Serialize)]
struct CommitParityReport {
    sha: String,
    subject: String,
    parent_count: usize,
    cli_count: usize,
    gix_count: usize,
    identical: bool,
    differences: Vec<DifferenceReport>,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DifferenceReport {
    OnlyInCli {
        change: ChangeReport,
    },
    OnlyInGix {
        change: ChangeReport,
    },
    KindDiffers {
        parent_sha: Option<String>,
        path: PathReport,
        cli_kind: ChangeKindReport,
        gix_kind: ChangeKindReport,
    },
    RenameFromDiffers {
        parent_sha: Option<String>,
        to: PathReport,
        cli_from: PathReport,
        gix_from: PathReport,
    },
}

#[derive(Serialize, Clone)]
struct ChangeReport {
    path: PathReport,
    kind: ChangeKindReport,
    parent_sha: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ChangeKindReport {
    Added,
    Modified,
    Deleted,
    Renamed {
        from: PathReport,
    },
    Gitlink {
        old_oid: Option<String>,
        new_oid: Option<String>,
    },
}

#[derive(Serialize, Clone)]
struct PathReport {
    display: String,
    hex: String,
}

fn build_report(repo_path: PathBuf, head: String, commits: Vec<CommitParity>) -> Report {
    let total_commits = commits.len();
    let identical_commits = commits.iter().filter(|commit| commit.identical).count();
    let errored_commits = commits
        .iter()
        .filter(|commit| commit.error.is_some())
        .count();
    let divergent_commits = total_commits.saturating_sub(identical_commits);

    let mut counts = CategoryCounts::default();
    let mut samples = CategorySamples::default();

    for commit in &commits {
        if commit.identical {
            continue;
        }
        let categories = classify_commit(commit);
        let sample = sample_for_commit(commit);
        let mut categorized = false;

        if categories.rename_vs_add_delete {
            counts.rename_vs_add_delete += 1;
            push_sample(&mut samples.rename_vs_add_delete, sample.clone());
            categorized = true;
        }
        if categories.rename_from_differs {
            counts.rename_from_differs += 1;
            push_sample(&mut samples.rename_from_differs, sample.clone());
            categorized = true;
        }
        if categories.kind_differs {
            counts.kind_differs += 1;
            push_sample(&mut samples.kind_differs, sample.clone());
            categorized = true;
        }
        if categories.count_differs {
            counts.count_differs += 1;
            push_sample(&mut samples.count_differs, sample.clone());
            categorized = true;
        }
        if !categorized && commit.error.is_none() {
            counts.other += 1;
            push_sample(&mut samples.other, sample);
        }
    }

    Report {
        repo: repo_path.display().to_string(),
        head,
        total_commits,
        identical_commits,
        divergent_commits,
        errored_commits,
        aggregates: Aggregates {
            identical_percent: percent(identical_commits, total_commits),
            divergent_percent: percent(divergent_commits, total_commits),
            breakdown: DivergenceBreakdown {
                rename_vs_add_delete: category_breakdown(
                    counts.rename_vs_add_delete,
                    total_commits,
                    samples.rename_vs_add_delete,
                ),
                rename_from_differs: category_breakdown(
                    counts.rename_from_differs,
                    total_commits,
                    samples.rename_from_differs,
                ),
                kind_differs: category_breakdown(
                    counts.kind_differs,
                    total_commits,
                    samples.kind_differs,
                ),
                count_differs: category_breakdown(
                    counts.count_differs,
                    total_commits,
                    samples.count_differs,
                ),
                other: category_breakdown(counts.other, total_commits, samples.other),
            },
        },
        commits: commits.into_iter().map(commit_report).collect(),
    }
}

#[derive(Default)]
struct CommitCategories {
    rename_vs_add_delete: bool,
    rename_from_differs: bool,
    kind_differs: bool,
    count_differs: bool,
}

fn classify_commit(commit: &CommitParity) -> CommitCategories {
    CommitCategories {
        rename_vs_add_delete: has_rename_vs_add_delete(&commit.differences),
        rename_from_differs: commit
            .differences
            .iter()
            .any(|diff| matches!(diff, Difference::RenameFromDiffers { .. })),
        kind_differs: commit
            .differences
            .iter()
            .any(|diff| matches!(diff, Difference::KindDiffers { .. })),
        count_differs: commit.cli_count != commit.gix_count,
    }
}

fn has_rename_vs_add_delete(differences: &[Difference]) -> bool {
    let only_cli = differences
        .iter()
        .filter_map(|diff| match diff {
            Difference::OnlyInCli { change } => Some(change),
            _ => None,
        })
        .collect::<Vec<_>>();
    let only_gix = differences
        .iter()
        .filter_map(|diff| match diff {
            Difference::OnlyInGix { change } => Some(change),
            _ => None,
        })
        .collect::<Vec<_>>();

    only_cli.iter().any(|change| {
        rename_change(change)
            .is_some_and(|(from, to, parent)| has_add_delete_pair(&only_gix, from, to, parent))
    }) || only_gix.iter().any(|change| {
        rename_change(change)
            .is_some_and(|(from, to, parent)| has_add_delete_pair(&only_cli, from, to, parent))
    })
}

fn rename_change(change: &FileChange) -> Option<(&GitPath, &GitPath, &Option<String>)> {
    match &change.kind {
        FileChangeKind::Renamed { from } => Some((from, &change.path, &change.parent_sha)),
        _ => None,
    }
}

fn has_add_delete_pair(
    changes: &[&FileChange],
    from: &GitPath,
    to: &GitPath,
    parent_sha: &Option<String>,
) -> bool {
    let has_added_to = changes.iter().any(|change| {
        change.parent_sha == *parent_sha
            && change.path == *to
            && matches!(change.kind, FileChangeKind::Added)
    });
    let has_deleted_from = changes.iter().any(|change| {
        change.parent_sha == *parent_sha
            && change.path == *from
            && matches!(change.kind, FileChangeKind::Deleted)
    });
    has_added_to && has_deleted_from
}

fn push_sample(samples: &mut Vec<CategorySample>, sample: CategorySample) {
    if samples.len() < SAMPLE_COMMITS_PER_CATEGORY {
        samples.push(sample);
    }
}

fn sample_for_commit(commit: &CommitParity) -> CategorySample {
    CategorySample {
        sha: commit.sha.clone(),
        subject: commit.subject.clone(),
        differences: commit
            .differences
            .iter()
            .take(SAMPLE_DIFFS_PER_COMMIT)
            .map(difference_report)
            .collect(),
    }
}

fn category_breakdown(
    commits: usize,
    total_commits: usize,
    samples: Vec<CategorySample>,
) -> CategoryBreakdown {
    CategoryBreakdown {
        commits,
        percent_of_total: percent(commits, total_commits),
        samples,
    }
}

fn commit_report(commit: CommitParity) -> CommitParityReport {
    CommitParityReport {
        sha: commit.sha,
        subject: commit.subject,
        parent_count: commit.parent_count,
        cli_count: commit.cli_count,
        gix_count: commit.gix_count,
        identical: commit.identical,
        differences: commit.differences.iter().map(difference_report).collect(),
        error: commit.error,
    }
}

fn difference_report(diff: &Difference) -> DifferenceReport {
    match diff {
        Difference::OnlyInCli { change } => DifferenceReport::OnlyInCli {
            change: change_report(change),
        },
        Difference::OnlyInGix { change } => DifferenceReport::OnlyInGix {
            change: change_report(change),
        },
        Difference::KindDiffers {
            parent_sha,
            path,
            cli_kind,
            gix_kind,
        } => DifferenceReport::KindDiffers {
            parent_sha: parent_sha.clone(),
            path: path_report(path),
            cli_kind: change_kind_report(cli_kind),
            gix_kind: change_kind_report(gix_kind),
        },
        Difference::RenameFromDiffers {
            parent_sha,
            to,
            cli_from,
            gix_from,
        } => DifferenceReport::RenameFromDiffers {
            parent_sha: parent_sha.clone(),
            to: path_report(to),
            cli_from: path_report(cli_from),
            gix_from: path_report(gix_from),
        },
    }
}

fn change_report(change: &FileChange) -> ChangeReport {
    ChangeReport {
        path: path_report(&change.path),
        kind: change_kind_report(&change.kind),
        parent_sha: change.parent_sha.clone(),
    }
}

fn change_kind_report(kind: &FileChangeKind) -> ChangeKindReport {
    match kind {
        FileChangeKind::Added => ChangeKindReport::Added,
        FileChangeKind::Modified => ChangeKindReport::Modified,
        FileChangeKind::Deleted => ChangeKindReport::Deleted,
        FileChangeKind::Renamed { from } => ChangeKindReport::Renamed {
            from: path_report(from),
        },
        FileChangeKind::Gitlink { old_oid, new_oid } => ChangeKindReport::Gitlink {
            old_oid: old_oid.clone(),
            new_oid: new_oid.clone(),
        },
    }
}

fn path_report(path: &GitPath) -> PathReport {
    PathReport {
        display: path.to_string_lossy().into_owned(),
        hex: hex_bytes(path.as_bytes()),
    }
}

fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("# gix Parity Audit Report\n\n");
    out.push_str(&format!("Repo: {}\n", report.repo));
    out.push_str(&format!("HEAD: {}\n", report.head));
    out.push_str(&format!(
        "Total commits scanned: {}\n",
        report.total_commits
    ));
    out.push_str(&format!(
        "Identical: {} ({:.2}%)\n",
        report.identical_commits, report.aggregates.identical_percent
    ));
    out.push_str(&format!(
        "Divergent: {} ({:.2}%)\n",
        report.divergent_commits, report.aggregates.divergent_percent
    ));
    if report.errored_commits > 0 {
        out.push_str(&format!(
            "Errored/partial comparisons: {}\n",
            report.errored_commits
        ));
    }

    out.push_str("\n## Divergence breakdown\n");
    push_breakdown_line(
        &mut out,
        "Rename vs add+delete",
        &report.aggregates.breakdown.rename_vs_add_delete,
    );
    push_breakdown_line(
        &mut out,
        "Rename-from path differs",
        &report.aggregates.breakdown.rename_from_differs,
    );
    push_breakdown_line(
        &mut out,
        "Kind differs (type/copy/gitlink)",
        &report.aggregates.breakdown.kind_differs,
    );
    push_breakdown_line(
        &mut out,
        "Count differs",
        &report.aggregates.breakdown.count_differs,
    );
    push_breakdown_line(&mut out, "Other", &report.aggregates.breakdown.other);

    out.push_str("\n## Sample divergent commits\n");
    push_sample_section(
        &mut out,
        "Rename vs add+delete",
        &report.aggregates.breakdown.rename_vs_add_delete,
    );
    push_sample_section(
        &mut out,
        "Rename-from path differs",
        &report.aggregates.breakdown.rename_from_differs,
    );
    push_sample_section(
        &mut out,
        "Kind differs (type/copy/gitlink)",
        &report.aggregates.breakdown.kind_differs,
    );
    push_sample_section(
        &mut out,
        "Count differs",
        &report.aggregates.breakdown.count_differs,
    );
    push_sample_section(&mut out, "Other", &report.aggregates.breakdown.other);

    let errored = report
        .commits
        .iter()
        .filter(|commit| commit.error.is_some())
        .take(SAMPLE_COMMITS_PER_CATEGORY)
        .collect::<Vec<_>>();
    if !errored.is_empty() {
        out.push_str("\n### Errored/partial comparisons\n");
        for commit in errored {
            out.push_str(&format!(
                "- {} {}: {}\n",
                short_sha(&commit.sha),
                commit.subject,
                commit.error.as_deref().unwrap_or_default()
            ));
        }
    }

    out
}

fn push_breakdown_line(out: &mut String, label: &str, breakdown: &CategoryBreakdown) {
    out.push_str(&format!(
        "- {}: {} ({:.2}%)\n",
        label, breakdown.commits, breakdown.percent_of_total
    ));
}

fn push_sample_section(out: &mut String, label: &str, breakdown: &CategoryBreakdown) {
    if breakdown.samples.is_empty() {
        return;
    }
    out.push_str(&format!("\n### {} ({})\n", label, breakdown.commits));
    for sample in &breakdown.samples {
        out.push_str(&format!(
            "- {} {}: {} sampled difference(s)\n",
            short_sha(&sample.sha),
            sample.subject,
            sample.differences.len()
        ));
        for diff in &sample.differences {
            out.push_str(&format!("  - {}\n", format_difference(diff)));
        }
    }
}

fn format_difference(diff: &DifferenceReport) -> String {
    match diff {
        DifferenceReport::OnlyInCli { change } => {
            format!("only in CLI: {}", format_change(change))
        }
        DifferenceReport::OnlyInGix { change } => {
            format!("only in gix: {}", format_change(change))
        }
        DifferenceReport::KindDiffers {
            parent_sha,
            path,
            cli_kind,
            gix_kind,
        } => format!(
            "kind differs{} at {}: CLI {}, gix {}",
            parent_suffix(parent_sha),
            path.display,
            format_kind(cli_kind),
            format_kind(gix_kind)
        ),
        DifferenceReport::RenameFromDiffers {
            parent_sha,
            to,
            cli_from,
            gix_from,
        } => format!(
            "rename source differs{} to {}: CLI from {}, gix from {}",
            parent_suffix(parent_sha),
            to.display,
            cli_from.display,
            gix_from.display
        ),
    }
}

fn format_change(change: &ChangeReport) -> String {
    format!(
        "{} {}{}",
        format_kind(&change.kind),
        change.path.display,
        parent_suffix(&change.parent_sha)
    )
}

fn format_kind(kind: &ChangeKindReport) -> String {
    match kind {
        ChangeKindReport::Added => "added".to_string(),
        ChangeKindReport::Modified => "modified".to_string(),
        ChangeKindReport::Deleted => "deleted".to_string(),
        ChangeKindReport::Renamed { from } => format!("renamed from {}", from.display),
        ChangeKindReport::Gitlink { old_oid, new_oid } => {
            format!(
                "gitlink {} -> {}",
                old_oid.as_deref().unwrap_or("<none>"),
                new_oid.as_deref().unwrap_or("<none>")
            )
        }
    }
}

fn parent_suffix(parent_sha: &Option<String>) -> String {
    parent_sha
        .as_ref()
        .map(|sha| format!(" [parent {}]", short_sha(sha)))
        .unwrap_or_default()
}

fn print_summary(report: &Report, json_path: &Path, md_path: &Path) {
    println!(
        "summary: total={} identical={} ({:.2}%) divergent={} ({:.2}%) errors={}",
        report.total_commits,
        report.identical_commits,
        report.aggregates.identical_percent,
        report.divergent_commits,
        report.aggregates.divergent_percent,
        report.errored_commits
    );
    println!(
        "breakdown: rename_vs_add_delete={} rename_from_differs={} kind_differs={} count_differs={} other={}",
        report.aggregates.breakdown.rename_vs_add_delete.commits,
        report.aggregates.breakdown.rename_from_differs.commits,
        report.aggregates.breakdown.kind_differs.commits,
        report.aggregates.breakdown.count_differs.commits,
        report.aggregates.breakdown.other.commits
    );
    println!("wrote {}", json_path.display());
    println!("wrote {}", md_path.display());
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}
