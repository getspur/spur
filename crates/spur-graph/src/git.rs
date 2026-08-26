use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context as _};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitStatusCode {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PorcelainV2EntryKind {
    Ordinary,
    Rename { score: u8 },
    Copy { score: u8 },
    Unmerged,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PorcelainV2Entry {
    pub path: String,
    pub old_path: Option<String>,
    pub index_status: GitStatusCode,
    pub worktree_status: GitStatusCode,
    pub kind: PorcelainV2EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsmonitorCapabilities {
    pub release_enabled: bool,
    pub built_in_supported: bool,
    pub local_filesystem: bool,
    pub watcher_healthy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsmonitorDaemonStatus {
    Healthy,
    Unsupported,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsmonitorFallbackReason {
    ReleaseDisabled,
    BuiltInUnsupported,
    NonLocalFilesystem,
    WatcherUnhealthy,
    OptimizedCommandFailed,
    OptimizedOutputMalformed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsmonitorStatusRoute {
    FsmonitorNative,
    ExactFallback(FsmonitorFallbackReason),
}

/// Correctness route for a request-scoped validation lease.
///
/// Git's built-in fsmonitor can accelerate `git status` without exposing a
/// supported synchronous token fence to callers. Keep those capabilities
/// separate: a healthy status route is not, by itself, permission to skip the
/// exact observation at either side of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsmonitorValidationRoute {
    TokenFence,
    ExactObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusObservation {
    pub entries: Vec<PorcelainV2Entry>,
    pub source: FsmonitorStatusRoute,
}

pub fn parse_porcelain_v2(out: &[u8]) -> anyhow::Result<Vec<PorcelainV2Entry>> {
    let records: Vec<_> = out.split(|byte| *byte == 0).collect();
    let mut entries = Vec::new();
    let mut index = 0;

    while index < records.len() {
        let record = records[index];
        if record.is_empty() {
            if index + 1 == records.len() {
                break;
            }
            return Err(anyhow!(
                "malformed porcelain-v2 output: empty record at field {index}"
            ));
        }

        match record[0] {
            b'#' => {
                if !record.starts_with(b"# ") {
                    return Err(anyhow!("malformed porcelain-v2 header at field {index}"));
                }
                std::str::from_utf8(record)
                    .with_context(|| format!("porcelain-v2 header {index} is not UTF-8"))?;
            }
            b'1' => entries.push(parse_ordinary_porcelain_v2(record, index)?),
            b'2' => {
                let old_path = records.get(index + 1).copied().ok_or_else(|| {
                    anyhow!("malformed porcelain-v2 rename/copy record {index}: missing old path")
                })?;
                if old_path.is_empty() {
                    return Err(anyhow!(
                        "malformed porcelain-v2 rename/copy record {index}: empty old path"
                    ));
                }
                entries.push(parse_rename_or_copy_porcelain_v2(record, old_path, index)?);
                index += 1;
            }
            b'u' => entries.push(parse_unmerged_porcelain_v2(record, index)?),
            b'?' => entries.push(parse_special_porcelain_v2(
                record,
                index,
                b'?',
                GitStatusCode::Untracked,
                PorcelainV2EntryKind::Untracked,
            )?),
            b'!' => entries.push(parse_special_porcelain_v2(
                record,
                index,
                b'!',
                GitStatusCode::Ignored,
                PorcelainV2EntryKind::Ignored,
            )?),
            tag => {
                return Err(anyhow!(
                    "unsupported porcelain-v2 record type {:?} at field {index}",
                    char::from(tag)
                ));
            }
        }
        index += 1;
    }

    entries.sort();
    entries.dedup();
    Ok(entries)
}

fn parse_ordinary_porcelain_v2(record: &[u8], index: usize) -> anyhow::Result<PorcelainV2Entry> {
    let fields = porcelain_v2_fields(record, 9, index, "ordinary")?;
    require_porcelain_v2_tag(fields[0], b"1", index, "ordinary")?;
    let (index_status, worktree_status) = parse_porcelain_v2_xy(fields[1], index)?;
    Ok(PorcelainV2Entry {
        path: porcelain_v2_path(fields[8], index, "path")?,
        old_path: None,
        index_status,
        worktree_status,
        kind: PorcelainV2EntryKind::Ordinary,
    })
}

fn parse_rename_or_copy_porcelain_v2(
    record: &[u8],
    old_path: &[u8],
    index: usize,
) -> anyhow::Result<PorcelainV2Entry> {
    let fields = porcelain_v2_fields(record, 10, index, "rename/copy")?;
    require_porcelain_v2_tag(fields[0], b"2", index, "rename/copy")?;
    let (index_status, worktree_status) = parse_porcelain_v2_xy(fields[1], index)?;
    let score = fields[8];
    let Some((&operation, digits)) = score.split_first() else {
        return Err(anyhow!(
            "malformed porcelain-v2 rename/copy record {index}: missing score"
        ));
    };
    let score = std::str::from_utf8(digits)
        .with_context(|| format!("porcelain-v2 rename/copy score {index} is not UTF-8"))?
        .parse::<u8>()
        .with_context(|| format!("invalid porcelain-v2 rename/copy score at field {index}"))?;
    if score > 100 {
        return Err(anyhow!(
            "invalid porcelain-v2 rename/copy score {score} at field {index}"
        ));
    }

    let kind = match operation {
        b'R' if matches!(index_status, GitStatusCode::Renamed)
            || matches!(worktree_status, GitStatusCode::Renamed) =>
        {
            PorcelainV2EntryKind::Rename { score }
        }
        b'C' if matches!(index_status, GitStatusCode::Copied)
            || matches!(worktree_status, GitStatusCode::Copied) =>
        {
            PorcelainV2EntryKind::Copy { score }
        }
        b'R' | b'C' => {
            return Err(anyhow!(
                "porcelain-v2 rename/copy operation disagrees with XY at field {index}"
            ));
        }
        _ => {
            return Err(anyhow!(
                "invalid porcelain-v2 rename/copy operation at field {index}"
            ));
        }
    };

    Ok(PorcelainV2Entry {
        path: porcelain_v2_path(fields[9], index, "new path")?,
        old_path: Some(porcelain_v2_path(old_path, index, "old path")?),
        index_status,
        worktree_status,
        kind,
    })
}

fn parse_unmerged_porcelain_v2(record: &[u8], index: usize) -> anyhow::Result<PorcelainV2Entry> {
    let fields = porcelain_v2_fields(record, 11, index, "unmerged")?;
    require_porcelain_v2_tag(fields[0], b"u", index, "unmerged")?;
    let (index_status, worktree_status) = parse_porcelain_v2_xy(fields[1], index)?;
    Ok(PorcelainV2Entry {
        path: porcelain_v2_path(fields[10], index, "path")?,
        old_path: None,
        index_status,
        worktree_status,
        kind: PorcelainV2EntryKind::Unmerged,
    })
}

fn parse_special_porcelain_v2(
    record: &[u8],
    index: usize,
    tag: u8,
    worktree_status: GitStatusCode,
    kind: PorcelainV2EntryKind,
) -> anyhow::Result<PorcelainV2Entry> {
    let expected_prefix = [tag, b' '];
    let path = record.strip_prefix(&expected_prefix).ok_or_else(|| {
        anyhow!(
            "malformed porcelain-v2 {:?} record at field {index}",
            char::from(tag)
        )
    })?;
    Ok(PorcelainV2Entry {
        path: porcelain_v2_path(path, index, "path")?,
        old_path: None,
        index_status: GitStatusCode::Unmodified,
        worktree_status,
        kind,
    })
}

fn porcelain_v2_fields<'a>(
    record: &'a [u8],
    expected: usize,
    index: usize,
    kind: &str,
) -> anyhow::Result<Vec<&'a [u8]>> {
    let fields: Vec<_> = record.splitn(expected, |byte| *byte == b' ').collect();
    if fields.len() != expected || fields.iter().any(|field| field.is_empty()) {
        return Err(anyhow!(
            "malformed porcelain-v2 {kind} record at field {index}: expected {expected} fields"
        ));
    }
    Ok(fields)
}

fn require_porcelain_v2_tag(
    actual: &[u8],
    expected: &[u8],
    index: usize,
    kind: &str,
) -> anyhow::Result<()> {
    if actual != expected {
        return Err(anyhow!(
            "malformed porcelain-v2 {kind} tag at field {index}"
        ));
    }
    Ok(())
}

fn parse_porcelain_v2_xy(
    xy: &[u8],
    index: usize,
) -> anyhow::Result<(GitStatusCode, GitStatusCode)> {
    if xy.len() != 2 {
        return Err(anyhow!(
            "malformed porcelain-v2 XY at field {index}: expected two status bytes"
        ));
    }
    Ok((
        parse_porcelain_v2_status(xy[0], index)?,
        parse_porcelain_v2_status(xy[1], index)?,
    ))
}

fn parse_porcelain_v2_status(status: u8, index: usize) -> anyhow::Result<GitStatusCode> {
    match status {
        b'.' => Ok(GitStatusCode::Unmodified),
        b'M' => Ok(GitStatusCode::Modified),
        b'A' => Ok(GitStatusCode::Added),
        b'D' => Ok(GitStatusCode::Deleted),
        b'R' => Ok(GitStatusCode::Renamed),
        b'C' => Ok(GitStatusCode::Copied),
        b'T' => Ok(GitStatusCode::TypeChanged),
        b'U' => Ok(GitStatusCode::Unmerged),
        _ => Err(anyhow!(
            "unsupported porcelain-v2 status {:?} at field {index}",
            char::from(status)
        )),
    }
}

fn porcelain_v2_path(path: &[u8], index: usize, label: &str) -> anyhow::Result<String> {
    if path.is_empty() {
        return Err(anyhow!(
            "malformed porcelain-v2 record at field {index}: empty {label}"
        ));
    }
    std::str::from_utf8(path)
        .with_context(|| format!("porcelain-v2 {label} at field {index} is not UTF-8"))
        .map(ToOwned::to_owned)
}

pub fn fsmonitor_status_route(capabilities: FsmonitorCapabilities) -> FsmonitorStatusRoute {
    if !capabilities.release_enabled {
        FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::ReleaseDisabled)
    } else if !capabilities.built_in_supported {
        FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::BuiltInUnsupported)
    } else if !capabilities.local_filesystem {
        FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::NonLocalFilesystem)
    } else if !capabilities.watcher_healthy {
        FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::WatcherUnhealthy)
    } else {
        FsmonitorStatusRoute::FsmonitorNative
    }
}

pub fn fsmonitor_validation_route(
    capabilities: FsmonitorCapabilities,
    synchronous_token_fence_supported: bool,
) -> FsmonitorValidationRoute {
    if synchronous_token_fence_supported
        && matches!(
            fsmonitor_status_route(capabilities),
            FsmonitorStatusRoute::FsmonitorNative
        )
    {
        FsmonitorValidationRoute::TokenFence
    } else {
        FsmonitorValidationRoute::ExactObservation
    }
}

/// Probe the production validation route.
///
/// Git 2.55's public `fsmonitor--daemon` command surface exposes daemon
/// lifecycle operations, but no synchronous token query. The supported route
/// therefore remains exact even when its status observation may use fsmonitor.
pub fn probe_fsmonitor_validation_route(
    root: &Path,
    release_enabled: bool,
    local_filesystem: bool,
) -> FsmonitorValidationRoute {
    fsmonitor_validation_route(
        probe_fsmonitor_capabilities(root, release_enabled, local_filesystem),
        false,
    )
}

pub fn fsmonitor_capabilities_from_daemon_status(
    release_enabled: bool,
    local_filesystem: bool,
    daemon_status: FsmonitorDaemonStatus,
) -> FsmonitorCapabilities {
    FsmonitorCapabilities {
        release_enabled,
        built_in_supported: !matches!(daemon_status, FsmonitorDaemonStatus::Unsupported),
        local_filesystem,
        watcher_healthy: matches!(daemon_status, FsmonitorDaemonStatus::Healthy),
    }
}

pub fn probe_fsmonitor_capabilities(
    root: &Path,
    release_enabled: bool,
    local_filesystem: bool,
) -> FsmonitorCapabilities {
    let daemon_status = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["fsmonitor--daemon", "status"])
        .output()
    {
        Ok(output) if output.status.success() => FsmonitorDaemonStatus::Healthy,
        Ok(output) if fsmonitor_daemon_is_unsupported(&output.stderr) => {
            FsmonitorDaemonStatus::Unsupported
        }
        Ok(_) => FsmonitorDaemonStatus::Unhealthy,
        Err(_) => FsmonitorDaemonStatus::Unsupported,
    };
    fsmonitor_capabilities_from_daemon_status(release_enabled, local_filesystem, daemon_status)
}

fn fsmonitor_daemon_is_unsupported(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    [
        "not supported",
        "not a git command",
        "unknown subcommand",
        "unknown option",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}

pub fn status_observation(
    root: &Path,
    capabilities: FsmonitorCapabilities,
) -> anyhow::Result<GitStatusObservation> {
    status_observation_with_runner(capabilities, |args| git_stdout_bytes(root, args))
}

fn status_observation_with_runner<F>(
    capabilities: FsmonitorCapabilities,
    mut run: F,
) -> anyhow::Result<GitStatusObservation>
where
    F: FnMut(&[&str]) -> anyhow::Result<Vec<u8>>,
{
    const OPTIMIZED_ARGS: &[&str] = &[
        "-c",
        "core.fsmonitor=true",
        "-c",
        "core.untrackedCache=true",
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
    ];
    const EXACT_ARGS: &[&str] = &[
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
    ];

    match fsmonitor_status_route(capabilities) {
        FsmonitorStatusRoute::FsmonitorNative => match run(OPTIMIZED_ARGS) {
            Ok(out) => match parse_porcelain_v2(&out) {
                Ok(entries) => Ok(GitStatusObservation {
                    entries,
                    source: FsmonitorStatusRoute::FsmonitorNative,
                }),
                Err(optimized_error) => exact_status_observation(
                    &mut run,
                    EXACT_ARGS,
                    FsmonitorFallbackReason::OptimizedOutputMalformed,
                    Some(optimized_error),
                ),
            },
            Err(optimized_error) => exact_status_observation(
                &mut run,
                EXACT_ARGS,
                FsmonitorFallbackReason::OptimizedCommandFailed,
                Some(optimized_error),
            ),
        },
        FsmonitorStatusRoute::ExactFallback(reason) => {
            exact_status_observation(&mut run, EXACT_ARGS, reason, None)
        }
    }
}

fn exact_status_observation<F>(
    run: &mut F,
    args: &[&str],
    reason: FsmonitorFallbackReason,
    optimized_error: Option<anyhow::Error>,
) -> anyhow::Result<GitStatusObservation>
where
    F: FnMut(&[&str]) -> anyhow::Result<Vec<u8>>,
{
    let out = match run(args) {
        Ok(out) => out,
        Err(exact_error) => {
            return match optimized_error {
                Some(optimized_error) => Err(anyhow!(
                    "optimized git status failed ({optimized_error:#}); exact fallback also failed: {exact_error:#}"
                )),
                None => Err(exact_error).context("exact git status observation failed"),
            };
        }
    };
    let entries = match parse_porcelain_v2(&out) {
        Ok(entries) => entries,
        Err(exact_error) => {
            return match optimized_error {
                Some(optimized_error) => Err(anyhow!(
                    "optimized git status was unusable ({optimized_error:#}); exact fallback output was also malformed: {exact_error:#}"
                )),
                None => Err(exact_error).context("exact git status output was malformed"),
            };
        }
    };
    Ok(GitStatusObservation {
        entries,
        source: FsmonitorStatusRoute::ExactFallback(reason),
    })
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

/// Returns the roots of every worktree registered with the repository at `root`.
///
/// The roots retain the deterministic order emitted by Git.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
///
/// let roots = spur_graph::git::registered_worktree_roots(Path::new("."))?;
/// assert!(!roots.is_empty());
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error when Git cannot enumerate the worktrees or its
/// NUL-delimited porcelain output contains malformed or non-UTF-8 records.
pub fn registered_worktree_roots(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let out = git_stdout_bytes(root, &["worktree", "list", "--porcelain", "-z"])?;
    parse_registered_worktree_roots(&out).with_context(|| {
        format!(
            "failed to parse registered worktrees for `{}`",
            root.display()
        )
    })
}

fn parse_registered_worktree_roots(out: &[u8]) -> anyhow::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();

    for (index, record) in out
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .enumerate()
    {
        let record = std::str::from_utf8(record).with_context(|| {
            format!("git worktree list emitted non-UTF-8 record at field {index}")
        })?;
        if let Some(path) = record.strip_prefix("worktree ") {
            if path.is_empty() {
                return Err(anyhow!(
                    "malformed git worktree record at field {index}: missing path"
                ));
            }
            roots.push(PathBuf::from(path));
        } else if record.starts_with("worktree") {
            return Err(anyhow!(
                "malformed git worktree record at field {index}: expected `worktree <path>`"
            ));
        }
    }

    if roots.is_empty() {
        return Err(anyhow!(
            "malformed git worktree list output: no worktree records"
        ));
    }

    Ok(roots)
}

pub fn rev_parse_head(root: &Path) -> anyhow::Result<String> {
    git_stdout(root, &["rev-parse", "HEAD"]).map(|out| out.trim_end().to_owned())
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
            oid.to_owned()
        };
        by_path.insert(
            path.to_owned(),
            TrackedEntry {
                path: path.to_owned(),
                oid: oid.to_owned(),
                mode: mode.to_owned(),
                content_oid,
                is_gitlink,
            },
        );
    }

    Ok(by_path.into_values().collect())
}

pub fn status_dirty_paths(root: &Path) -> anyhow::Result<Vec<DirtyEntry>> {
    let observation = status_observation(
        root,
        FsmonitorCapabilities {
            release_enabled: false,
            built_in_supported: false,
            local_filesystem: true,
            watcher_healthy: false,
        },
    )?;
    Ok(observation
        .entries
        .into_iter()
        .map(|entry| DirtyEntry {
            path: entry.path,
            status: porcelain_v1_compatible_status(entry.index_status, entry.worktree_status),
        })
        .collect())
}

fn porcelain_v1_compatible_status(
    index_status: GitStatusCode,
    worktree_status: GitStatusCode,
) -> String {
    if matches!(worktree_status, GitStatusCode::Untracked) {
        return "??".to_owned();
    }
    if matches!(worktree_status, GitStatusCode::Ignored) {
        return "!!".to_owned();
    }
    [
        porcelain_v1_status_byte(index_status),
        porcelain_v1_status_byte(worktree_status),
    ]
    .into_iter()
    .collect()
}

fn porcelain_v1_status_byte(status: GitStatusCode) -> char {
    match status {
        GitStatusCode::Unmodified => ' ',
        GitStatusCode::Modified => 'M',
        GitStatusCode::Added => 'A',
        GitStatusCode::Deleted => 'D',
        GitStatusCode::Renamed => 'R',
        GitStatusCode::Copied => 'C',
        GitStatusCode::TypeChanged => 'T',
        GitStatusCode::Unmerged => 'U',
        GitStatusCode::Untracked => '?',
        GitStatusCode::Ignored => '!',
    }
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
            paths.insert(path.to_owned());
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

    use anyhow::anyhow;
    use tempfile::TempDir;

    use super::{
        detect, fsmonitor_capabilities_from_daemon_status, fsmonitor_status_route,
        fsmonitor_validation_route, ls_files_with_oids, parse_porcelain_v2,
        parse_registered_worktree_roots, registered_worktree_roots, rev_parse_common_dir,
        rev_parse_head, status_dirty_paths, status_observation_with_runner, FsmonitorCapabilities,
        FsmonitorDaemonStatus, FsmonitorFallbackReason, FsmonitorStatusRoute,
        FsmonitorValidationRoute, GitStatusCode, PorcelainV2EntryKind,
    };

    #[test]
    fn porcelain_v2_parses_all_changed_entry_kinds_and_spaces() {
        let raw = concat!(
            "1 .M N... 100644 100644 100644 aaaa bbbb ordinary file.rs\0",
            "1 M. N... 100644 100644 100644 aaaa bbbb staged.rs\0",
            "1 D. N... 100644 000000 000000 aaaa 0000 staged-delete.rs\0",
            "1 .D N... 100644 100644 000000 aaaa bbbb worktree-delete.rs\0",
            "? untracked file.rs\0",
            "2 R. N... 100644 100644 100644 aaaa bbbb R100 renamed file.rs\0old file.rs\0",
            "2 C. N... 100644 100644 100644 aaaa bbbb C075 copied file.rs\0source file.rs\0",
        );

        let entries = parse_porcelain_v2(raw.as_bytes()).unwrap_or_default();
        let ordinary = entries
            .iter()
            .find(|entry| entry.path == "ordinary file.rs")
            .expect("ordinary entry");
        assert_eq!(ordinary.kind, PorcelainV2EntryKind::Ordinary);
        assert_eq!(ordinary.index_status, GitStatusCode::Unmodified);
        assert_eq!(ordinary.worktree_status, GitStatusCode::Modified);

        let staged = entries
            .iter()
            .find(|entry| entry.path == "staged.rs")
            .expect("staged entry");
        assert_eq!(staged.index_status, GitStatusCode::Modified);
        assert_eq!(staged.worktree_status, GitStatusCode::Unmodified);

        let staged_delete = entries
            .iter()
            .find(|entry| entry.path == "staged-delete.rs")
            .expect("staged delete");
        assert_eq!(staged_delete.index_status, GitStatusCode::Deleted);
        assert_eq!(staged_delete.worktree_status, GitStatusCode::Unmodified);

        let worktree_delete = entries
            .iter()
            .find(|entry| entry.path == "worktree-delete.rs")
            .expect("worktree delete");
        assert_eq!(worktree_delete.index_status, GitStatusCode::Unmodified);
        assert_eq!(worktree_delete.worktree_status, GitStatusCode::Deleted);

        let untracked = entries
            .iter()
            .find(|entry| entry.path == "untracked file.rs")
            .expect("untracked entry");
        assert_eq!(untracked.kind, PorcelainV2EntryKind::Untracked);
        assert_eq!(untracked.worktree_status, GitStatusCode::Untracked);

        let renamed = entries
            .iter()
            .find(|entry| entry.path == "renamed file.rs")
            .expect("rename entry");
        assert_eq!(renamed.old_path.as_deref(), Some("old file.rs"));
        assert_eq!(renamed.kind, PorcelainV2EntryKind::Rename { score: 100 });

        let copied = entries
            .iter()
            .find(|entry| entry.path == "copied file.rs")
            .expect("copy entry");
        assert_eq!(copied.old_path.as_deref(), Some("source file.rs"));
        assert_eq!(copied.kind, PorcelainV2EntryKind::Copy { score: 75 });
    }

    #[test]
    fn porcelain_v2_rejects_malformed_or_unsupported_records() {
        let malformed: &[&[u8]] = &[
            b"1 .M too-few-fields\0",
            b"2 R. N... 100644 100644 100644 aaaa bbbb R100 new.rs\0",
            b"2 R. N... 100644 100644 100644 aaaa bbbb R101 new.rs\0old.rs\0",
            b"1 .X N... 100644 100644 100644 aaaa bbbb invalid.rs\0",
            b"1x .M N... 100644 100644 100644 aaaa bbbb invalid-tag.rs\0",
            b"x unsupported\0",
            b"? ok.rs\0\0? later.rs\0",
            b"? non-utf8-\xff.rs\0",
        ];

        for raw in malformed {
            assert!(
                parse_porcelain_v2(raw).is_err(),
                "accepted malformed porcelain-v2 record: {raw:?}"
            );
        }
    }

    #[test]
    fn fsmonitor_route_requires_all_four_capability_gates() {
        for mask in 0_u8..16 {
            let capabilities = FsmonitorCapabilities {
                release_enabled: mask & 0b1000 != 0,
                built_in_supported: mask & 0b0100 != 0,
                local_filesystem: mask & 0b0010 != 0,
                watcher_healthy: mask & 0b0001 != 0,
            };
            let route = fsmonitor_status_route(capabilities);
            assert_eq!(
                route == FsmonitorStatusRoute::FsmonitorNative,
                mask == 0b1111,
                "unexpected route for capability mask {mask:04b}: {route:?}"
            );
        }
    }

    #[test]
    fn validation_lease_requires_a_supported_synchronous_token_fence() {
        let capabilities = all_capabilities();

        assert_eq!(
            fsmonitor_validation_route(capabilities, false),
            FsmonitorValidationRoute::ExactObservation
        );
        assert_eq!(
            fsmonitor_validation_route(capabilities, true),
            FsmonitorValidationRoute::TokenFence
        );

        for mask in 0_u8..16 {
            let capabilities = FsmonitorCapabilities {
                release_enabled: mask & 0b1000 != 0,
                built_in_supported: mask & 0b0100 != 0,
                local_filesystem: mask & 0b0010 != 0,
                watcher_healthy: mask & 0b0001 != 0,
            };
            assert_eq!(
                fsmonitor_validation_route(capabilities, true),
                if mask == 0b1111 {
                    FsmonitorValidationRoute::TokenFence
                } else {
                    FsmonitorValidationRoute::ExactObservation
                },
                "unexpected validation route for capability mask {mask:04b}"
            );
        }
    }

    #[test]
    fn fsmonitor_route_reports_each_capability_fallback_reason() {
        let cases = [
            (
                FsmonitorCapabilities {
                    release_enabled: false,
                    ..all_capabilities()
                },
                FsmonitorFallbackReason::ReleaseDisabled,
            ),
            (
                FsmonitorCapabilities {
                    built_in_supported: false,
                    ..all_capabilities()
                },
                FsmonitorFallbackReason::BuiltInUnsupported,
            ),
            (
                FsmonitorCapabilities {
                    local_filesystem: false,
                    ..all_capabilities()
                },
                FsmonitorFallbackReason::NonLocalFilesystem,
            ),
            (
                FsmonitorCapabilities {
                    watcher_healthy: false,
                    ..all_capabilities()
                },
                FsmonitorFallbackReason::WatcherUnhealthy,
            ),
        ];

        for (capabilities, expected_reason) in cases {
            assert_eq!(
                fsmonitor_status_route(capabilities),
                FsmonitorStatusRoute::ExactFallback(expected_reason)
            );
        }
    }

    #[test]
    fn fsmonitor_daemon_status_is_an_injectable_capability_decision() {
        let healthy =
            fsmonitor_capabilities_from_daemon_status(true, true, FsmonitorDaemonStatus::Healthy);
        assert_eq!(
            healthy,
            FsmonitorCapabilities {
                release_enabled: true,
                built_in_supported: true,
                local_filesystem: true,
                watcher_healthy: true,
            }
        );

        let unsupported = fsmonitor_capabilities_from_daemon_status(
            true,
            true,
            FsmonitorDaemonStatus::Unsupported,
        );
        assert!(!unsupported.built_in_supported);
        assert!(!unsupported.watcher_healthy);

        let unhealthy =
            fsmonitor_capabilities_from_daemon_status(true, true, FsmonitorDaemonStatus::Unhealthy);
        assert!(unhealthy.built_in_supported);
        assert!(!unhealthy.watcher_healthy);
    }

    #[test]
    fn fsmonitor_uses_one_process_scoped_optimized_status_observation() {
        let mut calls = Vec::new();
        let observation = status_observation_with_runner(all_capabilities(), |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            Ok(b"? file with spaces.rs\0".to_vec())
        })
        .unwrap();

        assert_eq!(observation.source, FsmonitorStatusRoute::FsmonitorNative);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            [
                "-c",
                "core.fsmonitor=true",
                "-c",
                "core.untrackedCache=true",
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
            ]
        );
        assert!(!calls[0].iter().any(|arg| arg == "config"));
    }

    #[test]
    fn fsmonitor_gate_failure_uses_one_exact_process_scoped_observation() {
        let mut calls = Vec::new();
        let capabilities = FsmonitorCapabilities {
            watcher_healthy: false,
            ..all_capabilities()
        };
        let observation = status_observation_with_runner(capabilities, |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            Ok(b"? exact.rs\0".to_vec())
        })
        .unwrap();

        assert_eq!(
            observation.source,
            FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::WatcherUnhealthy)
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            [
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
            ]
        );
        assert!(!calls[0].iter().any(|arg| arg == "config"));
    }

    #[test]
    fn fsmonitor_command_failure_falls_back_exactly_in_the_same_request() {
        let mut calls = Vec::new();
        let mut attempt = 0;
        let observation = status_observation_with_runner(all_capabilities(), |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            attempt += 1;
            if attempt == 1 {
                Err(anyhow!("optimized command failed"))
            } else {
                Ok(b"? fallback.rs\0".to_vec())
            }
        })
        .unwrap();

        assert_eq!(calls.len(), 2);
        assert_eq!(
            observation.source,
            FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::OptimizedCommandFailed)
        );
        assert_eq!(observation.entries[0].path, "fallback.rs");
    }

    #[test]
    fn fsmonitor_malformed_output_falls_back_exactly_in_the_same_request() {
        let mut calls = Vec::new();
        let mut attempt = 0;
        let observation = status_observation_with_runner(all_capabilities(), |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            attempt += 1;
            if attempt == 1 {
                Ok(b"x malformed\0".to_vec())
            } else {
                Ok(b"? recovered.rs\0".to_vec())
            }
        })
        .unwrap();

        assert_eq!(calls.len(), 2);
        assert_eq!(
            observation.source,
            FsmonitorStatusRoute::ExactFallback(FsmonitorFallbackReason::OptimizedOutputMalformed)
        );
        assert_eq!(observation.entries[0].path, "recovered.rs");
    }

    #[test]
    fn fsmonitor_exact_command_or_parse_failure_is_returned() {
        let capabilities = FsmonitorCapabilities {
            release_enabled: false,
            ..all_capabilities()
        };
        let command_error =
            status_observation_with_runner(capabilities, |_| Err(anyhow!("exact command failed")))
                .unwrap_err();
        assert!(format!("{command_error:#}").contains("exact command failed"));

        let parse_error =
            status_observation_with_runner(capabilities, |_| Ok(b"x malformed\0".to_vec()))
                .unwrap_err();
        assert!(format!("{parse_error:#}").contains("unsupported porcelain-v2 record"));
    }

    #[test]
    fn fsmonitor_failed_optimized_and_exact_commands_return_both_errors() {
        let mut attempt = 0;
        let error = status_observation_with_runner(all_capabilities(), |_| {
            attempt += 1;
            if attempt == 1 {
                Err(anyhow!("optimized failed"))
            } else {
                Err(anyhow!("exact failed"))
            }
        })
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("optimized failed"), "{message}");
        assert!(message.contains("exact failed"), "{message}");
    }

    fn all_capabilities() -> FsmonitorCapabilities {
        FsmonitorCapabilities {
            release_enabled: true,
            built_in_supported: true,
            local_filesystem: true,
            watcher_healthy: true,
        }
    }

    #[test]
    fn registered_worktree_roots_includes_linked_path_with_spaces_in_git_order() {
        let repo = init_repo();
        commit_file(repo.path(), "README.md", "worktree fixture\n");
        let linked_parent = TempDir::new().unwrap();
        let linked_root = linked_parent.path().join("linked worktree");
        let linked_root_arg = linked_root.to_str().expect("linked worktree path is UTF-8");
        run_git(
            repo.path(),
            &["worktree", "add", "--detach", linked_root_arg],
        );

        let roots = registered_worktree_roots(repo.path()).unwrap();
        let canonical_roots: Vec<_> = roots
            .into_iter()
            .map(|root| root.canonicalize().unwrap())
            .collect();

        assert_eq!(
            canonical_roots,
            vec![
                repo.path().canonicalize().unwrap(),
                linked_root.canonicalize().unwrap(),
            ]
        );
    }

    #[test]
    fn registered_worktree_roots_rejects_empty_worktree_record() {
        let error = parse_registered_worktree_roots(b"worktree \0\0").unwrap_err();

        assert!(
            format!("{error:#}").contains("malformed git worktree record at field 0: missing path"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn registered_worktree_roots_rejects_non_utf8_record() {
        let error = parse_registered_worktree_roots(b"worktree /repo\0HEAD \xff\0\0").unwrap_err();

        assert!(
            format!("{error:#}").contains("git worktree list emitted non-UTF-8 record"),
            "unexpected error: {error:#}"
        );
    }

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
            .to_owned()
    }
}
