//! Ingest flow — applies a `RemoteDelta` from an `ExternalPmSync` into
//! `.beads/beads.db` per §5 of `docs/architecture/spur-pm-github-ingest.md`.
//!
//! All state lives in `.beads/beads.db` (no sidecar): provenance via
//! columns, sync watermark via `spur-sync v1` sentinel comments,
//! per-imported-comment idempotency via `<!-- spur-import gh:<id> -->`
//! markers. See `watermark.rs` for the format helpers and §4 of the
//! spec for the rationale.
//!
//! Concurrency: the entire run holds an exclusive process-level lock
//! `.beads/.spur-ingest.lock` (separate from beads's `.write.lock`,
//! which is acquired by each `BeadsCrateAdapter::write`). A second
//! concurrent `apply_remote_delta` against the same `.beads/` fails
//! fast with "another ingest run is in progress (pid=N)". Each
//! per-node `adapter.write` closure additionally **re-reads the
//! watermark inside the closure** — defense-in-depth against the
//! process lock being bypassed.

pub mod dep_hints;
pub mod github;
pub mod watermark;

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::adapter::IssueTracker;
use crate::advanced::{BeadsAdvanced, Comment};
use crate::beads_crate::BeadsCrateAdapter;
use crate::sync::{
    ConflictReason, DepHint, ExternalPmSync, RemoteConflict, RemoteDelta, RemoteKind, RemoteNode,
    RemoteRef, RemoteState, SyncError, SyncResult,
};
use crate::types::Issue;
use watermark::{
    format_import_comment, format_sync_sentinel, latest_sync_sentinel, scan_import_markers,
    sentinel_from_node, LinkState, SyncSentinel,
};

// ─── Public API types (§5.1) ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IngestOptions {
    pub since: Option<DateTime<Utc>>,
    pub label_namespace: String,
    pub auto_label: Option<String>,
    pub dry_run: bool,
    /// Override for the `.spur-ingest.lock` acquisition timeout.
    /// Defaults to 30s per spec §5.2; tests use a shorter value to
    /// keep the concurrent-runs regression case fast.
    pub lock_timeout_ms: Option<u64>,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            since: None,
            label_namespace: "gh".to_string(),
            auto_label: Some("spur-managed".to_string()),
            dry_run: false,
            lock_timeout_ms: None,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IngestReport {
    pub run_id: i64,
    pub source_system: String,
    pub source_repo: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fetched_remote_nodes: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    pub ingested: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub conflicts: Vec<RemoteConflict>,
    pub deletions: Vec<RemoteRef>,
    pub dep_hints_added: usize,
    pub comments_added: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

// ─── Field set + conflict detector (§5.4) ──────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Description,
    Status,
    Priority,
    Assignee,
    Labels,
}

/// Set of mapped fields that can participate in a diff. Bitset
/// representation so `intersect` and `is_empty` are branch-free.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FieldSet(u8);

impl FieldSet {
    const TITLE: u8 = 1 << 0;
    const DESCRIPTION: u8 = 1 << 1;
    const STATUS: u8 = 1 << 2;
    const PRIORITY: u8 = 1 << 3;
    const ASSIGNEE: u8 = 1 << 4;
    const LABELS: u8 = 1 << 5;

    pub const fn empty() -> Self {
        FieldSet(0)
    }

    /// The set of fields a local user can mutate that the diff also
    /// observes. Used as the conservative "local-changed" set in
    /// Phase 1 when `local.updated_at > watermark.last_synced_at`.
    pub const fn user_mutable() -> Self {
        FieldSet(
            Self::TITLE
                | Self::DESCRIPTION
                | Self::STATUS
                | Self::PRIORITY
                | Self::ASSIGNEE
                | Self::LABELS,
        )
    }

    pub fn with(mut self, f: Field) -> Self {
        self.0 |= Self::bit(f);
        self
    }

    pub fn insert(&mut self, f: Field) {
        self.0 |= Self::bit(f);
    }

    pub fn contains(self, f: Field) -> bool {
        self.0 & Self::bit(f) != 0
    }

    pub fn intersection(self, other: Self) -> Self {
        FieldSet(self.0 & other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn as_field_names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.contains(Field::Title) {
            out.push("title");
        }
        if self.contains(Field::Description) {
            out.push("description");
        }
        if self.contains(Field::Status) {
            out.push("status");
        }
        if self.contains(Field::Priority) {
            out.push("priority");
        }
        if self.contains(Field::Assignee) {
            out.push("assignee");
        }
        if self.contains(Field::Labels) {
            out.push("labels");
        }
        out
    }

    const fn bit(f: Field) -> u8 {
        match f {
            Field::Title => Self::TITLE,
            Field::Description => Self::DESCRIPTION,
            Field::Status => Self::STATUS,
            Field::Priority => Self::PRIORITY,
            Field::Assignee => Self::ASSIGNEE,
            Field::Labels => Self::LABELS,
        }
    }
}

/// Stand-in for the full `MappedDiff` that PR-4 (`mapping.rs`) will
/// own. PR-3 ships the minimum viable diff: it knows which mapped
/// fields changed, can render an `IssueUpdate`, and can answer
/// `is_empty()`. PR-4 will replace this with the canonical mapping
/// from §7.4 and a richer label model.
#[derive(Debug, Default, Clone)]
pub struct MappedDiff {
    pub new_title: Option<String>,
    pub new_description: Option<String>,
    pub new_status: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub new_assignee: Option<Option<String>>,
    pub remote_changed: FieldSet,
}

impl MappedDiff {
    pub fn is_empty(&self) -> bool {
        self.new_title.is_none()
            && self.new_description.is_none()
            && self.new_status.is_none()
            && self.add_labels.is_empty()
            && self.remove_labels.is_empty()
            && self.new_assignee.is_none()
    }

    pub fn remote_changed_fields(&self) -> FieldSet {
        self.remote_changed
    }

    pub fn to_issue_update(&self) -> crate::types::IssueUpdate {
        crate::types::IssueUpdate {
            status: self.new_status.clone(),
            body: self.new_description.clone(),
            add_labels: self.add_labels.clone(),
            remove_labels: self.remove_labels.clone(),
            // Some(name) sets, Some("") clears, None leaves unchanged.
            assignee: self
                .new_assignee
                .clone()
                .map(|inner| inner.unwrap_or_default()),
            ..Default::default()
        }
    }
}

/// Build a minimal mapped diff between a local `Issue` and an
/// incoming `RemoteNode`. Captures title/description/status/labels;
/// PR-4's `mapping.rs` replaces this with the full §7.4 mapping.
pub fn compute_mapped_diff(local: &Issue, remote: &RemoteNode) -> MappedDiff {
    let mut diff = MappedDiff::default();

    if local.title != remote.title {
        diff.new_title = Some(remote.title.clone());
        diff.remote_changed.insert(Field::Title);
    }
    if local.body != remote.body {
        diff.new_description = Some(remote.body.clone());
        diff.remote_changed.insert(Field::Description);
    }

    let remote_status = remote_status_to_str(&remote.state);
    let local_status_norm = local.status.to_ascii_lowercase();
    if local_status_norm != remote_status {
        diff.new_status = Some(remote_status.to_string());
        diff.remote_changed.insert(Field::Status);
    }

    // Labels: namespaced as `gh:<name>`; we only own the *user-visible*
    // namespaced set. Two label kinds are auto-applied by ingest itself
    // and must NEVER be diffed out (they aren't on the remote, so a
    // naive subtraction would always want to remove them — that would
    // fire the same-field-conflict detector for every re-ingest of an
    // issue with zero remote labels):
    //  - `gh:issue` / `gh:pr`         — kind marker
    //  - `gh:also-assigned:<login>`   — multi-assignee shim (Phase 2)
    // Plus the global `spur-managed` auto-label which is not in the
    // gh: namespace and is already excluded by `starts_with("gh:")`.
    let local_gh: HashSet<&str> = local
        .labels
        .iter()
        .filter(|l| l.starts_with("gh:"))
        .filter(|l| l.as_str() != "gh:issue" && l.as_str() != "gh:pr")
        .filter(|l| !l.starts_with("gh:also-assigned:"))
        .map(String::as_str)
        .collect();
    let remote_gh: HashSet<String> = remote
        .labels
        .iter()
        .map(|l| format!("gh:{}", sanitize_label(l)))
        .collect();
    let add: Vec<String> = remote_gh
        .iter()
        .filter(|l| !local_gh.contains(l.as_str()))
        .cloned()
        .collect();
    let remove: Vec<String> = local_gh
        .into_iter()
        .filter(|l| !remote_gh.contains(*l))
        .map(|l| l.to_string())
        .collect();
    if !add.is_empty() || !remove.is_empty() {
        diff.add_labels = add;
        diff.remove_labels = remove;
        diff.remote_changed.insert(Field::Labels);
    }

    diff
}

fn remote_status_to_str(state: &RemoteState) -> &'static str {
    match state {
        RemoteState::Open => "open",
        RemoteState::Closed { .. } => "closed",
        RemoteState::Draft => "open",
    }
}

fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Tolerance applied to `local.updated_at > wm.last_synced_at`.
///
/// Beads's `add_comment` (used to write `spur-sync v1` sentinels)
/// internally bumps the host issue's `updated_at` to its own
/// `Utc::now()` AFTER returning. There's no way to make the
/// sentinel's `last_synced_at` field equal that internal value
/// without writing a second sentinel. So the previous run's
/// final `local.updated_at` is always microseconds-to-milliseconds
/// ahead of its `wm.last_synced_at` — purely ingest self-induced.
///
/// A real user edit happens at human latency (seconds at minimum),
/// not microseconds. Filtering by a tolerance window keeps the
/// spec's "any user-mutable field touched since sync" semantics
/// without false-positive every re-ingest.
const SENTINEL_BUMP_TOLERANCE: chrono::Duration = chrono::Duration::milliseconds(100);

/// Field-level three-way conflict (§5.4). Returns `true` iff local
/// and remote both mutated the SAME mapped field since the last
/// successful sync.
pub fn is_field_level_conflict(
    diff: &MappedDiff,
    local: &Issue,
    watermark: &SyncSentinel,
    _remote: &RemoteNode,
) -> bool {
    let remote_changed = diff.remote_changed_fields();
    let threshold = watermark.last_synced_at + SENTINEL_BUMP_TOLERANCE;
    let local_might_have_changed_any_field = local.updated_at > threshold;
    if !local_might_have_changed_any_field {
        return false;
    }
    let local_changed = FieldSet::user_mutable();
    !remote_changed.intersection(local_changed).is_empty()
}

// ─── Process-level lock (§5.2) ─────────────────────────────────────────

const INGEST_LOCK_FILENAME: &str = ".spur-ingest.lock";
const INGEST_LOCK_TIMEOUT_MS: u64 = 30_000;
const INGEST_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Hold-the-lock guard. Drops the flock + truncates the PID file when
/// `apply_remote_delta` returns (success OR error). The lock file
/// itself is left in place — `.beads/` only contains files from the
/// A-9 allowlist; the inode persists between runs.
struct IngestLockGuard {
    _file: File,
}

impl Drop for IngestLockGuard {
    fn drop(&mut self) {
        // try_unlock_exclusive isn't necessary — closing the file
        // descriptor releases the advisory lock. We additionally
        // zero the PID payload so a post-mortem reader doesn't see
        // a stale pid.
        let _ = self._file.set_len(0);
    }
}

fn acquire_ingest_lock(beads_dir: &Path, timeout_ms: u64) -> Result<IngestLockGuard, SyncError> {
    let lock_path = beads_dir.join(INGEST_LOCK_FILENAME);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening ingest lock at {}", lock_path.display()))
        .map_err(SyncError::Other)?;

    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                // Write our pid into the lock file so a contending
                // run can name the holder in its error message.
                let pid = std::process::id();
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let _ = writeln!(file, "{pid}");
                return Ok(IngestLockGuard { _file: file });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => {
                return Err(SyncError::Other(anyhow::anyhow!(
                    "ingest lock acquire failed at {}: {err}",
                    lock_path.display(),
                )));
            }
        }

        if start.elapsed() >= timeout {
            let existing_pid = read_lock_pid(&lock_path).unwrap_or(0);
            return Err(SyncError::Other(anyhow::anyhow!(
                "another ingest run is in progress (pid={existing_pid})"
            )));
        }
        thread::sleep(INGEST_LOCK_POLL_INTERVAL);
    }
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    let mut f = File::open(path).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    buf.trim().parse().ok()
}

// ─── Helpers used by the apply loop ────────────────────────────────────

fn beads_external_ref(source_system: &str, remote_id: &str) -> String {
    format!("{source_system}:{remote_id}")
}

fn sync_label(label: &str, namespace: &str) -> String {
    let body = sanitize_label(label);
    format!("{namespace}:{body}")
}

/// Convert beads-rust `Comment` rows into the `crate::advanced::Comment`
/// shape the watermark helpers expect.
fn comments_from_storage(
    storage: &beads_rust::storage::sqlite::SqliteStorage,
    beads_id: &str,
) -> anyhow::Result<Vec<Comment>> {
    let rows = storage.get_comments(beads_id)?;
    Ok(rows
        .into_iter()
        .map(|c| Comment {
            id: c.id.to_string(),
            body: c.body,
            actor: c.author,
            created_at: c.created_at,
        })
        .collect())
}

fn write_sync_sentinel(
    storage: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    beads_id: &str,
    node: &RemoteNode,
    source_system: &str,
    state: LinkState,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let s = sentinel_from_node(node, source_system, state, now);
    let body = format_sync_sentinel(&s);
    storage.add_comment(beads_id, actor, &body)?;
    Ok(())
}

fn write_disconnect_sentinel(
    storage: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    beads_id: &str,
    remote: &RemoteRef,
    prior: Option<SyncSentinel>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let s = SyncSentinel {
        source_system: remote.source_system.clone(),
        remote_id: remote.remote_id.clone(),
        remote_number: prior.as_ref().and_then(|p| p.remote_number),
        remote_etag: prior.as_ref().and_then(|p| p.remote_etag.clone()),
        remote_updated_at: prior.as_ref().map(|p| p.remote_updated_at).unwrap_or(now),
        last_synced_at: now,
        last_synced_remote_updated_at: prior
            .as_ref()
            .map(|p| p.last_synced_remote_updated_at)
            .unwrap_or(now),
        state: LinkState::Disconnected,
    };
    let body = format_sync_sentinel(&s);
    storage.add_comment(beads_id, actor, &body)?;
    Ok(())
}

fn write_dep_hint_sentinels(
    storage: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    beads_id: &str,
    hints: &[DepHint],
    already_present: &HashSet<String>,
) -> anyhow::Result<usize> {
    let mut added = 0;
    for h in hints {
        let body = dep_hints::format_dep_hint_sentinel(h);
        // Dedup by sentinel body (best-effort — Phase 1 idempotency is
        // body-equality. The brain consumes hints via list_dep_hints,
        // which scans this set; duplicates are tolerated but ugly.)
        if already_present.contains(&body) {
            continue;
        }
        storage.add_comment(beads_id, actor, &body)?;
        added += 1;
    }
    Ok(added)
}

fn scan_dep_hint_bodies(comments: &[Comment]) -> HashSet<String> {
    comments
        .iter()
        .filter(|c| c.body.starts_with(watermark::DEP_HINT_SENTINEL_HEADER))
        .map(|c| c.body.clone())
        .collect()
}

fn import_new_comments(
    storage: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    beads_id: &str,
    node: &RemoteNode,
) -> anyhow::Result<usize> {
    let comments = comments_from_storage(storage, beads_id)?;
    let already_imported = scan_import_markers(&comments);
    let mut added = 0;
    for rc in &node.comments {
        if already_imported.contains(&rc.remote_id) {
            continue;
        }
        let body = format_import_comment(rc, &node.html_url);
        storage.add_comment(beads_id, actor, &body)?;
        added += 1;
    }
    Ok(added)
}

fn make_remote_conflict(
    local: &Issue,
    wm: &SyncSentinel,
    node: &RemoteNode,
    _diff: &MappedDiff,
) -> RemoteConflict {
    RemoteConflict {
        beads_id: local.id.clone(),
        remote_id: node.remote_id.clone(),
        local_updated_at: local.updated_at,
        remote_updated_at: node.updated_at,
        watermark_remote_updated_at: wm.last_synced_remote_updated_at,
        reason: ConflictReason::LocalAndRemoteBothMutated,
    }
}

// ─── Entry point ───────────────────────────────────────────────────────

/// Apply a `RemoteDelta` to the local Beads store under the
/// per-run process lock. See §5.2 of the spec for the per-node
/// state machine.
pub async fn apply_remote_delta(
    beads: &BeadsCrateAdapter,
    sync: &dyn ExternalPmSync,
    delta: RemoteDelta,
    opts: &IngestOptions,
) -> SyncResult<IngestReport> {
    let beads_dir = beads.beads_dir.clone();
    let source_system = sync.source_system();
    let source_repo = sync.source_repo().to_string();

    let lock_timeout_ms = opts.lock_timeout_ms.unwrap_or(INGEST_LOCK_TIMEOUT_MS);
    let _lock = acquire_ingest_lock(&beads_dir, lock_timeout_ms)?;

    let mut report = IngestReport {
        source_system: source_system.to_string(),
        source_repo: source_repo.clone(),
        fetched_remote_nodes: delta.nodes.len(),
        dry_run: opts.dry_run,
        ..Default::default()
    };

    if opts.dry_run {
        report.dep_hints_added = delta.nodes.iter().map(|node| node.dep_hints.len()).sum();
        report.comments_added = delta.nodes.iter().map(|node| node.comments.len()).sum();
        return Ok(report);
    }

    for node in &delta.nodes {
        let outcome = apply_node(beads, source_system, &source_repo, opts, node).await?;
        match outcome {
            NodeOutcome::Ingested {
                dep_hints,
                comments,
            } => {
                report.ingested += 1;
                report.dep_hints_added += dep_hints;
                report.comments_added += comments;
            }
            NodeOutcome::Updated {
                dep_hints,
                comments,
            } => {
                report.updated += 1;
                report.dep_hints_added += dep_hints;
                report.comments_added += comments;
            }
            NodeOutcome::Unchanged { comments } => {
                report.unchanged += 1;
                report.comments_added += comments;
            }
            NodeOutcome::Conflict(c) => {
                report.conflicts.push(c);
            }
        }
    }

    for r in &delta.deletions {
        apply_deletion(beads, source_system, r).await?;
        report.deletions.push(r.clone());
    }

    Ok(report)
}

enum NodeOutcome {
    Ingested { dep_hints: usize, comments: usize },
    Updated { dep_hints: usize, comments: usize },
    Unchanged { comments: usize },
    Conflict(RemoteConflict),
}

async fn apply_node(
    beads: &BeadsCrateAdapter,
    source_system: &'static str,
    source_repo: &str,
    opts: &IngestOptions,
    node: &RemoteNode,
) -> SyncResult<NodeOutcome> {
    let external_ref = beads_external_ref(source_system, &node.remote_id);

    // Cheap preview — read-only, no lock. The cheap-path is advisory;
    // the in-closure re-read inside `adapter.write` below is the
    // source of truth for the decision (§5.2 / spec).
    let preview_existing = beads
        .find_by_external_ref(&external_ref)
        .await
        .map_err(SyncError::Other)?;
    if let Some(local) = preview_existing.as_ref() {
        let comments = beads
            .list_comments(&local.id)
            .await
            .map_err(SyncError::Other)?;
        if let Some(wm) = latest_sync_sentinel(&comments) {
            if wm.last_synced_remote_updated_at == node.updated_at
                && (node.etag.is_none() || wm.remote_etag == node.etag)
                && node
                    .comments
                    .iter()
                    .all(|rc| scan_import_markers(&comments).contains(&rc.remote_id))
            {
                // Truly nothing to do. Cheap-path short-circuit.
                return Ok(NodeOutcome::Unchanged { comments: 0 });
            }
        }
    }

    // Mutating path — all reads re-done inside the write closure so
    // that the watermark seen at decision time is the watermark held
    // under the write lock.
    let node_clone = node.clone();
    let opts_clone = opts.clone();
    let source_repo_owned = source_repo.to_string();
    let external_ref_owned = external_ref.clone();
    let actor = beads.config.actor.clone();
    let source_system_owned: &'static str = source_system;

    let result = beads
        .write(move |s| -> anyhow::Result<NodeOutcomeKind> {
            apply_node_under_lock(
                s,
                &actor,
                source_system_owned,
                &source_repo_owned,
                &external_ref_owned,
                &node_clone,
                &opts_clone,
            )
        })
        .await
        .map_err(SyncError::Other)?;

    Ok(match result {
        NodeOutcomeKind::Ingested {
            dep_hints,
            comments,
        } => NodeOutcome::Ingested {
            dep_hints,
            comments,
        },
        NodeOutcomeKind::Updated {
            dep_hints,
            comments,
        } => NodeOutcome::Updated {
            dep_hints,
            comments,
        },
        NodeOutcomeKind::Unchanged { comments } => NodeOutcome::Unchanged { comments },
        NodeOutcomeKind::Conflict(c) => NodeOutcome::Conflict(c),
    })
}

enum NodeOutcomeKind {
    Ingested { dep_hints: usize, comments: usize },
    Updated { dep_hints: usize, comments: usize },
    Unchanged { comments: usize },
    Conflict(RemoteConflict),
}

fn apply_node_under_lock(
    s: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    source_system: &'static str,
    source_repo: &str,
    external_ref: &str,
    node: &RemoteNode,
    opts: &IngestOptions,
) -> anyhow::Result<NodeOutcomeKind> {
    let now = Utc::now();
    let existing_br = s.find_by_external_ref(external_ref)?;
    let existing: Option<Issue> = match existing_br {
        Some(br) => {
            let mut br = br;
            let id = br.id.clone();
            let mut labels_map = s.get_labels_for_issues(std::slice::from_ref(&id))?;
            br.labels = labels_map.remove(&id).unwrap_or_default();
            Some(crate::beads_crate::issue_tracker::br_to_pm_issue(br))
        }
        None => None,
    };

    let comments = comments_from_storage(s, existing.as_ref().map(|e| e.id.as_str()).unwrap_or(""))
        .unwrap_or_default();
    let watermark = latest_sync_sentinel(&comments);

    // ── NEW ────────────────────────────────────────────────────────
    if existing.is_none() {
        let create = node_to_issue_create(node, source_system, source_repo, opts);
        let beads_id = create_issue_in_place(s, actor, create, &node.title, &node.body)?;

        write_sync_sentinel(
            s,
            actor,
            &beads_id,
            node,
            source_system,
            LinkState::Active,
            now,
        )?;

        let comments_added = import_new_comments(s, actor, &beads_id, node)?;

        let existing_dep_hints = scan_dep_hint_bodies(&comments_from_storage(s, &beads_id)?);
        let dep_hints_added =
            write_dep_hint_sentinels(s, actor, &beads_id, &node.dep_hints, &existing_dep_hints)?;

        return Ok(NodeOutcomeKind::Ingested {
            dep_hints: dep_hints_added,
            comments: comments_added,
        });
    }

    let local = existing.unwrap();

    // ── EXISTING WITH WM ───────────────────────────────────────────
    if let Some(wm) = watermark.as_ref() {
        let diff = compute_mapped_diff(&local, node);

        // If nothing mapped changed AND no new comments to import,
        // still refresh the sentinel so the next cheap-path can
        // short-circuit on (remote_updated_at, etag).
        let imported = scan_import_markers(&comments);
        let any_new_comment = node
            .comments
            .iter()
            .any(|rc| !imported.contains(&rc.remote_id));
        if diff.is_empty() && !any_new_comment {
            // Refresh sentinel only if the remote pointer moved.
            if wm.last_synced_remote_updated_at != node.updated_at || wm.remote_etag != node.etag {
                write_sync_sentinel(s, actor, &local.id, node, source_system, wm.state, now)?;
            }
            return Ok(NodeOutcomeKind::Unchanged { comments: 0 });
        }

        if is_field_level_conflict(&diff, &local, wm, node) {
            return Ok(NodeOutcomeKind::Conflict(make_remote_conflict(
                &local, wm, node, &diff,
            )));
        }

        if !diff.is_empty() {
            apply_issue_update(s, actor, &local.id, &diff)?;
        }
        write_sync_sentinel(
            s,
            actor,
            &local.id,
            node,
            source_system,
            LinkState::Active,
            now,
        )?;
        let comments_added = import_new_comments(s, actor, &local.id, node)?;
        let existing_dep_hints = scan_dep_hint_bodies(&comments_from_storage(s, &local.id)?);
        let dep_hints_added =
            write_dep_hint_sentinels(s, actor, &local.id, &node.dep_hints, &existing_dep_hints)?;

        return Ok(NodeOutcomeKind::Updated {
            dep_hints: dep_hints_added,
            comments: comments_added,
        });
    }

    // ── EXISTING WITHOUT WM (recovery branch §5.2) ────────────────
    let diff = compute_mapped_diff(&local, node);
    let mut updated = false;
    if !diff.is_empty() {
        apply_issue_update(s, actor, &local.id, &diff)?;
        updated = true;
    }
    write_sync_sentinel(
        s,
        actor,
        &local.id,
        node,
        source_system,
        LinkState::Active,
        now,
    )?;
    let comments_added = import_new_comments(s, actor, &local.id, node)?;
    let existing_dep_hints = scan_dep_hint_bodies(&comments_from_storage(s, &local.id)?);
    let dep_hints_added =
        write_dep_hint_sentinels(s, actor, &local.id, &node.dep_hints, &existing_dep_hints)?;

    if updated {
        Ok(NodeOutcomeKind::Updated {
            dep_hints: dep_hints_added,
            comments: comments_added,
        })
    } else {
        Ok(NodeOutcomeKind::Unchanged {
            comments: comments_added,
        })
    }
}

fn node_to_issue_create(
    node: &RemoteNode,
    source_system: &'static str,
    source_repo: &str,
    opts: &IngestOptions,
) -> crate::types::IssueCreate {
    let mut labels = Vec::new();
    if let Some(auto) = opts.auto_label.as_deref() {
        labels.push(auto.to_string());
    }
    match node.kind {
        RemoteKind::Issue => labels.push(format!("{}:issue", opts.label_namespace)),
        RemoteKind::PullRequest => labels.push(format!("{}:pr", opts.label_namespace)),
    }
    for raw in &node.labels {
        labels.push(sync_label(raw, &opts.label_namespace));
    }
    crate::types::IssueCreate {
        title: node.title.clone(),
        description: Some(node.body.clone()),
        labels,
        assignee: node.assignees.first().map(|a| format!("gh:{a}")),
        external_ref: Some(beads_external_ref(source_system, &node.remote_id)),
        source_system: Some(source_system.to_string()),
        source_repo: Some(source_repo.to_string()),
        ..Default::default()
    }
}

fn create_issue_in_place(
    s: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    params: crate::types::IssueCreate,
    title: &str,
    body: &str,
) -> anyhow::Result<String> {
    if let Some(external_ref) = params.external_ref.as_deref() {
        if let Some(existing) = s.find_by_external_ref(external_ref)? {
            return Ok(existing.id);
        }
    }
    let now = Utc::now();
    let id = beads_rust::util::generate_id(title, Some(body), Some("spur"), now);
    let issue = beads_rust::model::Issue {
        id: id.clone(),
        title: title.to_string(),
        description: Some(body.to_string()),
        status: beads_rust::model::Status::Open,
        priority: beads_rust::model::Priority::default(),
        issue_type: beads_rust::model::IssueType::Task,
        created_at: now,
        updated_at: now,
        assignee: params.assignee.clone(),
        owner: None,
        estimated_minutes: None,
        due_at: None,
        defer_until: None,
        external_ref: params.external_ref.clone(),
        ephemeral: false,
        content_hash: None,
        design: None,
        acceptance_criteria: None,
        notes: None,
        created_by: Some(actor.to_string()),
        closed_at: None,
        close_reason: None,
        closed_by_session: None,
        source_system: params.source_system.clone(),
        source_repo: params.source_repo.clone(),
        deleted_at: None,
        deleted_by: None,
        delete_reason: None,
        original_type: None,
        compaction_level: None,
        compacted_at: None,
        compacted_at_commit: None,
        original_size: None,
        sender: None,
        pinned: false,
        is_template: false,
        labels: Vec::new(),
        dependencies: Vec::new(),
        comments: Vec::new(),
    };
    s.create_issue(&issue, actor)?;
    if !params.labels.is_empty() {
        s.set_labels(&id, &params.labels, actor)?;
    }
    Ok(id)
}

fn apply_issue_update(
    s: &mut beads_rust::storage::sqlite::SqliteStorage,
    actor: &str,
    beads_id: &str,
    diff: &MappedDiff,
) -> anyhow::Result<()> {
    let mut br_update = beads_rust::storage::sqlite::IssueUpdate::default();
    if let Some(title) = diff.new_title.as_ref() {
        br_update.title = Some(title.clone());
    }
    if let Some(body) = diff.new_description.as_ref() {
        br_update.description = Some(Some(body.clone()));
    }
    if let Some(status) = diff.new_status.as_ref() {
        if let Ok(parsed) = std::str::FromStr::from_str(status.as_str()) {
            let parsed: beads_rust::model::Status = parsed;
            br_update.status = Some(parsed);
        }
    }
    if let Some(assignee_opt) = diff.new_assignee.as_ref() {
        br_update.assignee = Some(assignee_opt.clone());
    }
    let has_any = br_update.title.is_some()
        || br_update.description.is_some()
        || br_update.status.is_some()
        || br_update.assignee.is_some();
    if has_any {
        s.update_issue(beads_id, &br_update, actor)?;
    }
    for label in &diff.add_labels {
        s.add_label(beads_id, label, actor)?;
    }
    for label in &diff.remove_labels {
        s.remove_label(beads_id, label, actor)?;
    }
    Ok(())
}

async fn apply_deletion(
    beads: &BeadsCrateAdapter,
    source_system: &'static str,
    r: &RemoteRef,
) -> SyncResult<()> {
    let external_ref = beads_external_ref(source_system, &r.remote_id);
    let actor = beads.config.actor.clone();
    let r_clone = r.clone();
    beads
        .write(move |s| -> anyhow::Result<()> {
            let Some(br) = s.find_by_external_ref(&external_ref)? else {
                return Ok(());
            };
            let beads_id = br.id.clone();
            let comments = comments_from_storage(s, &beads_id)?;
            let prior_wm = latest_sync_sentinel(&comments);
            let now = Utc::now();
            write_disconnect_sentinel(s, &actor, &beads_id, &r_clone, prior_wm, now)?;
            let body = format!(
                "spur-audit v1\nkind: disconnected\nremote_id: {}\nsource_system: {}\n",
                r_clone.remote_id, r_clone.source_system,
            );
            s.add_comment(&beads_id, &actor, &body)?;
            Ok(())
        })
        .await
        .map_err(SyncError::Other)
}

// ─── Resolved dep-hint helper (§5.6) — canonical type lives in `advanced` ──

pub use crate::advanced::ResolvedDepHint;

// ─── Public unit tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{RemoteKind, RemoteState};

    fn ts(seconds: i64) -> DateTime<Utc> {
        chrono::TimeZone::timestamp_opt(&Utc, seconds, 0)
            .single()
            .unwrap()
    }

    fn node() -> RemoteNode {
        RemoteNode {
            remote_id: "I_kwDO_1".into(),
            remote_number: Some(1),
            kind: RemoteKind::Issue,
            title: "Original".into(),
            body: "body".into(),
            state: RemoteState::Open,
            labels: vec![],
            assignees: vec![],
            created_at: ts(1),
            updated_at: ts(2),
            html_url: "https://github.com/o/r/issues/1".into(),
            etag: None,
            dep_hints: vec![],
            comments: vec![],
            raw: serde_json::Value::Null,
        }
    }

    fn local() -> Issue {
        Issue {
            id: "bd-1".into(),
            source: crate::types::PmSource::Beads,
            title: "Original".into(),
            body: "body".into(),
            status: "open".into(),
            labels: vec![],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: vec![],
            due_at: None,
            source_system: Some("github".into()),
            source_repo: Some("o/r".into()),
            external_ref: Some("github:I_kwDO_1".into()),
            created_at: ts(1),
            updated_at: ts(2),
        }
    }

    fn watermark() -> SyncSentinel {
        SyncSentinel {
            source_system: "github".into(),
            remote_id: "I_kwDO_1".into(),
            remote_number: Some(1),
            remote_etag: None,
            remote_updated_at: ts(2),
            last_synced_at: ts(2),
            last_synced_remote_updated_at: ts(2),
            state: LinkState::Active,
        }
    }

    #[test]
    fn fieldset_intersect_works() {
        let a = FieldSet::empty().with(Field::Title).with(Field::Status);
        let b = FieldSet::empty().with(Field::Status).with(Field::Labels);
        let c = a.intersection(b);
        assert!(c.contains(Field::Status));
        assert!(!c.contains(Field::Title));
        assert!(!c.contains(Field::Labels));
    }

    #[test]
    fn fieldset_user_mutable_covers_all_fields() {
        let s = FieldSet::user_mutable();
        for f in [
            Field::Title,
            Field::Description,
            Field::Status,
            Field::Priority,
            Field::Assignee,
            Field::Labels,
        ] {
            assert!(s.contains(f));
        }
    }

    #[test]
    fn disjoint_mutations_are_not_a_conflict() {
        // Local bumped priority (so local.updated_at > wm.last_synced_at),
        // remote diff has no overlapping field.
        let mut l = local();
        l.priority = Some(0);
        l.updated_at = ts(10);
        let diff = MappedDiff::default();
        let conflict = is_field_level_conflict(&diff, &l, &watermark(), &node());
        assert!(!conflict, "empty remote_changed must never conflict");
    }

    #[test]
    fn same_field_mutation_is_a_conflict() {
        let mut l = local();
        l.title = "Local renamed".into();
        l.updated_at = ts(10);
        let diff = MappedDiff {
            new_title: Some("Remote renamed".into()),
            remote_changed: FieldSet::empty().with(Field::Title),
            ..Default::default()
        };
        assert!(is_field_level_conflict(&diff, &l, &watermark(), &node()));
    }

    #[test]
    fn remote_only_change_is_not_a_conflict() {
        let l = local(); // local.updated_at == wm.last_synced_at, so untouched
        let diff = MappedDiff {
            new_title: Some("Remote renamed".into()),
            remote_changed: FieldSet::empty().with(Field::Title),
            ..Default::default()
        };
        assert!(!is_field_level_conflict(&diff, &l, &watermark(), &node()));
    }
}
