use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::extract::languages::Language;
use crate::extract::tree_sitter::{BytesExtractor, ExtractedSymbol};
use crate::schema::{ChangeKind, RenamePrev, SnapshotKey, SymbolSnapshotArtifact, WalkStrategy};

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

pub struct SymbolDiffCtx {
    extractors: HashMap<Language, BytesExtractor>,
    diagnostics: Vec<String>,
}

impl SymbolDiffCtx {
    pub fn new() -> Self {
        Self {
            extractors: HashMap::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    fn for_language(&mut self, language: Language) -> Result<&mut BytesExtractor> {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.extractors.entry(language) {
            entry.insert(BytesExtractor::for_language(language)?);
        }
        Ok(self
            .extractors
            .get_mut(&language)
            .expect("extractor was just inserted"))
    }

    fn record_diagnostics(&mut self, diagnostics: Vec<String>) {
        self.diagnostics.extend(diagnostics);
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
}

pub fn symbol_changes_for_commit(
    worktree: &Path,
    sha: &str,
    ctx: &mut SymbolDiffCtx,
) -> Result<Vec<SymbolChange>> {
    let mut out = Vec::new();
    let mut by_snapshot_key = HashMap::new();

    for file_change in file_changes_for_commit(worktree, sha)? {
        let Some(language) = Language::from_path(&file_change.path) else {
            continue;
        };

        let blobs = blobs_for_change(worktree, sha, &file_change)?;
        let deleted_path = blobs.left_path.as_deref().unwrap_or(&file_change.path);
        let (left_symbols, right_symbols) = {
            let extractor = ctx.for_language(language)?;
            (
                extract_symbols(extractor, blobs.left_path.as_deref(), &blobs.left),
                extract_symbols(extractor, Some(&file_change.path), &blobs.right),
            )
        };

        let mut direct_changes = Vec::new();
        let mut deleted_candidates = Vec::new();
        let mut added_candidates = Vec::new();

        if matches!(file_change.kind, FileChangeKind::Renamed { .. }) {
            deleted_candidates.extend(left_symbols.iter().map(|left| SymbolChange {
                snapshot: snapshot_from(sha, deleted_path, left),
                change_kind: ChangeKind::Deleted,
            }));
            added_candidates.extend(right_symbols.iter().map(|right| SymbolChange {
                snapshot: snapshot_from(sha, &file_change.path, right),
                change_kind: ChangeKind::Added,
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
                match left_by_identity.remove(&identity) {
                    Some(left) if left.anchor_hash == right.anchor_hash => continue,
                    Some(_) => direct_changes.push(SymbolChange {
                        snapshot: snapshot_from(sha, &file_change.path, right),
                        change_kind: ChangeKind::Modified,
                    }),
                    None => added_candidates.push(SymbolChange {
                        snapshot: snapshot_from(sha, &file_change.path, right),
                        change_kind: ChangeKind::Added,
                    }),
                }
            }

            for (_, left) in left_by_identity {
                deleted_candidates.push(SymbolChange {
                    snapshot: snapshot_from(sha, deleted_path, left),
                    change_kind: ChangeKind::Deleted,
                });
            }
        }

        let (rename_changes, diagnostics) =
            detect_renames(deleted_candidates, added_candidates, &file_change, language);
        ctx.record_diagnostics(diagnostics);

        for change in direct_changes.into_iter().chain(rename_changes) {
            push_symbol_change(&mut out, &mut by_snapshot_key, change);
        }
    }

    Ok(out)
}

fn push_symbol_change(
    out: &mut Vec<SymbolChange>,
    by_snapshot_key: &mut HashMap<SnapshotKey, usize>,
    change: SymbolChange,
) {
    if let Some(existing) = by_snapshot_key.get(&change.snapshot.key).copied() {
        let merged = merge_change_kind(&out[existing].change_kind, change.change_kind);
        out[existing].change_kind = merged;
        return;
    }

    by_snapshot_key.insert(change.snapshot.key.clone(), out.len());
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

    let mut matches = Vec::new();
    for added in added_candidates {
        let mut scored: Vec<_> = deleted_candidates
            .iter()
            .map(|deleted| (deleted, jaccard_tokens(&added.snapshot, &deleted.snapshot)))
            .collect();
        scored.sort_by(|(_, left), (_, right)| {
            right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal)
        });

        let Some((best_deleted, best_score)) = scored.first().copied() else {
            continue;
        };
        if best_score < threshold {
            diagnostics.push(ambiguous_rename_diagnostic(file_change, added));
            continue;
        }
        if let Some((_, second_score)) = scored.get(1).copied() {
            if second_score >= threshold {
                diagnostics.push(format!(
                    "merge_collision: file={} candidate={}",
                    file_change.path.display(),
                    added.snapshot.entity_name
                ));
                continue;
            }
            if best_score - second_score < 0.05 {
                diagnostics.push(ambiguous_rename_diagnostic(file_change, added));
                continue;
            }
        }

        matches.push(RenameMatch {
            from: best_deleted.clone(),
            to: added.clone(),
            score: best_score,
        });
    }

    reject_ambiguous_splits(matches, file_change, diagnostics)
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
        if scores[0].1 - scores[1].1 < 0.05 {
            for (index, _) in scores {
                rejected.insert(index);
                diagnostics.push(ambiguous_rename_diagnostic(file_change, &matches[index].to));
            }
        }
    }

    matches
        .into_iter()
        .enumerate()
        .filter_map(|(index, rename_match)| (!rejected.contains(&index)).then_some(rename_match))
        .collect()
}

fn ambiguous_rename_diagnostic(file_change: &FileChange, change: &SymbolChange) -> String {
    format!(
        "ambiguous_rename: file={} candidate={}",
        file_change.path.display(),
        change.snapshot.entity_name
    )
}

fn jaccard_tokens(a: &SymbolSnapshotArtifact, b: &SymbolSnapshotArtifact) -> f64 {
    let a_tokens: HashSet<_> = a.tokens.iter().collect();
    let b_tokens: HashSet<_> = b.tokens.iter().collect();
    let union = a_tokens.union(&b_tokens).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = a_tokens.intersection(&b_tokens).count();
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
    left_path: Option<PathBuf>,
    left: Option<Vec<u8>>,
    right: Option<Vec<u8>>,
}

fn blobs_for_change(worktree: &Path, sha: &str, file_change: &FileChange) -> Result<ChangeBlobs> {
    let right = match &file_change.kind {
        FileChangeKind::Deleted => None,
        FileChangeKind::Added | FileChangeKind::Modified | FileChangeKind::Renamed { .. } => {
            Some(cat_file_blob(worktree, sha, &file_change.path)?)
        }
    };

    let left_path = match &file_change.kind {
        FileChangeKind::Added => None,
        FileChangeKind::Modified | FileChangeKind::Deleted => Some(file_change.path.clone()),
        FileChangeKind::Renamed { from } => Some(from.clone()),
    };
    let left = match (file_change.parent_sha.as_deref(), left_path.as_ref()) {
        (Some(parent), Some(path)) => Some(cat_file_blob(worktree, parent, path)?),
        _ => None,
    };

    Ok(ChangeBlobs {
        left_path,
        left,
        right,
    })
}

fn cat_file_blob(worktree: &Path, sha: &str, path: &Path) -> Result<Vec<u8>> {
    let spec = format!("{sha}:{}", path.to_string_lossy());
    run_git_bytes(worktree, &["cat-file", "blob", &spec])
}

fn extract_symbols(
    extractor: &mut BytesExtractor,
    logical_path: Option<&Path>,
    bytes: &Option<Vec<u8>>,
) -> Vec<ExtractedSymbol> {
    match (logical_path, bytes.as_deref()) {
        (Some(path), Some(bytes)) => extractor.extract(path, bytes).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn snapshot_from(commit: &str, path: &Path, symbol: &ExtractedSymbol) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: crate::identity::stable_symbol_id_for(
                path,
                &symbol.entity_name,
                &symbol.anchor_hash,
            ),
            commit: commit.to_string(),
        },
        file_path: path.to_path_buf(),
        entity_name: symbol.entity_name.clone(),
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
        byte_range: symbol.byte_range,
        line_range: symbol.line_range,
        anchor_hash: symbol.anchor_hash.clone(),
        tokens: symbol.tokens.clone(),
    }
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

    #[test]
    fn symbol_diff_classifies_added_modified_deleted() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), b"fn a() {}\nfn b() {}\n").unwrap();
        commit(dir.path(), "c1");
        std::fs::write(dir.path().join("lib.rs"), b"fn a() { 42; }\nfn c() {}\n").unwrap();
        let sha2 = commit(dir.path(), "c2");

        let mut ctx = SymbolDiffCtx::new();
        let changes = symbol_changes_for_commit(dir.path(), &sha2, &mut ctx).unwrap();
        let by_name: std::collections::HashMap<_, _> = changes
            .iter()
            .map(|c| (c.snapshot.entity_name.clone(), &c.change_kind))
            .collect();

        assert!(matches!(by_name.get("a"), Some(ChangeKind::Modified)));
        assert!(matches!(by_name.get("c"), Some(ChangeKind::Added)));
        assert!(matches!(by_name.get("b"), Some(ChangeKind::Deleted)));
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
        let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
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
        let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
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
        let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
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
        let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
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
}
