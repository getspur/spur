//! Git history walker for code graph artifacts.
//!
//! `try_rename_match` is deliberately crate-private; rename-corpus coverage
//! lives in this crate's unit tests so the heuristic is not exported as public
//! API.
//!
//! ```compile_fail
//! use spur_graph::git_walk::try_rename_match;
//!
//! let _ = try_rename_match;
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use anyhow::{anyhow, bail, Context as _, Result};
use gix::object::tree::diff::{Action, Change};
use gix::objs::tree::EntryMode;
use indicatif::ProgressBar;

use crate::content_hash::compute_graph_content_hash;
use crate::extract::languages::Language;
use crate::extract::tree_sitter::{BytesExtractor, ExtractError, ExtractedSymbol};
use crate::schema::{
    ChangeKind, CommitArtifact, CommitIndexArtifact, EdgeEndpoint, GitPath, GraphIndexArtifact,
    GraphIndexHeader, RelationKind, RenamePrev, SnapshotKey, SymbolSnapshotArtifact,
    TemporalEdgeArtifact, WalkStrategy, GRAPH_INDEX_VERSION_TEMPORAL,
};
use crate::store::parquet::{
    load_temporal_artifact_metadata_parquet, stream_temporal_artifact_parquet_into_sink,
};
use crate::store::{
    commit_index, current_manifest_version, load_temporal_artifact_parquet,
    resolve_artifact_location, TemporalShardSink,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWalkConfig {
    pub target_refs: Vec<String>,
    pub walk_strategy: WalkStrategy,
    pub allow_replace_refs: bool,
    pub use_gix_diff: bool,
}

impl Default for GitWalkConfig {
    fn default() -> Self {
        Self {
            target_refs: vec!["main".to_owned()],
            walk_strategy: WalkStrategy::Reachable,
            allow_replace_refs: false,
            use_gix_diff: true,
        }
    }
}

pub fn snapshot_refs(worktree: &Path, refs: &[&str]) -> Result<BTreeMap<String, String>> {
    ensure_not_shallow(worktree)?;
    let mut snapshot = BTreeMap::new();

    for target_ref in refs {
        let ref_name;
        let rev = if *target_ref == "HEAD" {
            "HEAD^{commit}"
        } else {
            ref_name = format!("refs/heads/{target_ref}");
            &ref_name
        };
        let stdout = run_git(worktree, &["rev-parse", "--verify", rev]).with_context(|| {
            format!("target ref `{target_ref}` does not exist; refusing to fall back")
        })?;
        snapshot.insert((*target_ref).to_owned(), stdout.trim().to_owned());
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
pub enum IncrementalPlan {
    ColdWalk {
        from_root: bool,
    },
    FastForward {
        from: String,
        to: String,
    },
    ForcePushRecover {
        merge_base: Option<String>,
        to: String,
    },
}

pub fn plan_incremental_walk(
    worktree: &Path,
    stored_tip: Option<&str>,
    new_tip: &str,
) -> Result<IncrementalPlan> {
    let Some(stored) = stored_tip else {
        return Ok(IncrementalPlan::ColdWalk { from_root: true });
    };

    let ancestor = Command::new("git")
        .current_dir(worktree)
        .args(["merge-base", "--is-ancestor", stored, new_tip])
        .status()
        .with_context(|| {
            format!(
                "spawn git merge-base --is-ancestor in `{}`",
                worktree.display()
            )
        })?;

    if ancestor.success() {
        return Ok(IncrementalPlan::FastForward {
            from: stored.to_owned(),
            to: new_tip.to_owned(),
        });
    }

    tracing::warn!(
        stored_tip = stored,
        new_tip,
        status = ?ancestor.code(),
        "spur-graph: stored commit is not an ancestor of new tip; force-push recovery will re-walk the diverged range"
    );
    let merge_base = run_git(worktree, &["merge-base", stored, new_tip])
        .map(|stdout| stdout.trim().to_owned())
        .inspect_err(|error| {
            tracing::warn!(
                stored_tip = stored,
                new_tip,
                error = %error,
                "spur-graph: force-push recovery could not find a merge base; falling back to cold recovery for this ref"
            );
        })
        .ok()
        .filter(|sha| !sha.is_empty());

    Ok(IncrementalPlan::ForcePushRecover {
        merge_base,
        to: new_tip.to_owned(),
    })
}

pub fn run_full_walk_into(
    worktree: &Path,
    config: &GitWalkConfig,
    progress: Option<ProgressBar>,
    mut sink: Option<&mut TemporalShardSink>,
) -> Result<(GraphIndexArtifact, CommitIndexArtifact)> {
    ensure_not_shallow(worktree)?;
    check_replace_refs(worktree, config.allow_replace_refs)?;
    let gix_repo = if config.use_gix_diff {
        Some(open_gix_repo_without_replace_refs(worktree)?)
    } else {
        None
    };

    let first_ref = config
        .target_refs
        .first()
        .context("GitWalkConfig.target_refs must contain at least one ref")?;
    let target_refs: Vec<_> = config.target_refs.iter().map(String::as_str).collect();
    let refs = snapshot_refs(worktree, &target_refs)?;
    let tip = refs
        .get(first_ref)
        .with_context(|| format!("target ref `{first_ref}` was not present after ref snapshot"))?;
    let base = load_incremental_base(
        worktree,
        first_ref,
        config.walk_strategy,
        sink.as_deref_mut(),
    )?;
    let plan = plan_incremental_walk(
        worktree,
        base.as_ref().map(|base| base.stored_tip.as_str()),
        tip,
    )?;
    let use_incremental_base =
        matches!(plan, IncrementalPlan::FastForward { .. }) && base.is_some();
    let commit_shas = if use_incremental_base {
        planned_commits(worktree, &plan, tip, config.walk_strategy)?
    } else {
        walk_commits(worktree, tip, config.walk_strategy)?
    };

    let (mut graph, mut commits) = if use_incremental_base {
        let base = base.expect("incremental base checked above");
        let mut commits = base.commits;
        commits.refs = refs.clone();
        commits.indexed_at = chrono::Utc::now().to_rfc3339();
        (base.graph, commits)
    } else {
        (
            empty_graph_artifact(),
            CommitIndexArtifact {
                schema_version: current_temporal_schema_version()?,
                commits: Vec::with_capacity(commit_shas.len()),
                refs: refs.clone(),
                indexed_at: chrono::Utc::now().to_rfc3339(),
                walk_strategy: config.walk_strategy,
            },
        )
    };
    let mut ctx = SymbolDiffCtx::new();

    for sha in commit_shas {
        let commit = match &gix_repo {
            Some(repo) => read_commit_gix(repo, &sha)?,
            None => read_commit(worktree, &sha)?,
        };
        graph.commits.push(commit.clone());
        commits.commits.push(commit.clone());

        let file_changes = match &gix_repo {
            Some(repo) => file_changes_for_commit_gix(repo, &sha)?,
            None => file_changes_for_commit(worktree, &sha)?,
        };
        for file_change in &file_changes {
            graph
                .temporal_edges
                .push(file_change_to_temporal_edge(&sha, file_change));
        }

        for symbol_change in symbol_changes_for_commit(worktree, &sha, &file_changes, &mut ctx)? {
            let snapshot_key = symbol_change.snapshot.key.clone();
            let parent_sha = symbol_change.parent_sha.clone();
            let change_kind = symbol_change.change_kind.clone();

            graph.symbol_snapshots.push(symbol_change.snapshot.clone());
            graph.temporal_edges.push(TemporalEdgeArtifact {
                source: EdgeEndpoint::Commit { sha: sha.clone() },
                target: EdgeEndpoint::Snapshot {
                    key: snapshot_key.clone(),
                },
                relation: RelationKind::Touches,
                parent: parent_sha.clone(),
                change_kind: Some(change_kind.clone()),
            });

            if let ChangeKind::RenamedFrom(RenamePrev::Symbol(previous_key)) = change_kind {
                graph.temporal_edges.push(TemporalEdgeArtifact {
                    source: EdgeEndpoint::Snapshot {
                        key: previous_key.clone(),
                    },
                    target: EdgeEndpoint::Snapshot { key: snapshot_key },
                    relation: RelationKind::Touches,
                    parent: parent_sha,
                    change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(previous_key))),
                });
            }
        }
        if let Some(progress) = progress.as_ref() {
            progress.inc(1);
        }
        if let Some(sink) = sink.as_deref_mut() {
            sink.append_commit(
                &commit,
                &mut graph.temporal_edges,
                &mut graph.symbol_snapshots,
            )?;
        }
    }

    graph.diagnostics.extend(ctx.diagnostics().iter().cloned());
    Ok((graph, commits))
}

struct IncrementalBase {
    graph: GraphIndexArtifact,
    commits: CommitIndexArtifact,
    stored_tip: String,
}

fn load_incremental_base(
    worktree: &Path,
    first_ref: &str,
    walk_strategy: WalkStrategy,
    sink: Option<&mut TemporalShardSink>,
) -> Result<Option<IncrementalBase>> {
    let Some(pointer) = commit_index::load_pointer(worktree)? else {
        return Ok(None);
    };
    let commits = commit_index::load_artifact(worktree, &pointer)?;
    if commits.walk_strategy != walk_strategy {
        tracing::info!(
            stored = ?commits.walk_strategy,
            requested = ?walk_strategy,
            "spur-graph: commit-index walk strategy changed; selecting cold temporal walk"
        );
        return Ok(None);
    }
    let Some(stored_tip) = pointer
        .refs
        .get(first_ref)
        .or_else(|| commits.refs.get(first_ref))
        .or_else(|| commits.commits.last().map(|commit| &commit.sha))
        .cloned()
    else {
        return Ok(None);
    };

    let graph_location = match resolve_artifact_location(worktree, None) {
        Ok(location) => location,
        Err(error) => {
            tracing::info!(
                error = %error,
                "spur-graph: commit-index pointer exists but prior graph artifact is missing; selecting cold temporal walk"
            );
            return Ok(None);
        }
    };

    tracing::debug!(
        path = %graph_location.path.display(),
        "spur-graph: loading temporal-only Parquet graph artifact for incremental walk"
    );
    let mut graph = if sink.is_some() {
        load_temporal_artifact_metadata_parquet(&graph_location.path)?
    } else {
        load_temporal_artifact_parquet(&graph_location.path)?
    };
    graph.header.content_hash_blake3 = None;
    // The temporal store reuses persisted snapshot shards verbatim; only the
    // structural artifact is rebuilt when the extractor/queries change. If the
    // manifest version (which hashes the tree-sitter query bytes + schema +
    // extractor versions) has moved since this artifact was written, the stored
    // snapshots were produced by a different extractor and would silently miss
    // any newly-emitted symbol kinds. Force a cold re-walk so history is
    // re-extracted with the current extractor.
    let expected_manifest_version = crate::store::build::current_manifest_version();
    if graph.manifest_version != expected_manifest_version {
        tracing::info!(
            stored = graph.manifest_version,
            expected = expected_manifest_version,
            path = %graph_location.path.display(),
            "spur-graph: extractor/query manifest version changed; selecting cold temporal walk"
        );
        return Ok(None);
    }
    if !graph.commits.iter().any(|commit| commit.sha == stored_tip) {
        tracing::info!(
            stored_tip,
            path = %graph_location.path.display(),
            "spur-graph: prior graph artifact lacks stored tip; selecting cold temporal walk"
        );
        return Ok(None);
    }

    if let Some(sink) = sink {
        stream_temporal_artifact_parquet_into_sink(&graph_location.path, sink)?;
    }

    Ok(Some(IncrementalBase {
        graph,
        commits,
        stored_tip,
    }))
}

fn planned_commits(
    worktree: &Path,
    plan: &IncrementalPlan,
    tip: &str,
    strategy: WalkStrategy,
) -> Result<Vec<String>> {
    match plan {
        IncrementalPlan::ColdWalk { .. } => walk_commits(worktree, tip, strategy),
        IncrementalPlan::FastForward { from, to } => {
            walk_commit_range(worktree, Some(from), to, strategy)
        }
        IncrementalPlan::ForcePushRecover {
            merge_base: Some(from),
            to,
        } => walk_commit_range(worktree, Some(from), to, strategy),
        IncrementalPlan::ForcePushRecover {
            merge_base: None,
            to,
        } => walk_commits(worktree, to, strategy),
    }
}

fn walk_commits(worktree: &Path, tip: &str, strategy: WalkStrategy) -> Result<Vec<String>> {
    let mut args = vec!["rev-list", "--topo-order", "--reverse"];
    if matches!(strategy, WalkStrategy::FirstParent) {
        args.push("--first-parent");
    }
    args.push(tip);

    let stdout = run_git(worktree, &args)?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn walk_commit_range(
    worktree: &Path,
    from_exclusive: Option<&str>,
    tip: &str,
    strategy: WalkStrategy,
) -> Result<Vec<String>> {
    let Some(from_exclusive) = from_exclusive else {
        return walk_commits(worktree, tip, strategy);
    };

    let range = format!("{from_exclusive}..{tip}");
    let mut args = vec!["rev-list", "--topo-order", "--reverse"];
    if matches!(strategy, WalkStrategy::FirstParent) {
        args.push("--first-parent");
    }
    args.push(&range);

    let stdout = run_git(worktree, &args)?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn read_commit(worktree: &Path, sha: &str) -> Result<CommitArtifact> {
    let stdout = run_git(
        worktree,
        &[
            "show",
            "-s",
            "--format=%H%x00%P%x00%ct%x00%an%x00%ae%x00%s",
            sha,
        ],
    )?;
    let mut fields = stdout.trim_end_matches('\n').splitn(6, '\0');
    let actual_sha = fields
        .next()
        .filter(|field| !field.is_empty())
        .with_context(|| format!("git show emitted malformed metadata for commit `{sha}`"))?;
    let parents = fields
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let author_time = fields
        .next()
        .with_context(|| format!("git show omitted author time for commit `{sha}`"))?
        .parse::<i64>()
        .with_context(|| format!("git show emitted invalid author time for commit `{sha}`"))?;
    let author_name = fields.next().unwrap_or_default().to_owned();
    let author_email = fields.next().unwrap_or_default().to_owned();
    let summary = fields.next().unwrap_or_default().to_owned();

    Ok(CommitArtifact {
        sha: actual_sha.to_owned(),
        parents,
        author_time,
        author_name,
        author_email,
        summary,
    })
}

fn open_gix_repo_without_replace_refs(worktree: &Path) -> Result<gix::Repository> {
    // Disable git's default replace-ref honoring inside gix. We rely on the
    // pre-walk `check_replace_refs` guard to refuse repos with `refs/replace`
    // entries unless `allow_replace_refs: true` is explicitly set; if a caller
    // opts in to allow them, the CLI path uses raw OIDs, and we want gix to do
    // the same so the two paths stay equivalent.
    // gix 0.77 treats `core.useReplaceRefs=false` as permission to scan the
    // replacement namespace at open time, so route that scan to an empty
    // SPUR-private prefix while keeping the git-compatible config override.
    let mut repo = gix::open_opts(
        worktree,
        gix::open::Options::default().cli_overrides([
            "core.useReplaceRefs=false",
            "gitoxide.objects.replaceRefBase=refs/spur-disabled-replace/",
        ]),
    )
    .with_context(|| format!("open gix repository `{}`", worktree.display()))?;
    repo.object_cache_size_if_unset(128 * 1024 * 1024);
    Ok(repo)
}

fn read_commit_gix(repo: &gix::Repository, sha: &str) -> Result<CommitArtifact> {
    let oid = gix::ObjectId::from_hex(sha.as_bytes())?;
    let commit = repo.find_commit(oid)?;
    let parents = commit
        .parent_ids()
        .map(|parent| parent.to_hex().to_string())
        .collect();
    let author = commit.author()?.trim();
    let author_time = author.seconds();
    let author_name = String::from_utf8_lossy(author.name.as_ref()).into_owned();
    let author_email = String::from_utf8_lossy(author.email.as_ref()).into_owned();
    let summary = commit_subject(&commit);

    Ok(CommitArtifact {
        sha: commit.id.to_hex().to_string(),
        parents,
        author_time,
        author_name,
        author_email,
        summary,
    })
}

fn commit_subject(commit: &gix::Commit<'_>) -> String {
    let message = commit.message_raw_sloppy();
    let first_line = message
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(message, |index| &message[..index]);
    String::from_utf8_lossy(first_line).into_owned()
}

fn file_change_to_temporal_edge(commit_sha: &str, change: &FileChange) -> TemporalEdgeArtifact {
    TemporalEdgeArtifact {
        source: EdgeEndpoint::Commit {
            sha: commit_sha.to_owned(),
        },
        target: EdgeEndpoint::File {
            path: change.path.clone(),
        },
        relation: RelationKind::Touches,
        parent: change.parent_sha.clone(),
        change_kind: Some(match &change.kind {
            FileChangeKind::Added => ChangeKind::Added,
            FileChangeKind::Modified | FileChangeKind::Gitlink { .. } => ChangeKind::Modified,
            FileChangeKind::Deleted => ChangeKind::Deleted,
            FileChangeKind::Renamed { from } => {
                ChangeKind::RenamedFrom(RenamePrev::File(from.clone()))
            }
        }),
    }
}

fn empty_graph_artifact() -> GraphIndexArtifact {
    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_owned(),
            content_hash_blake3: None,
        },
        manifest_version: current_manifest_version(),
        graph_content_hash: compute_graph_content_hash(std::iter::empty::<(&str, &str)>()),
        file_manifests: Vec::new(),
        files: Vec::new(),
        file_node_ids: Vec::new(),
        symbols: Vec::new(),
        symbol_node_ids: Vec::new(),
        edges: Vec::new(),
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed {
        from: GitPath,
    },
    Gitlink {
        old_oid: Option<String>,
        new_oid: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: GitPath,
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
                "-c",
                "core.quotepath=false",
                "diff-tree",
                "-r",
                "-z",
                "--raw",
                "--find-renames",
                &parent,
                sha,
            ],
        )?;
        parse_raw_diff(&stdout, Some(parent), &mut changes)?;
    }

    Ok(changes)
}

fn file_changes_for_commit_gix(repo: &gix::Repository, sha: &str) -> Result<Vec<FileChange>> {
    let oid = gix::ObjectId::from_hex(sha.as_bytes())?;
    let commit = repo.find_commit(oid)?;
    let parent_ids = commit.parent_ids().map(gix::Id::detach).collect::<Vec<_>>();

    if parent_ids.is_empty() {
        return root_commit_changes_gix(&commit);
    }

    let commit_tree = commit.tree()?;
    let mut resource_cache = repo.diff_resource_cache_for_tree_diff()?;
    let mut changes = Vec::new();

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
                    changes.push(change);
                }
                Ok(Action::Continue)
            },
        )?;
        resource_cache.clear_resource_cache_keep_allocation();
    }

    Ok(changes)
}

fn root_commit_changes_gix(commit: &gix::Commit<'_>) -> Result<Vec<FileChange>> {
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
    let parent_sha = Some(parent_sha.to_owned());
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

pub struct SymbolDiffCtx {
    extractors: HashMap<Language, BytesExtractor>,
    diagnostics: Vec<String>,
    cat_file_batches: HashMap<PathBuf, CatFileBatch>,
    parse_cache: HashMap<(Language, String), Vec<ExtractedSymbol>>,
}

impl SymbolDiffCtx {
    pub fn new() -> Self {
        Self {
            extractors: HashMap::new(),
            diagnostics: Vec::new(),
            cat_file_batches: HashMap::new(),
            parse_cache: HashMap::new(),
        }
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    #[cfg(test)]
    pub(crate) fn parse_cache_len(&self) -> usize {
        self.parse_cache.len()
    }

    fn for_language(&mut self, language: Language) -> Result<&mut BytesExtractor> {
        match self.extractors.entry(language) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                Ok(entry.insert(BytesExtractor::for_language(language)?))
            }
        }
    }

    fn record_diagnostics(&mut self, diagnostics: Vec<String>) {
        self.diagnostics.extend(diagnostics);
    }

    fn cat_file_batch(&mut self, worktree: &Path) -> Result<&mut CatFileBatch> {
        match self.cat_file_batches.entry(worktree.to_path_buf()) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                Ok(entry.insert(CatFileBatch::new(worktree)?))
            }
        }
    }
}

impl Default for SymbolDiffCtx {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolChange {
    pub snapshot: SymbolSnapshotArtifact,
    pub change_kind: ChangeKind,
    pub parent_sha: Option<String>,
}

pub fn symbol_changes_for_commit(
    worktree: &Path,
    sha: &str,
    file_changes: &[FileChange],
    ctx: &mut SymbolDiffCtx,
) -> Result<Vec<SymbolChange>> {
    let mut out = Vec::new();
    let mut by_snapshot_key = HashMap::new();

    for file_change in file_changes {
        if matches!(file_change.kind, FileChangeKind::Gitlink { .. }) {
            let diagnostic = format!(
                "gitlink: file={} commit={} skipped submodule recursion; file-level touch retained",
                file_change.path.display(),
                sha
            );
            tracing::warn!(diagnostic = %diagnostic, "spur-graph: gitlink encountered during symbol walk");
            ctx.record_diagnostics(vec![diagnostic]);
            continue;
        }

        let current_path = file_change.path.to_path_buf();
        let Some(language) = Language::from_path(&current_path) else {
            continue;
        };

        let blobs = {
            let cat_file_batch = ctx.cat_file_batch(worktree)?;
            blobs_for_change(worktree, cat_file_batch, sha, file_change)?
        };
        if blobs
            .left
            .as_ref()
            .is_some_and(|(_, bytes)| is_binary(bytes))
            || blobs
                .right
                .as_ref()
                .is_some_and(|(_, bytes)| is_binary(bytes))
        {
            let diagnostic = format!(
                "binary_blob: file={} commit={} skipped symbol diff; file-level touch retained",
                file_change.path.display(),
                sha
            );
            tracing::warn!(diagnostic = %diagnostic, "spur-graph: binary blob encountered during symbol walk");
            ctx.record_diagnostics(vec![diagnostic]);
            continue;
        }

        let deleted_path = blobs.left_path.as_ref().unwrap_or(&file_change.path);
        let left_path_buf = blobs.left_path.as_ref().map(GitPath::to_path_buf);
        let left_result = match (left_path_buf.as_deref(), blobs.left.as_ref()) {
            (Some(path), Some((oid, bytes))) => cached_extract(ctx, language, oid, path, bytes)?,
            _ => Ok(Vec::new()),
        };
        let right_result = match blobs.right.as_ref() {
            Some((oid, bytes)) => cached_extract(ctx, language, oid, &current_path, bytes)?,
            None => Ok(Vec::new()),
        };
        let mut parse_failed = false;
        let left_symbols = match left_result {
            Ok(symbols) => symbols,
            Err(error) => {
                ctx.record_diagnostics(vec![parse_failed_diagnostic(
                    sha,
                    deleted_path,
                    "left",
                    &error,
                )]);
                parse_failed = true;
                Vec::new()
            }
        };
        let right_symbols = match right_result {
            Ok(symbols) => symbols,
            Err(error) => {
                ctx.record_diagnostics(vec![parse_failed_diagnostic(
                    sha,
                    &file_change.path,
                    "right",
                    &error,
                )]);
                parse_failed = true;
                Vec::new()
            }
        };
        if parse_failed {
            continue;
        }

        let mut direct_changes = Vec::new();
        let mut deleted_candidates = Vec::new();
        let mut added_candidates = Vec::new();

        if matches!(file_change.kind, FileChangeKind::Renamed { .. }) {
            deleted_candidates.extend(left_symbols.iter().map(|left| SymbolChange {
                snapshot: snapshot_from(sha, deleted_path, left),
                change_kind: ChangeKind::Deleted,
                parent_sha: file_change.parent_sha.clone(),
            }));
            added_candidates.extend(right_symbols.iter().map(|right| SymbolChange {
                snapshot: snapshot_from(sha, &file_change.path, right),
                change_kind: ChangeKind::Added,
                parent_sha: file_change.parent_sha.clone(),
            }));
        } else {
            let mut left_by_identity: HashMap<(String, String, Option<String>), &ExtractedSymbol> =
                left_symbols
                    .iter()
                    .map(|symbol| {
                        (
                            (
                                symbol.entity_name.clone(),
                                symbol.symbol_kind.clone(),
                                symbol.enclosing_scope.clone(),
                            ),
                            symbol,
                        )
                    })
                    .collect();

            for right in &right_symbols {
                let identity = (
                    right.entity_name.clone(),
                    right.symbol_kind.clone(),
                    right.enclosing_scope.clone(),
                );
                let right_snapshot = snapshot_from(sha, &file_change.path, right);
                match left_by_identity.remove(&identity) {
                    Some(left) => {
                        let left_snapshot = snapshot_from(sha, deleted_path, left);
                        if left.anchor_hash == right.anchor_hash
                            && left_snapshot.key.stable_symbol_id
                                == right_snapshot.key.stable_symbol_id
                        {
                            continue;
                        }
                        direct_changes.push(SymbolChange {
                            snapshot: right_snapshot,
                            change_kind: ChangeKind::Modified,
                            parent_sha: file_change.parent_sha.clone(),
                        });
                    }
                    None => added_candidates.push(SymbolChange {
                        snapshot: right_snapshot,
                        change_kind: ChangeKind::Added,
                        parent_sha: file_change.parent_sha.clone(),
                    }),
                }
            }

            for (_, left) in left_by_identity {
                deleted_candidates.push(SymbolChange {
                    snapshot: snapshot_from(sha, deleted_path, left),
                    change_kind: ChangeKind::Deleted,
                    parent_sha: file_change.parent_sha.clone(),
                });
            }
        }

        let (rename_changes, diagnostics) =
            detect_renames(deleted_candidates, added_candidates, file_change, language);
        ctx.record_diagnostics(diagnostics);

        for change in direct_changes.into_iter().chain(rename_changes) {
            push_symbol_change(&mut out, &mut by_snapshot_key, change);
        }
    }

    Ok(out)
}

fn push_symbol_change(
    out: &mut Vec<SymbolChange>,
    by_snapshot_key: &mut HashMap<(Option<String>, SnapshotKey), usize>,
    change: SymbolChange,
) {
    let key = (change.parent_sha.clone(), change.snapshot.key.clone());
    if let Some(existing) = by_snapshot_key.get(&key).copied() {
        let merged = merge_change_kind(&out[existing].change_kind, change.change_kind);
        out[existing].change_kind = merged;
        return;
    }

    by_snapshot_key.insert(key, out.len());
    out.push(change);
}

fn merge_change_kind(existing: &ChangeKind, incoming: ChangeKind) -> ChangeKind {
    if existing == &incoming {
        return incoming;
    }
    ChangeKind::Modified
}

#[derive(Debug, Clone)]
struct RenameMatch {
    from: SymbolChange,
    to: SymbolChange,
    score: f64,
}

#[cfg(test)]
pub(crate) fn try_rename_match(
    deleted_candidates: Vec<SymbolChange>,
    added_candidates: Vec<SymbolChange>,
    file_change: &FileChange,
    language: Language,
) -> (Vec<SymbolChange>, Vec<String>) {
    detect_renames(deleted_candidates, added_candidates, file_change, language)
}

fn detect_renames(
    deleted_candidates: Vec<SymbolChange>,
    added_candidates: Vec<SymbolChange>,
    file_change: &FileChange,
    language: Language,
) -> (Vec<SymbolChange>, Vec<String>) {
    if deleted_candidates.is_empty() || added_candidates.is_empty() {
        let mut changes = deleted_candidates;
        changes.extend(added_candidates);
        return (changes, Vec::new());
    }

    let mut diagnostics = Vec::new();
    let mut matches = Vec::new();
    let mut tier2_deleted = deleted_candidates;
    let mut tier2_added = added_candidates;

    if matches!(file_change.kind, FileChangeKind::Renamed { .. }) {
        let (tier1_matches, remaining_deleted, remaining_added) =
            tier1_file_rename_matches(tier2_deleted, tier2_added);
        matches.extend(tier1_matches);
        tier2_deleted = remaining_deleted;
        tier2_added = remaining_added;
    }

    let tier2_matches = tier2_jaccard_matches(
        &tier2_deleted,
        &tier2_added,
        file_change,
        language,
        &mut diagnostics,
    );
    matches.extend(tier2_matches);

    let matched_from: HashSet<_> = matches
        .iter()
        .map(|rename_match| rename_match.from.snapshot.key.clone())
        .collect();
    let matched_to: HashSet<_> = matches
        .iter()
        .map(|rename_match| rename_match.to.snapshot.key.clone())
        .collect();

    let mut changes = Vec::new();
    for rename_match in matches {
        let mut to = rename_match.to;
        to.change_kind =
            ChangeKind::RenamedFrom(RenamePrev::Symbol(rename_match.from.snapshot.key.clone()));
        changes.push(to);
    }

    changes.extend(
        tier2_deleted
            .into_iter()
            .filter(|change| !matched_from.contains(&change.snapshot.key)),
    );
    changes.extend(
        tier2_added
            .into_iter()
            .filter(|change| !matched_to.contains(&change.snapshot.key)),
    );

    (changes, diagnostics)
}

fn tier1_file_rename_matches(
    deleted_candidates: Vec<SymbolChange>,
    added_candidates: Vec<SymbolChange>,
) -> (Vec<RenameMatch>, Vec<SymbolChange>, Vec<SymbolChange>) {
    let mut matches = Vec::new();
    let mut used_deleted = HashSet::new();
    let mut used_added = HashSet::new();

    for (added_index, added) in added_candidates.iter().enumerate() {
        if let Some((deleted_index, deleted)) =
            deleted_candidates
                .iter()
                .enumerate()
                .find(|(deleted_index, deleted)| {
                    !used_deleted.contains(deleted_index)
                        && deleted.snapshot.entity_name == added.snapshot.entity_name
                        && deleted.snapshot.symbol_kind == added.snapshot.symbol_kind
                        && deleted.snapshot.enclosing_scope == added.snapshot.enclosing_scope
                })
        {
            used_deleted.insert(deleted_index);
            used_added.insert(added_index);
            matches.push(RenameMatch {
                from: deleted.clone(),
                to: added.clone(),
                score: 1.0,
            });
        }
    }

    let remaining_deleted = deleted_candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, change)| (!used_deleted.contains(&index)).then_some(change))
        .collect();
    let remaining_added = added_candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, change)| (!used_added.contains(&index)).then_some(change))
        .collect();

    (matches, remaining_deleted, remaining_added)
}

fn tier2_jaccard_matches(
    deleted_candidates: &[SymbolChange],
    added_candidates: &[SymbolChange],
    file_change: &FileChange,
    language: Language,
    diagnostics: &mut Vec<String>,
) -> Vec<RenameMatch> {
    let Some(threshold) = jaccard_threshold_for(language) else {
        return Vec::new();
    };

    let deleted_token_sets = token_sets_for_changes(deleted_candidates);
    let added_token_sets = token_sets_for_changes(added_candidates);
    let mut matches = Vec::new();
    for added in &added_token_sets {
        let (Some((best_deleted, best_score)), second) =
            best_two_jaccard_matches(added, &deleted_token_sets)
        else {
            continue;
        };
        if best_score < threshold {
            record_ambiguous_rename_pair(diagnostics, file_change, added.change, best_deleted);
            continue;
        }
        if let Some((second_deleted, second_score)) = second {
            if second_score >= threshold {
                record_ambiguous_rename_pair(diagnostics, file_change, added.change, best_deleted);
                record_ambiguous_rename_pair(
                    diagnostics,
                    file_change,
                    added.change,
                    second_deleted,
                );
                diagnostics.push(format!(
                    "merge_collision: file={} candidate={}",
                    file_change.path.display(),
                    added.change.snapshot.entity_name
                ));
                continue;
            }
            if best_score - second_score < RENAME_AMBIGUITY_EPSILON {
                record_ambiguous_rename_pair(diagnostics, file_change, added.change, best_deleted);
                record_ambiguous_rename_pair(
                    diagnostics,
                    file_change,
                    added.change,
                    second_deleted,
                );
                continue;
            }
        }

        matches.push(RenameMatch {
            from: best_deleted.clone(),
            to: added.change.clone(),
            score: best_score,
        });
    }

    reject_ambiguous_splits(matches, file_change, diagnostics)
}

struct ChangeTokenSet<'a> {
    change: &'a SymbolChange,
    tokens: HashSet<&'a str>,
}

fn token_sets_for_changes(changes: &[SymbolChange]) -> Vec<ChangeTokenSet<'_>> {
    changes
        .iter()
        .map(|change| ChangeTokenSet {
            change,
            tokens: change.snapshot.tokens.iter().map(String::as_str).collect(),
        })
        .collect()
}

type ScoredChange<'a> = (&'a SymbolChange, f64);
const RENAME_AMBIGUITY_EPSILON: f64 = 0.05;

fn best_two_jaccard_matches<'a>(
    added: &ChangeTokenSet<'_>,
    deleted_candidates: &'a [ChangeTokenSet<'a>],
) -> (Option<ScoredChange<'a>>, Option<ScoredChange<'a>>) {
    let mut best = None;
    let mut second = None;
    for deleted in deleted_candidates {
        let scored = (
            deleted.change,
            jaccard_token_sets(&added.tokens, &deleted.tokens),
        );
        if best.is_none_or(|(_, best_score)| score_outranks(scored.1, best_score)) {
            second = best;
            best = Some(scored);
        } else if second.is_none_or(|(_, second_score)| score_outranks(scored.1, second_score)) {
            second = Some(scored);
        }
    }
    (best, second)
}

fn score_outranks(candidate: f64, current: f64) -> bool {
    current.partial_cmp(&candidate) == Some(std::cmp::Ordering::Less)
}

fn reject_ambiguous_splits(
    matches: Vec<RenameMatch>,
    file_change: &FileChange,
    diagnostics: &mut Vec<String>,
) -> Vec<RenameMatch> {
    let mut by_deleted: HashMap<SnapshotKey, Vec<usize>> = HashMap::new();
    for (index, rename_match) in matches.iter().enumerate() {
        by_deleted
            .entry(rename_match.from.snapshot.key.clone())
            .or_default()
            .push(index);
    }

    let mut rejected = HashSet::new();
    for indexes in by_deleted.values() {
        if indexes.len() < 2 {
            continue;
        }
        let mut scores: Vec<_> = indexes
            .iter()
            .map(|index| (*index, matches[*index].score))
            .collect();
        scores.sort_by(|(_, left), (_, right)| {
            right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal)
        });
        if scores[0].1 - scores[1].1 < RENAME_AMBIGUITY_EPSILON {
            if let Some(winner) = split_tiebreak_winner(&matches, &scores) {
                for (index, _) in scores {
                    if index != winner {
                        rejected.insert(index);
                    }
                }
                continue;
            }

            for (index, _) in scores {
                rejected.insert(index);
                record_ambiguous_rename_pair(
                    diagnostics,
                    file_change,
                    &matches[index].to,
                    &matches[index].from,
                );
            }
        }
    }

    matches
        .into_iter()
        .enumerate()
        .filter_map(|(index, rename_match)| (!rejected.contains(&index)).then_some(rename_match))
        .collect()
}

fn split_tiebreak_winner(matches: &[RenameMatch], scores: &[(usize, f64)]) -> Option<usize> {
    let top_score = scores.first()?.1;
    let mut winner = None;
    let mut tied = false;

    for (index, score) in scores {
        if top_score - score >= RENAME_AMBIGUITY_EPSILON {
            continue;
        }

        let candidate = split_tiebreak_signal(&matches[*index]);
        match winner {
            None => {
                winner = Some((*index, candidate));
                tied = false;
            }
            Some((_, current)) => match candidate.cmp(&current) {
                std::cmp::Ordering::Greater => {
                    winner = Some((*index, candidate));
                    tied = false;
                }
                std::cmp::Ordering::Equal => tied = true,
                std::cmp::Ordering::Less => {}
            },
        }
    }

    if tied {
        None
    } else {
        winner.and_then(|(index, signal)| (signal.0 || signal.1).then_some(index))
    }
}

fn split_tiebreak_signal(rename_match: &RenameMatch) -> (bool, bool, std::cmp::Reverse<usize>) {
    (
        rename_match.from.snapshot.entity_name == rename_match.to.snapshot.entity_name,
        entity_names_share_significant_token(
            &rename_match.from.snapshot.entity_name,
            &rename_match.to.snapshot.entity_name,
        ),
        std::cmp::Reverse(line_start_distance(
            &rename_match.from.snapshot,
            &rename_match.to.snapshot,
        )),
    )
}

fn entity_names_share_significant_token(left: &str, right: &str) -> bool {
    significant_name_tokens(left).any(|left_token| {
        significant_name_tokens(right).any(|right_token| right_token == left_token)
    })
}

fn significant_name_tokens(name: &str) -> impl Iterator<Item = &str> {
    name.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 3)
        .filter(|token| !matches!(*token, "old" | "new"))
}

fn line_start_distance(from: &SymbolSnapshotArtifact, to: &SymbolSnapshotArtifact) -> usize {
    from.line_range[0].abs_diff(to.line_range[0])
}

fn record_ambiguous_rename_pair(
    diagnostics: &mut Vec<String>,
    file_change: &FileChange,
    left: &SymbolChange,
    right: &SymbolChange,
) {
    diagnostics.push(ambiguous_rename_diagnostic(file_change, left, right));
    diagnostics.push(ambiguous_rename_diagnostic(file_change, right, left));
}

fn ambiguous_rename_diagnostic(
    file_change: &FileChange,
    change: &SymbolChange,
    other: &SymbolChange,
) -> String {
    format!(
        "ambiguous_rename: file={} stable_symbol_id={} candidate={} other_stable_symbol_id={} other_candidate={}",
        file_change.path.display(),
        change.snapshot.key.stable_symbol_id,
        change.snapshot.entity_name,
        other.snapshot.key.stable_symbol_id,
        other.snapshot.entity_name
    )
}

fn jaccard_token_sets(a_tokens: &HashSet<&str>, b_tokens: &HashSet<&str>) -> f64 {
    let union = a_tokens.union(b_tokens).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = a_tokens.intersection(b_tokens).count();
    intersection as f64 / union as f64
}

fn jaccard_threshold_for(language: Language) -> Option<f64> {
    match language {
        Language::Rust | Language::TypeScript => Some(0.7),
        Language::Python => Some(0.65),
        _ => None,
    }
}

struct ChangeBlobs {
    left_path: Option<GitPath>,
    left: Option<(String, Vec<u8>)>,
    right: Option<(String, Vec<u8>)>,
}

fn blobs_for_change(
    worktree: &Path,
    cat_file_batch: &mut CatFileBatch,
    sha: &str,
    file_change: &FileChange,
) -> Result<ChangeBlobs> {
    let right = match &file_change.kind {
        FileChangeKind::Deleted => None,
        FileChangeKind::Added | FileChangeKind::Modified | FileChangeKind::Renamed { .. } => {
            Some(cat_file_blob(
                worktree,
                cat_file_batch,
                sha,
                &file_change.path.to_path_buf(),
            )?)
        }
        FileChangeKind::Gitlink { .. } => None,
    };

    let left_path = match &file_change.kind {
        FileChangeKind::Added | FileChangeKind::Gitlink { .. } => None,
        FileChangeKind::Modified | FileChangeKind::Deleted => Some(file_change.path.clone()),
        FileChangeKind::Renamed { from } => Some(from.clone()),
    };
    let left = match (file_change.parent_sha.as_deref(), left_path.as_ref()) {
        (Some(parent), Some(path)) => Some(cat_file_blob(
            worktree,
            cat_file_batch,
            parent,
            &path.to_path_buf(),
        )?),
        _ => None,
    };

    Ok(ChangeBlobs {
        left_path,
        left,
        right,
    })
}

struct CatFileBatch {
    worktree: PathBuf,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_drain: Option<JoinHandle<()>>,
}

impl CatFileBatch {
    fn new(worktree: &Path) -> Result<Self> {
        let mut child = Command::new("git")
            .current_dir(worktree)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn git cat-file --batch in `{}`", worktree.display()))?;
        let stdin = child
            .stdin
            .take()
            .context("git cat-file --batch missing stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("git cat-file --batch missing stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("git cat-file --batch missing stderr")?;
        let stderr_drain = std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut sink = io::sink();
            let _ = io::copy(&mut stderr, &mut sink);
        });

        Ok(Self {
            worktree: worktree.to_path_buf(),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_drain: Some(stderr_drain),
        })
    }

    fn read(&mut self, sha: &str, path: &Path) -> Result<Option<(String, Vec<u8>)>> {
        let spec = blob_spec(sha, path);
        let spec_display = blob_spec_display(sha, path);
        let stdin = self
            .stdin
            .as_mut()
            .context("git cat-file --batch stdin already closed")?;
        write_blob_query(stdin, &spec).with_context(|| {
            format!(
                "write git cat-file --batch query `{spec_display}` in `{}`",
                self.worktree.display()
            )
        })?;

        let mut header = Vec::new();
        let bytes_read = self
            .stdout
            .read_until(b'\n', &mut header)
            .with_context(|| {
                format!(
                    "read git cat-file --batch header for `{spec_display}` in `{}`",
                    self.worktree.display()
                )
            })?;
        if bytes_read == 0 {
            bail!(
                "git cat-file --batch closed before header for `{spec_display}` in `{}`",
                self.worktree.display()
            );
        }
        if header.ends_with(b" missing\n") {
            return Ok(None);
        }
        if header.ends_with(b"\n") {
            header.pop();
        }

        let header = std::str::from_utf8(&header).with_context(|| {
            format!("git cat-file --batch header for `{spec_display}` emitted non-UTF-8")
        })?;
        let mut parts = header.split(' ');
        let oid = parts
            .next()
            .filter(|part| !part.is_empty())
            .with_context(|| format!("git cat-file --batch header `{header}` missing oid"))?;
        let oid = oid.to_owned();
        let object_type = parts
            .next()
            .with_context(|| format!("git cat-file --batch header `{header}` missing type"))?;
        let size = parts
            .next()
            .with_context(|| format!("git cat-file --batch header `{header}` missing size"))?;
        if parts.next().is_some() {
            bail!("unexpected git cat-file --batch header `{header}` for `{spec_display}`");
        }
        if object_type != "blob" {
            bail!(
                "git cat-file --batch `{spec_display}` resolved {oid} as `{object_type}`, expected blob"
            );
        }
        let size = size.parse::<usize>().with_context(|| {
            format!("parse git cat-file --batch size `{size}` for `{spec_display}`")
        })?;
        let mut bytes = vec![0; size];
        self.stdout.read_exact(&mut bytes).with_context(|| {
            format!(
                "read git cat-file --batch body for `{spec_display}` in `{}`",
                self.worktree.display()
            )
        })?;
        let mut trailing = [0; 1];
        self.stdout.read_exact(&mut trailing).with_context(|| {
            format!(
                "read git cat-file --batch trailing newline for `{spec_display}` in `{}`",
                self.worktree.display()
            )
        })?;
        if trailing != [b'\n'] {
            bail!("git cat-file --batch body for `{spec_display}` missing trailing newline");
        }

        Ok(Some((oid, bytes)))
    }
}

impl Drop for CatFileBatch {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
        if let Some(stderr_drain) = self.stderr_drain.take() {
            let _ = stderr_drain.join();
        }
    }
}

fn cat_file_blob(
    worktree: &Path,
    cat_file_batch: &mut CatFileBatch,
    sha: &str,
    path: &Path,
) -> Result<(String, Vec<u8>)> {
    let spec_display = blob_spec_display(sha, path);
    let missing_blob_name = || {
        blob_oid_for_path(worktree, sha, path)
            .map(|oid| format!("{oid} (`{spec_display}`)"))
            .unwrap_or_else(|_| spec_display.clone())
    };

    match cat_file_batch.read(sha, path) {
        Ok(Some(blob)) => Ok(blob),
        Ok(None) if has_promisor_remote(worktree) => {
            let missing = missing_blob_name();
            let first_error = missing_batch_error(worktree, &spec_display);
            tracing::warn!(
                blob = %missing,
                error = %first_error,
                "spur-graph: missing blob during git walk; retrying once to trigger promisor fetch"
            );
            cat_file_blob_legacy(worktree, sha, path).with_context(|| {
                format!("missing blob `{missing}` not recovered by promisor remote; fail-closed for this commit")
            })?;
            match cat_file_batch.read(sha, path) {
                Ok(Some(blob)) => Ok(blob),
                Ok(None) => Err(missing_batch_error(worktree, &spec_display).context(format!(
                    "missing blob `{missing}` not recovered by promisor remote; fail-closed for this commit"
                ))),
                Err(error) => Err(error.context(format!(
                    "missing blob `{missing}` not recovered by promisor remote; fail-closed for this commit"
                ))),
            }
        }
        Ok(None) => {
            let missing = missing_blob_name();
            Err(
                missing_batch_error(worktree, &spec_display).context(format!(
                    "missing blob `{missing}`; partial clone? fail-closed"
                )),
            )
        }
        Err(error) => {
            let missing = missing_blob_name();
            Err(error.context(format!(
                "missing blob `{missing}`; partial clone? fail-closed"
            )))
        }
    }
}

fn cat_file_blob_legacy(worktree: &Path, sha: &str, path: &Path) -> Result<(String, Vec<u8>)> {
    let spec = blob_spec(sha, path);
    let spec_display = blob_spec_display(sha, path);
    let output = Command::new("git")
        .current_dir(worktree)
        .args(["cat-file", "blob"])
        .arg(&spec)
        .output()
        .with_context(|| {
            format!(
                "spawn git cat-file blob `{spec_display}` in `{}`",
                worktree.display()
            )
        })?;

    if output.status.success() {
        Ok((blob_oid_for_path(worktree, sha, path)?, output.stdout))
    } else {
        Err(anyhow!(
            "git cat-file blob `{spec_display}` failed in `{}`: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn missing_batch_error(worktree: &Path, spec_display: &str) -> anyhow::Error {
    anyhow!(
        "git cat-file --batch reported `{spec_display}` missing in `{}`",
        worktree.display()
    )
}

fn blob_oid_for_path(worktree: &Path, sha: &str, path: &Path) -> Result<String> {
    let spec = blob_spec(sha, path);
    let spec_display = blob_spec_display(sha, path);
    let output = Command::new("git")
        .current_dir(worktree)
        .arg("rev-parse")
        .arg(&spec)
        .output()
        .with_context(|| {
            format!(
                "spawn git rev-parse `{spec_display}` in `{}`",
                worktree.display()
            )
        })?;

    if !output.status.success() {
        bail!(
            "git rev-parse `{spec_display}` failed in `{}`: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let oid = String::from_utf8(output.stdout)
        .with_context(|| format!("git rev-parse `{spec_display}` emitted non-UTF-8 stdout"))?
        .trim()
        .to_owned();
    if oid.is_empty() {
        bail!("git rev-parse `{spec_display}` returned an empty oid");
    }

    Ok(oid)
}

#[cfg(unix)]
fn blob_spec(sha: &str, path: &Path) -> OsString {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let mut bytes = Vec::with_capacity(sha.len() + 1 + path.as_os_str().as_bytes().len());
    bytes.extend_from_slice(sha.as_bytes());
    bytes.push(b':');
    bytes.extend_from_slice(path.as_os_str().as_bytes());
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn blob_spec(sha: &str, path: &Path) -> OsString {
    OsString::from(format!("{sha}:{}", path.to_string_lossy()))
}

fn write_blob_query(stdin: &mut ChildStdin, spec: &OsString) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        stdin.write_all(spec.as_os_str().as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        stdin.write_all(spec.to_string_lossy().as_bytes())?;
    }
    stdin.write_all(b"\n")?;
    stdin.flush()
}

fn blob_spec_display(sha: &str, path: &Path) -> String {
    format!("{sha}:{}", path.to_string_lossy())
}

fn has_promisor_remote(worktree: &Path) -> bool {
    run_git(
        worktree,
        &["config", "--get-regexp", r"^remote\..*\.promisor$"],
    )
    .map(|stdout| !stdout.trim().is_empty())
    .unwrap_or(false)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

fn cached_extract(
    ctx: &mut SymbolDiffCtx,
    language: Language,
    oid: &str,
    path: &Path,
    bytes: &[u8],
) -> Result<std::result::Result<Vec<ExtractedSymbol>, ExtractError>> {
    let key = (language, oid.to_owned());
    if let Some(cached) = ctx.parse_cache.get(&key) {
        return Ok(Ok(cached.clone()));
    }
    let result = {
        let extractor = ctx.for_language(language)?;
        extractor.extract(path, bytes)
    };
    if let Ok(symbols) = &result {
        ctx.parse_cache.insert(key, symbols.clone());
    }
    Ok(result)
}

fn parse_failed_diagnostic(
    commit: &str,
    path: &GitPath,
    side: &str,
    error: &ExtractError,
) -> String {
    format!(
        "parse_failed: file={} sha={} side={} error={}; skipped symbol diff, file-level touch retained",
        path.display(),
        commit,
        side,
        error
    )
}

fn snapshot_from(commit: &str, path: &GitPath, symbol: &ExtractedSymbol) -> SymbolSnapshotArtifact {
    let relative_path = path.to_string_lossy();
    let fqn = match symbol.enclosing_scope.as_deref() {
        Some(scope) => format!("{scope}::{}", symbol.entity_name),
        None => symbol.entity_name.clone(),
    };
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: crate::identity::stable_symbol_id_for_discriminator(
                relative_path.as_ref(),
                &fqn,
                &symbol.symbol_kind,
                symbol.byte_range[0] as u64,
            ),
            commit: commit.to_owned(),
        },
        file_path: path.clone(),
        entity_name: symbol.entity_name.clone(),
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
        byte_range: symbol.byte_range,
        line_range: symbol.line_range,
        anchor_hash: symbol.anchor_hash.clone(),
        tokens: symbol.tokens.clone(),
    }
}

fn current_temporal_schema_version() -> Result<u32> {
    GRAPH_INDEX_VERSION_TEMPORAL.parse().with_context(|| {
        format!("parse temporal graph index version `{GRAPH_INDEX_VERSION_TEMPORAL}`")
    })
}

fn commit_parents(worktree: &Path, sha: &str) -> Result<Vec<String>> {
    let stdout = run_git(worktree, &["rev-list", "--parents", "-n", "1", sha])?;
    let mut fields = stdout.split_whitespace();
    fields.next();
    Ok(fields.map(str::to_string).collect())
}

fn root_commit_changes(worktree: &Path, sha: &str) -> Result<Vec<FileChange>> {
    let stdout = run_git_bytes(
        worktree,
        &["-c", "core.quotepath=false", "ls-tree", "-r", "-z", sha],
    )?;

    parse_ls_tree_root(&stdout)
}

fn parse_ls_tree_root(stdout: &[u8]) -> Result<Vec<FileChange>> {
    nul_fields(stdout)
        .map(|entry| {
            let (header, path) = split_once(entry, b'\t').with_context(|| {
                format!(
                    "git ls-tree emitted entry without path: `{}`",
                    String::from_utf8_lossy(entry)
                )
            })?;
            let header = std::str::from_utf8(header)
                .with_context(|| format!("git ls-tree emitted non-UTF-8 metadata: {header:?}"))?;
            let mut fields = header.split_whitespace();
            let mode = fields
                .next()
                .with_context(|| format!("git ls-tree emitted malformed entry `{header}`"))?;
            let object_type = fields
                .next()
                .with_context(|| format!("git ls-tree emitted malformed entry `{header}`"))?;
            let oid = fields
                .next()
                .with_context(|| format!("git ls-tree emitted malformed entry `{header}`"))?;
            let kind = if mode == "160000" || object_type == "commit" {
                FileChangeKind::Gitlink {
                    old_oid: None,
                    new_oid: oid_option(oid),
                }
            } else {
                FileChangeKind::Added
            };

            Ok(FileChange {
                path: GitPath::from_bytes(path.to_vec()),
                kind,
                parent_sha: None,
            })
        })
        .collect()
}

fn parse_raw_diff(
    stdout: &[u8],
    parent_sha: Option<String>,
    changes: &mut Vec<FileChange>,
) -> Result<()> {
    let mut fields = nul_fields(stdout);

    while let Some(header) = fields.next() {
        let header = std::str::from_utf8(header)
            .with_context(|| format!("git diff-tree emitted non-UTF-8 raw header {header:?}"))?;
        let mut parts = header.split_whitespace();
        let old_mode = parts
            .next()
            .and_then(|mode| mode.strip_prefix(':'))
            .with_context(|| format!("git diff-tree emitted malformed raw header `{header}`"))?;
        let new_mode = parts
            .next()
            .with_context(|| format!("git diff-tree emitted malformed raw header `{header}`"))?;
        let old_oid = parts
            .next()
            .with_context(|| format!("git diff-tree emitted malformed raw header `{header}`"))?;
        let new_oid = parts
            .next()
            .with_context(|| format!("git diff-tree emitted malformed raw header `{header}`"))?;
        let status = parts
            .next()
            .with_context(|| format!("git diff-tree emitted malformed raw header `{header}`"))?;
        let status_kind = status.as_bytes().first().copied().unwrap_or_default();

        let path1 = fields.next().with_context(|| {
            format!(
                "git diff-tree emitted raw status `{}` without a path",
                String::from_utf8_lossy(status.as_bytes())
            )
        })?;

        let kind = if old_mode == "160000" || new_mode == "160000" {
            FileChangeKind::Gitlink {
                old_oid: oid_option(old_oid),
                new_oid: oid_option(new_oid),
            }
        } else {
            match status_kind {
                b'A' => FileChangeKind::Added,
                b'M' | b'T' => FileChangeKind::Modified,
                b'D' => FileChangeKind::Deleted,
                b'R' | b'C' => FileChangeKind::Renamed {
                    from: GitPath::from_bytes(path1.to_vec()),
                },
                other => bail!(
                    "unexpected diff status `{}` in `{status}`",
                    char::from(other)
                ),
            }
        };

        let path = match &kind {
            FileChangeKind::Renamed { .. } | FileChangeKind::Gitlink { .. }
                if status_kind == b'R' || status_kind == b'C' =>
            {
                let path2 = fields.next().with_context(|| {
                    format!(
                        "git diff-tree emitted rename/copy `{status}` without a destination path"
                    )
                })?;
                GitPath::from_bytes(path2.to_vec())
            }
            FileChangeKind::Added
            | FileChangeKind::Modified
            | FileChangeKind::Deleted
            | FileChangeKind::Gitlink { .. } => GitPath::from_bytes(path1.to_vec()),
            FileChangeKind::Renamed { .. } => {
                unreachable!("kind=Renamed implies status_kind in [R,C]; see lines 1339-1340")
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

fn oid_option(oid: &str) -> Option<String> {
    (!oid.as_bytes().iter().all(|byte| *byte == b'0')).then(|| oid.to_owned())
}

fn split_once(bytes: &[u8], needle: u8) -> Option<(&[u8], &[u8])> {
    bytes
        .iter()
        .position(|byte| *byte == needle)
        .map(|index| (&bytes[..index], &bytes[index + 1..]))
}

fn nul_fields(stdout: &[u8]) -> impl Iterator<Item = &[u8]> {
    stdout.split(|b| *b == 0).filter(|field| !field.is_empty())
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
#[path = "rename_corpus_tests.rs"]
mod rename_corpus_tests;

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::schema::ChangeKind;

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
            .to_owned()
    }

    #[test]
    fn gix_open_ignores_replace_refs() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), b"fn a() -> u8 { 1 }\n").unwrap();
        let a = commit(dir.path(), "A");
        std::fs::write(dir.path().join("a.rs"), b"fn a() -> u8 { 2 }\n").unwrap();
        let b = commit(dir.path(), "B");
        std::fs::write(dir.path().join("a.rs"), b"fn a() -> u8 { 3 }\n").unwrap();
        let c = commit(dir.path(), "C");
        run_git(dir.path(), &["replace", &c, &a]).unwrap();

        let repo = open_gix_repo_without_replace_refs(dir.path()).unwrap();
        assert_eq!(
            repo.config_snapshot().boolean("core.useReplaceRefs"),
            Some(false),
            "opener must expose git-compatible replace-ref config"
        );
        let commit = repo
            .find_commit(gix::ObjectId::from_hex(c.as_bytes()).unwrap())
            .unwrap();
        let parents: Vec<_> = commit
            .parent_ids()
            .map(|parent| parent.to_hex().to_string())
            .collect();

        assert_eq!(parents, vec![b]);
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
    fn snapshot_refs_returns_head_tip_when_detached() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let sha = commit(dir.path(), "init");
        run_git(dir.path(), &["checkout", "--detach", "-q", &sha]).unwrap();

        let snap = snapshot_refs(dir.path(), &["HEAD"]).unwrap();

        assert_eq!(snap.get("HEAD"), Some(&sha));
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

    #[test]
    fn parse_raw_diff_handles_copy_status_like_rename() {
        let mut stdout = Vec::new();
        stdout.extend_from_slice(b":100644 100644 0000000000000000000000000000000000000001 0000000000000000000000000000000000000002 C100");
        stdout.push(0);
        stdout.extend_from_slice(b"src/original.rs");
        stdout.push(0);
        stdout.extend_from_slice(b"src/copied.rs");
        stdout.push(0);

        let mut changes = Vec::new();
        parse_raw_diff(&stdout, Some("deadbeef".into()), &mut changes)
            .expect("copy status must parse");

        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        assert_eq!(change.path.display().to_string(), "src/copied.rs");
        match &change.kind {
            FileChangeKind::Renamed { from } => {
                assert_eq!(from.display().to_string(), "src/original.rs");
            }
            other => panic!("expected Renamed (copy treated as rename), got {other:?}"),
        }
    }

    #[test]
    fn symbol_diff_classifies_added_modified_deleted() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"fn a() {}\nfn b() {}\n").unwrap();
        commit(dir.path(), "c1");
        std::fs::write(dir.path().join("lib.rs"), b"fn a() { 42; }\nfn c() {}\n").unwrap();
        let sha2 = commit(dir.path(), "c2");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha2).unwrap();
        let changes =
            symbol_changes_for_commit(dir.path(), &sha2, &file_changes, &mut ctx).unwrap();
        let by_name: std::collections::HashMap<_, _> = changes
            .iter()
            .map(|c| (c.snapshot.entity_name.clone(), &c.change_kind))
            .collect();

        assert!(matches!(by_name.get("a"), Some(ChangeKind::Modified)));
        assert!(matches!(by_name.get("c"), Some(ChangeKind::Added)));
        assert!(matches!(by_name.get("b"), Some(ChangeKind::Deleted)));
    }

    #[test]
    fn symbol_diff_marks_byte_start_shift_as_modified() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"fn target() { 1 }\n").unwrap();
        commit(dir.path(), "c1");
        std::fs::write(
            dir.path().join("lib.rs"),
            b"// inserted before target\nfn target() { 1 }\n",
        )
        .unwrap();
        let sha2 = commit(dir.path(), "c2");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha2).unwrap();
        let changes =
            symbol_changes_for_commit(dir.path(), &sha2, &file_changes, &mut ctx).unwrap();
        let target = changes
            .iter()
            .find(|change| change.snapshot.entity_name == "target")
            .expect("target should be emitted when stable id changes");

        assert!(matches!(target.change_kind, ChangeKind::Modified));
        assert_eq!(target.snapshot.byte_range[0], 26);
    }

    #[test]
    fn tier1_file_rename_inheritance_matches_same_name_kind() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("old.rs"), b"pub fn helper() { 1; 2; 3; }\n").unwrap();
        commit(dir.path(), "c1");
        std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
        let sha = commit(dir.path(), "rename");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        let changes = symbol_changes_for_commit(dir.path(), &sha, &file_changes, &mut ctx).unwrap();
        let helper = changes
            .iter()
            .find(|c| c.snapshot.entity_name == "helper")
            .unwrap();

        assert!(matches!(&helper.change_kind, ChangeKind::RenamedFrom(_)));
    }

    #[test]
    fn tier2_jaccard_matches_renamed_body_similar() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn old_name(a: u32, b: u32) -> u32 { a + b * 2 }\n",
        )
        .unwrap();
        commit(dir.path(), "c1");
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn new_name(a: u32, b: u32) -> u32 { a + b * 2 }\n",
        )
        .unwrap();
        let sha = commit(dir.path(), "c2");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        let changes = symbol_changes_for_commit(dir.path(), &sha, &file_changes, &mut ctx).unwrap();
        let renamed = changes
            .iter()
            .find(|c| c.snapshot.entity_name == "new_name")
            .unwrap();

        assert!(matches!(&renamed.change_kind, ChangeKind::RenamedFrom(_)));
    }

    #[test]
    fn tier3_ambiguous_falls_back_to_added_deleted() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"pub fn old() { 1 }\n").unwrap();
        commit(dir.path(), "c1");
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn a() { 1 }\npub fn b() { 1 }\n",
        )
        .unwrap();
        let sha = commit(dir.path(), "c2");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        let changes = symbol_changes_for_commit(dir.path(), &sha, &file_changes, &mut ctx).unwrap();
        let kinds: Vec<_> = changes.iter().map(|c| &c.change_kind).collect();

        assert!(kinds.iter().any(|k| matches!(k, ChangeKind::Deleted)));
        assert_eq!(
            kinds
                .iter()
                .filter(|k| matches!(k, ChangeKind::Added))
                .count(),
            2
        );
        assert!(!kinds
            .iter()
            .any(|k| matches!(k, ChangeKind::RenamedFrom(_))));
        assert!(ctx
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.contains("ambiguous_rename")));
    }

    #[test]
    fn merge_collision_emits_added_and_keeps_deleted() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn old_a(x: u32) -> u32 { x + 1 }\npub fn old_b(x: u32) -> u32 { x + 1 }\n",
        )
        .unwrap();
        commit(dir.path(), "c1");
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn merged_target(x: u32) -> u32 { x + 1 }\n",
        )
        .unwrap();
        let sha = commit(dir.path(), "c2");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        let changes = symbol_changes_for_commit(dir.path(), &sha, &file_changes, &mut ctx).unwrap();
        let added: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.change_kind, ChangeKind::Added))
            .collect();
        let deleted: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.change_kind, ChangeKind::Deleted))
            .collect();
        let renamed: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c.change_kind, ChangeKind::RenamedFrom(_)))
            .collect();

        assert_eq!(added.len(), 1, "merged_target should be Added");
        assert_eq!(deleted.len(), 2, "both olds should remain Deleted");
        assert!(
            renamed.is_empty(),
            "no RenamedFrom may be emitted in merge collision"
        );
        assert!(ctx
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.contains("merge_collision")));
    }

    #[test]
    fn tier2_jaccard_matches_preserves_matches_and_ambiguity_diagnostics() {
        let file_change = FileChange {
            path: GitPath::from("lib.rs"),
            kind: FileChangeKind::Modified,
            parent_sha: Some("parent".into()),
        };
        let deleted = vec![
            symbol_change("deleted_clean", "old_clean", &["a", "b", "c", "d"]),
            symbol_change("deleted_tie_a", "old_tie_a", &["t1", "t2", "t3", "t4"]),
            symbol_change("deleted_tie_b", "old_tie_b", &["t1", "t2", "t3", "t4"]),
            symbol_change("deleted_low", "old_low", &["x", "y", "z"]),
        ];
        let added = vec![
            symbol_change("added_clean", "new_clean", &["a", "b", "c", "d", "e"]),
            symbol_change("added_tie", "new_tie", &["t1", "t2", "t3", "t4"]),
            symbol_change("added_low", "new_low", &["x", "unrelated"]),
        ];
        let mut diagnostics = Vec::new();

        let matches = tier2_jaccard_matches(
            &deleted,
            &added,
            &file_change,
            Language::Rust,
            &mut diagnostics,
        );

        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].from.snapshot.key.stable_symbol_id,
            "deleted_clean"
        );
        assert_eq!(matches[0].to.snapshot.key.stable_symbol_id, "added_clean");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("merge_collision") && diagnostic.contains("candidate=new_tie")
        }));
        assert!(has_ambiguous_rename_diagnostic(
            &diagnostics,
            "added_tie",
            "deleted_tie_a"
        ));
        assert!(has_ambiguous_rename_diagnostic(
            &diagnostics,
            "added_tie",
            "deleted_tie_b"
        ));
        assert!(has_ambiguous_rename_diagnostic(
            &diagnostics,
            "added_low",
            "deleted_low"
        ));
    }

    fn symbol_change(stable_symbol_id: &str, entity_name: &str, tokens: &[&str]) -> SymbolChange {
        SymbolChange {
            snapshot: SymbolSnapshotArtifact {
                key: SnapshotKey {
                    stable_symbol_id: stable_symbol_id.into(),
                    commit: "commit".into(),
                },
                file_path: "lib.rs".into(),
                entity_name: entity_name.into(),
                symbol_kind: "function".into(),
                enclosing_scope: None,
                byte_range: [0, 1],
                line_range: [1, 1],
                anchor_hash: format!("hash-{stable_symbol_id}"),
                tokens: tokens.iter().map(|token| (*token).into()).collect(),
            },
            change_kind: ChangeKind::Added,
            parent_sha: Some("parent".into()),
        }
    }

    fn has_ambiguous_rename_diagnostic(
        diagnostics: &[String],
        stable_id: &str,
        other_stable_id: &str,
    ) -> bool {
        diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("ambiguous_rename")
                && diagnostic.contains(&format!("stable_symbol_id={stable_id}"))
                && diagnostic.contains(&format!("other_stable_symbol_id={other_stable_id}"))
        })
    }

    #[test]
    fn force_push_invalidates_and_rewalks_diverged_range() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), b"fn a() {}\n").unwrap();
        let sha1 = commit(dir.path(), "c1");
        std::fs::write(dir.path().join("a.rs"), b"fn a() { 1 }\n").unwrap();
        let sha2 = commit(dir.path(), "c2");

        run_git(dir.path(), &["reset", "--hard", &sha1]).unwrap();
        std::fs::write(dir.path().join("a.rs"), b"fn a() { 999 }\n").unwrap();
        let sha2b = commit(dir.path(), "c2b");

        let plan = plan_incremental_walk(dir.path(), Some(&sha1), &sha2b).unwrap();
        assert!(matches!(
            plan,
            IncrementalPlan::FastForward { from, to } if from == sha1 && to == sha2b
        ));

        let plan = plan_incremental_walk(dir.path(), Some(&sha2), &sha2b).unwrap();
        assert!(matches!(
            plan,
            IncrementalPlan::ForcePushRecover { merge_base: Some(base), to }
                if base == sha1 && to == sha2b
        ));
    }

    #[test]
    fn gitlink_emits_file_change_no_recurse() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        commit(dir.path(), "c1");
        let oid = "0123456789012345678901234567890123456789";
        run_git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000",
                oid,
                "vendor/submodule",
            ],
        )
        .unwrap();
        run_git(dir.path(), &["commit", "-q", "-m", "add gitlink"]).unwrap();
        let sha = run_git(dir.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();

        let changes = file_changes_for_commit(dir.path(), &sha).unwrap();

        let gitlink = changes
            .iter()
            .find(|change| change.path.ends_with("vendor/submodule"))
            .expect("gitlink change");
        assert!(matches!(
            &gitlink.kind,
            FileChangeKind::Gitlink {
                old_oid: None,
                new_oid: Some(new_oid),
            } if new_oid == oid
        ));
    }

    #[test]
    fn binary_blob_downgrades_to_file_level_and_logs() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"\0not rust\n").unwrap();
        let sha = commit(dir.path(), "binary rust extension");

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        let changes = symbol_changes_for_commit(dir.path(), &sha, &file_changes, &mut ctx).unwrap();

        assert!(changes.is_empty());
        assert!(ctx
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.contains("binary_blob")));
    }

    #[test]
    fn parse_cache_dedups_repeated_blob_oids() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"pub fn repeated_blob() {}\n").unwrap();
        let sha1 = commit(dir.path(), "add rust file");
        run_git(dir.path(), &["update-index", "--chmod=+x", "lib.rs"]).unwrap();
        run_git(dir.path(), &["commit", "-q", "-m", "mode only"]).unwrap();
        let sha2 = run_git(dir.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();

        let mut ctx = SymbolDiffCtx::new();
        let file_changes1 = file_changes_for_commit(dir.path(), &sha1).unwrap();
        symbol_changes_for_commit(dir.path(), &sha1, &file_changes1, &mut ctx).unwrap();
        let file_changes2 = file_changes_for_commit(dir.path(), &sha2).unwrap();

        assert!(file_changes2
            .iter()
            .any(|change| matches!(change.kind, FileChangeKind::Modified)));
        symbol_changes_for_commit(dir.path(), &sha2, &file_changes2, &mut ctx).unwrap();

        assert_eq!(ctx.parse_cache_len(), 1);
    }

    #[test]
    fn cat_file_batch_reads_multiple_blobs_and_reports_missing() {
        let dir = TempDir::new().unwrap();
        let fixture_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample_corpus");
        for path in ["src/lib.rs", "src/utils.rs", "expected_graph_index.json"] {
            let source = fixture_root.join(path);
            let destination = dir.path().join(path);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(source, destination).unwrap();
        }
        init_repo(dir.path());
        let sha = commit(dir.path(), "fixture");

        let valid_paths = [
            std::path::Path::new("src/lib.rs"),
            std::path::Path::new("src/utils.rs"),
            std::path::Path::new("expected_graph_index.json"),
        ];
        let mut batch = CatFileBatch::new(dir.path()).unwrap();
        for path in valid_paths {
            let expected = std::fs::read(dir.path().join(path)).unwrap();
            let actual = batch.read(&sha, path).unwrap().map(|(_oid, bytes)| bytes);

            assert_eq!(actual, Some(expected));
        }

        let missing = batch
            .read(&sha, std::path::Path::new("src/missing.rs"))
            .unwrap();

        assert_eq!(missing, None);
    }

    #[test]
    fn missing_blob_fails_closed_with_named_oid() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"fn missing_blob() {}\n").unwrap();
        let sha = commit(dir.path(), "blob to remove");
        let blob_oid = run_git(dir.path(), &["rev-parse", &format!("{sha}:lib.rs")])
            .unwrap()
            .trim()
            .to_owned();
        let object_path = git_dir(dir.path())
            .unwrap()
            .join("objects")
            .join(&blob_oid[..2])
            .join(&blob_oid[2..]);
        std::fs::remove_file(object_path).unwrap();

        let mut ctx = SymbolDiffCtx::new();
        let file_changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        let error =
            symbol_changes_for_commit(dir.path(), &sha, &file_changes, &mut ctx).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("missing blob"));
        assert!(message.contains(&blob_oid), "{message}");
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_path_does_not_panic() {
        use std::io::Write as _;
        use std::os::unix::ffi::OsStrExt as _;

        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"bad-\xff.rs"));
        let mut blob = Command::new("git")
            .current_dir(dir.path())
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        blob.stdin
            .as_mut()
            .unwrap()
            .write_all(b"fn non_utf8_path() {}\n")
            .unwrap();
        let blob_output = blob.wait_with_output().unwrap();
        assert!(blob_output.status.success());
        let blob_oid = String::from_utf8(blob_output.stdout)
            .unwrap()
            .trim()
            .to_owned();

        let mut tree_entry = Vec::new();
        tree_entry.extend_from_slice(b"100644 blob ");
        tree_entry.extend_from_slice(blob_oid.as_bytes());
        tree_entry.push(b'\t');
        tree_entry.extend_from_slice(path.as_os_str().as_bytes());
        tree_entry.push(0);
        let mut tree = Command::new("git")
            .current_dir(dir.path())
            .args(["mktree", "-z"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        tree.stdin.as_mut().unwrap().write_all(&tree_entry).unwrap();
        let tree_output = tree.wait_with_output().unwrap();
        assert!(tree_output.status.success());
        let tree_oid = String::from_utf8(tree_output.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let sha = run_git(
            dir.path(),
            &["commit-tree", &tree_oid, "-m", "non-utf8 path"],
        )
        .unwrap()
        .trim()
        .to_owned();

        let changes = file_changes_for_commit(dir.path(), &sha).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path.as_bytes(), path.as_os_str().as_bytes());

        let mut ctx = SymbolDiffCtx::new();
        let symbol_changes =
            symbol_changes_for_commit(dir.path(), &sha, &changes, &mut ctx).unwrap();
        assert!(symbol_changes
            .iter()
            .any(|change| change.snapshot.entity_name == "non_utf8_path"));
    }
}
