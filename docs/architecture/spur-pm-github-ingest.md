# spur-pm: GitHub Issue & PR Ingestion — Design Spec (Phase 1)

Status: Approved direction. Implementation-ready.

Builds on:
- [`docs/architecture/spur-pm-beads-source-of-truth.md`](spur-pm-beads-source-of-truth.md) — invariants this spec preserves.
- [`docs/research/github-ingestion-rust.md`](../research/github-ingestion-rust.md) — research and tradeoff analysis.

Scope: punch-list items 1–9 from the research doc. Out of scope (deferred to a Phase 2 spec): ETag incremental polling, webhook receiver, failure-state TUI plumbing, optional upstream `beads_rust` schema PR.

Acceptance: an open-source contributor with `gh auth login` already done can run `spur pm ingest github <owner>/<repo>` on a real upstream repo and see its issues + PRs land in `.beads/beads.db` with full provenance, surviving renames and re-runs, with dependency hints surfaced as structured comments for the brain to act on.

## 0. Invariants (re-stated; the spec must preserve all of these)

I-1. Beads is the only PM store used for dependency-aware orchestration.
I-2. GitHub is an ingest source and sync target, never a peer issue authority.
I-3. The graph engine reads only Beads state through `BeadsCrateAdapter`.
I-4. `PmService::advanced()` remains Beads-gated.
I-5. Remote sync failures must not mutate local truth backwards without explicit conflict handling.
I-6. PR creation remains separate from issue authority.
I-7. New remote PM support must not extend `PmBackendInner` with a peer `IssueTracker` backend.
I-8. Dependency extraction from remote Markdown produces **hints**, not edges. Only the brain mutates the local Beads DAG.

Any reviewer who finds a section of this spec contradicting one of these should treat the contradiction as a bug in the spec, not the invariant.

## 1. Architecture at a glance

```
                                ┌──────────────────────────────┐
spur pm ingest github X/Y ─────►│  CLI (crates/spur/src/cli/)  │
                                └──────────────┬───────────────┘
                                               │
                                               ▼
                                ┌──────────────────────────────┐
                                │  PmService::sync_target()    │  ◄── new accessor
                                └──────────────┬───────────────┘
                                               │ &dyn ExternalPmSync
                                               ▼
                                ┌──────────────────────────────┐
                                │  crates/spur-pm/src/         │
                                │    sync.rs    ←── new trait  │
                                │    ingest/                   │
                                │      mod.rs    apply_delta() │
                                │      watermark.rs            │
                                │      dep_hints.rs            │
                                │      github/                 │
                                │        mod.rs    GitHubSync  │
                                │        client.rs  octocrab   │
                                │        auth.rs   gh + device │
                                │        mapping.rs            │
                                │        graphql/              │
                                │          ingest_repo.graphql │
                                │          types.rs            │
                                └──────────────┬───────────────┘
                                               │
                                               ▼
                                ┌──────────────────────────────┐
                                │   .beads/beads.db            │
                                │   (sole source of truth)     │
                                │                              │
                                │   issues  ←── provenance:    │
                                │              external_ref,   │
                                │              source_system,  │
                                │              source_repo     │
                                │   comments ←── spur-sync v1, │
                                │              spur-dep-hint,  │
                                │              spur-import     │
                                │   dependencies               │
                                └──────────────────────────────┘
                                               ▲
                                               │ via BeadsCrateAdapter
                                               │ + .beads/.write.lock
                                               │
                                ┌──────────────────────────────┐
                                │  GitHub GraphQL/REST         │
                                │  via octocrab                │
                                └──────────────────────────────┘
```

Three rules govern the layout:

R-1. The CLI and `PmService` are the only callers allowed to drive ingest. Brain agents go through `PmService::sync_target()`; they never touch `octocrab` directly.
R-2. All writes go through `BeadsCrateAdapter::write` (the existing `.beads/.write.lock` discipline). The ingest module composes adapter methods; it does not open `SqliteStorage` directly.
R-3. **There is no second store.** All ingest state — provenance, sync watermark, link state, imported-comment idempotency markers — lives in `.beads/beads.db` via primitives `beads_rust` already provides (provenance columns, comments). Preserves invariant I-1 literally, not just in spirit.

## 2. Public types — `crates/spur-pm/src/sync.rs` (new module)

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The contract Spur uses to talk to any external PM. Separate from
/// `IssueTracker` so external systems cannot become peer authorities (I-7).
#[async_trait]
pub trait ExternalPmSync: Send + Sync {
    /// Stable provenance tag — "github", "linear", "plane".
    fn source_system(&self) -> &'static str;

    /// Per-instance scope, e.g. "getspur/spur".
    fn source_repo(&self) -> &str;

    /// Bulk pull. `since=None` means full repo state.
    async fn fetch_changes_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> SyncResult<RemoteDelta>;

    /// Fetch a single remote node by stable id. `if_none_match` is the
    /// REST-only fast path; GraphQL implementations ignore it.
    async fn fetch_one(
        &self,
        remote_id: &str,
        if_none_match: Option<&str>,
    ) -> SyncResult<FetchOneOutcome>;

    /// Project local Beads mutations onto the remote.
    /// `Vec` order is preserved; outcomes align positionally.
    async fn push_mutations(
        &self,
        diff: Vec<LocalMutation>,
    ) -> SyncResult<Vec<PushOutcome>>;

    /// Compare local watermarks against the remote (cheap; uses ETag/
    /// updated_at). Used by the apply step before any push.
    async fn detect_conflicts(
        &self,
        watermarks: &[SyncWatermark],
    ) -> SyncResult<Vec<RemoteConflict>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDelta {
    pub nodes: Vec<RemoteNode>,
    /// Remote IDs known to be deleted/inaccessible.
    pub deletions: Vec<RemoteRef>,
    /// Server-time cursor for the next `fetch_changes_since` call.
    pub watermark: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteNode {
    pub remote_id: String,           // e.g. GitHub node_id: "I_kwDO..."
    pub remote_number: Option<u64>,  // e.g. issue #42 (display only)
    pub kind: RemoteKind,
    pub title: String,
    pub body: String,
    pub state: RemoteState,
    pub labels: Vec<String>,         // raw remote names; mapping happens later
    pub assignees: Vec<String>,      // remote logins
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub html_url: String,
    pub etag: Option<String>,        // REST poll path only
    pub dep_hints: Vec<DepHint>,
    pub comments: Vec<RemoteComment>,
    /// Anything we didn't map; preserved for forward-compat.
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RemoteKind { Issue, PullRequest }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RemoteState {
    Open,
    Closed { reason: Option<String> },
    Draft,                               // PRs only
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRef {
    pub source_system: String,
    pub remote_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteComment {
    pub remote_id: String,               // GitHub comment node_id
    pub author: String,                  // login
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepHint {
    pub kind: DepHintKind,
    pub remote_keyword: String,          // verbatim, e.g. "Closes"
    pub remote_ref: String,              // "owner/repo#42" or "#42"
    pub resolved_beads_id: Option<String>,
    pub raw_span: String,                // exact source text
    pub source: DepHintSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DepHintKind {
    Closes, Fixes, Resolves,
    DependsOn, Blocks, BlockedBy,
    TaskList,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DepHintSource { Body, TimelineItem }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncWatermark {
    pub beads_id: String,
    pub remote_id: String,
    pub last_synced_at: DateTime<Utc>,
    pub last_synced_etag: Option<String>,
    pub last_synced_remote_updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FetchOneOutcome {
    Unchanged,                           // 304 / etag match
    Updated(RemoteNode),
    Gone,                                // 404 / repo private / transferred
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalMutation {
    pub beads_id: String,
    pub remote_id: String,
    pub kind: LocalMutationKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LocalMutationKind {
    StatusChange { from: String, to: String },
    LabelsAdded(Vec<String>),
    LabelsRemoved(Vec<String>),
    CommentAdded { body: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PushOutcome {
    Pushed { new_etag: Option<String>, new_remote_updated_at: DateTime<Utc> },
    Conflict(RemoteConflict),
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConflict {
    pub beads_id: String,
    pub remote_id: String,
    pub local_updated_at: DateTime<Utc>,
    pub remote_updated_at: DateTime<Utc>,
    pub watermark_remote_updated_at: DateTime<Utc>,
    pub reason: ConflictReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ConflictReason {
    RemoteMovedSinceLastSync,
    LocalAndRemoteBothMutated,
}

pub type SyncResult<T> = std::result::Result<T, SyncError>;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("authentication required: {0}")]
    NeedsAuth(String),
    #[error("remote not found: {0}")]
    Gone(String),
    #[error("rate limited; retry after {retry_after_s}s")]
    RateLimited { retry_after_s: u64 },
    #[error("transient network error: {0}")]
    Transient(String),
    #[error("malformed remote response: {0}")]
    Malformed(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

`SyncError` is the *only* error type that crosses the `ExternalPmSync` boundary; each variant maps to a documented retry / surfacing strategy in §6.

## 3. Public types — extension of `spur-pm` PM types

The existing `Issue`, `IssueCreate`, and `IssueUpdate` (`crates/spur-pm/src/types.rs`) currently hide the provenance fields that `beads_rust` already supports. We surface them as optional fields — old callers compile unchanged.

```rust
pub struct Issue {
    // ... existing fields ...

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
}

pub struct IssueCreate {
    // ... existing fields ...

    /// Provenance. When `external_ref` is `Some`, `create_issue` is
    /// idempotent: if a row already exists with this `external_ref` it
    /// returns the existing id rather than creating a duplicate (relies
    /// on `beads_rust`'s UNIQUE partial index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
}

pub struct IssueUpdate {
    // ... existing fields ...

    /// Set, clear, or leave the external_ref alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<Option<String>>,
}
```

**Wiring touchpoints (the five hardcoded `None` sites become passthroughs):**

- `crates/spur-pm/src/beads_crate/issue_tracker.rs:325` — `create_issue` populates `beads_rust::model::Issue.external_ref / source_system / source_repo` from `IssueCreate`.
- `crates/spur-pm/src/beads_crate/issue_tracker.rs:465` — `update_issue` threads `update.external_ref` into `beads_rust::storage::sqlite::IssueUpdate.external_ref`.
- `crates/spur-pm/src/beads_crate/issue_tracker.rs` `br_to_pm_issue` — copy the three provenance fields off the `beads_rust::model::Issue` onto the returned `Issue`.
- `crates/spur-pm/src/beads_crate/adapter.rs:570` — snapshot conversion mirrors the same three fields.
- `crates/spur-pm/src/graph_engine/snapshot.rs:397` — same.
- `crates/spur-pm/src/test_workspace.rs:116` — default to `None`; tests opt in.

**Idempotency contract on `create_issue`:** when `external_ref` is `Some`, the adapter does a `find_by_external_ref` first and returns the existing id if found (status quo today: a UNIQUE constraint violation surfaces as an error). This is the *primary* idempotency guarantee for ingest re-runs.

## 4. State storage — `.beads/beads.db` only

**There is no sidecar database.** Every fact ingest cares about — provenance, sync watermark, link state, imported-comment idempotency — is stored in `.beads/beads.db` using primitives that `beads_rust` already provides:

- **Provenance** uses the existing `external_ref` (UNIQUE partial index), `source_system`, and `source_repo` columns on `issues`.
- **Watermark + link state** live in a per-issue `spur-sync v1` sentinel comment, written via `BeadsAdvanced::add_comment`.
- **Imported-comment idempotency** uses an embedded `<!-- spur-import gh:<node_id> -->` marker on the first line of each imported comment.
- **Sync run history** is out of scope for Phase 1 — `IngestReport` is the only artifact, returned to the CLI.

This preserves invariant I-1 literally. One file, one write lock, one set of backups, one consistent crash boundary. The cost paid: an extra `list_comments` per issue during ingest (~sub-ms each in WAL mode). The cost not paid: a two-store consistency problem.

### 4.1 Per-issue sync watermark — `spur-sync v1` sentinel comment

Body format (one comment per ingested issue per ingest run; append-only):

```
spur-sync v1
source_system:                  github
remote_id:                      I_kwDOExample123
remote_number:                  42
remote_etag:                    W/"abc123def456"
remote_updated_at:              2026-05-10T12:00:00Z
last_synced_at:                 2026-05-12T03:00:00Z
last_synced_remote_updated_at:  2026-05-10T12:00:00Z
state:                          active
```

Reads — to load the current watermark for issue `bd-XYZ`:

```rust
let comments = adv.list_comments("bd-XYZ").await?;
let latest = comments.iter().rev()
    .find(|c| c.body.starts_with("spur-sync v1\n"))
    .map(parse_sync_comment);
```

Newest by `created_at` wins. If none exists, the issue has never been synced — treat as first-sight.

Writes — on every successful ingest/update of the issue, **append** a new sentinel via `BeadsAdvanced::add_comment`. Append-only by design: prior watermarks form a per-issue audit trail of when each remote version was observed. Phase 2 may add a compaction pass that prunes all but the newest `spur-sync v1` comment per issue.

### 4.2 Per-imported-comment idempotency — embedded marker

Each remote comment is imported into Beads as:

```
<!-- spur-import gh:IC_kwDOComment123 -->
imported from https://github.com/owner/repo/issues/42#issuecomment-123 by gh:alice (2026-05-10T12:00:00Z):

<remote comment body verbatim>
```

The first line is an HTML comment so it renders unobtrusively in any Markdown viewer (including `bd show`). The `<!-- spur-import gh:<id> -->` marker is the dedup key.

Idempotency check before importing a remote comment:

```rust
let comments = adv.list_comments(beads_id).await?;
let already_imported: HashSet<&str> = comments.iter()
    .filter_map(|c| c.body.lines().next()
        .and_then(parse_import_marker))   // returns Some("IC_kwDO...") on match
    .collect();
if !already_imported.contains(remote_comment_node_id) {
    adv.add_comment(beads_id, &format_import(remote_comment)).await?;
}
```

For an issue with N existing comments and M new remote comments, this is one `list_comments` call plus O(N) parse. Acceptable for Phase 1. Phase 2 can add `BeadsAdvanced::find_comment_by_external_marker` for a SQL `LIKE` shortcut.

### 4.3 `link.state` lifecycle

Encoded as the `state:` field of the latest `spur-sync v1` comment. Same state machine, no separate storage:

```
        ingest first sight
            │
            ▼
       ┌─────────┐  401  ┌────────────┐
       │ active  │◄──────│ needs_auth │
       └──┬──────┘       └────────────┘
          │ 404 / repo private
          ▼
   ┌──────────────┐
   │ disconnected │
   └──────────────┘
```

- `active`: ingest is working; last sync succeeded.
- `needs_auth`: a 401 was observed against this remote node.
- `disconnected`: a 404 / private-repo error was observed.

Every state transition emits a new `spur-sync v1` comment with the updated `state:` field. `BeadsAdvanced::list_comments` filtered for the sentinel is the source-of-truth query. Phase 1 wires `active` and `disconnected` transitions; `needs_auth` lands with the Phase 2 failure-state spec — no schema change required, only new code that writes the new state value.

### 4.4 Identity Registry

The doc's "Identity Registry mapping `beads_id <-> (source_system, remote_id, remote_version_hash)`" maps to:

| Doc field | Implementation |
|---|---|
| `beads_id` | `issues.id` |
| `source_system` | `issues.source_system` (existing column) |
| `remote_id` | encoded in `issues.external_ref` as `"github:<node_id>"` (UNIQUE indexed) |
| `remote_version_hash` | `remote_etag` field of the latest `spur-sync v1` comment |
| watermark | `last_synced_remote_updated_at` field of the latest `spur-sync v1` comment |

Lookup by remote id: `find_by_external_ref("github:<node_id>")` — already exposed at the `beads_rust` storage layer; Phase 1 surfaces it through `spur-pm` (see PR-1).

Lookup by local id → current watermark: `list_comments(beads_id)` + filter.

### 4.5 Future upstream optimization (Phase 2 or later, not Phase 1)

Once the design is proven in production, an optional upstream `beads_rust` PR can add indexed columns for hot watermark fields:

```sql
ALTER TABLE issues ADD COLUMN remote_etag TEXT;
ALTER TABLE issues ADD COLUMN last_synced_at DATETIME;
ALTER TABLE issues ADD COLUMN last_synced_remote_updated_at DATETIME;
ALTER TABLE issues ADD COLUMN link_state TEXT;
```

Migration is one-time and fully reversible: scan all `spur-sync v1` comments, project the latest per issue into the new columns, then stop emitting new sentinels (existing ones remain as audit history). Reads switch from comment-scan to column-read. **Until then, the sentinel approach is correct and complete — it is not a workaround, just a slower read path that needs no upstream coordination.**

## 5. Ingest flow — `crates/spur-pm/src/ingest/mod.rs`

### 5.1 Entry point

```rust
pub struct IngestOptions {
    pub since: Option<DateTime<Utc>>,
    pub label_namespace: String,      // default "gh"
    pub auto_label: Option<String>,   // default Some("spur-managed"), like GitHubAdapter today
    pub dry_run: bool,
}

pub struct IngestReport {
    pub run_id: i64,
    pub source_system: &'static str,
    pub source_repo: String,
    pub ingested: usize,    // newly created
    pub updated: usize,     // existed, mutated
    pub unchanged: usize,   // existed, no diff
    pub conflicts: Vec<RemoteConflict>,
    pub deletions: Vec<RemoteRef>,
    pub dep_hints_added: usize,
    pub comments_added: usize,
}

pub async fn apply_remote_delta(
    pm: &PmService,
    sync: &dyn ExternalPmSync,
    delta: RemoteDelta,
    opts: &IngestOptions,
) -> SyncResult<IngestReport>;
```

`apply_remote_delta` is the only mutation entry point. The CLI calls `sync.fetch_changes_since(opts.since).await?` then this function.

### 5.2 Apply sequence (per node)

```
For each node in delta.nodes:
  existing = pm.find_by_external_ref("github:" + node.remote_id).await?
  watermark = if existing { read_latest_sync_comment(existing.id)? } else { None }

  NEW BRANCH (existing is None):
    a. create = mapping::to_issue_create(node, opts)
         external_ref  = "github:<remote_id>"
         source_system = "github"
         source_repo   = "<owner>/<repo>"
    b. beads_id = pm.create_issue(create).await?   (idempotent on external_ref)
    c. write_sync_sentinel(beads_id, node, state=Active, now=Utc::now())
    d. apply_dep_hints(beads_id, node.dep_hints)
    e. import_comments(beads_id, node.comments)
    f. report.ingested += 1

  EXISTING BRANCH (existing is Some, watermark is Some):
    a. Cheap path: if watermark.last_synced_remote_updated_at == node.updated_at
         AND (node.etag.is_none() || watermark.remote_etag == node.etag):
           report.unchanged += 1; continue
    b. diff = mapping::diff_against_local(existing, node)
    c. if diff.is_empty():
         # remote updated_at moved but mapped fields didn't (e.g. someone reacted).
         # Still refresh the sentinel so the next sync can short-circuit.
         write_sync_sentinel(existing.id, node, state=watermark.state, now=Utc::now())
         report.unchanged += 1; continue
    d. # Conflict detection — see §5.4.
       if is_three_way_conflict(existing, watermark, node):
           report.conflicts.push(make_conflict(existing, watermark, node))
           continue
    e. pm.update_issue(existing.id, diff.to_issue_update()).await?
    f. write_sync_sentinel(existing.id, node, state=Active, now=Utc::now())
    g. apply_new_dep_hints(existing.id, node.dep_hints)
    h. import_new_comments(existing.id, node.comments)
    i. report.updated += 1

  EXISTING WITHOUT WATERMARK (recovery branch):
    # Issue created by ingest in a prior version, or by a manual `bd` op
    # with the same external_ref. Treat the current remote view as the
    # baseline — no conflict because we have no prior reference point.
    a. diff = mapping::diff_against_local(existing, node)
    b. if !diff.is_empty(): pm.update_issue(...)
    c. write_sync_sentinel(existing.id, node, state=Active, now=Utc::now())
    d. import_new_comments(existing.id, node.comments)
    e. report.updated += 1   (or unchanged += 1 if diff empty)

For each ref in delta.deletions:
  existing = pm.find_by_external_ref("github:" + ref.remote_id).await?
  if existing.is_some():
    write_sync_sentinel(existing.id, /* etag */ None, state=Disconnected, now)
    adv.add_comment(existing.id, "spur-audit v1\nkind:disconnected\n...")
    report.deletions.push(ref)
```

`write_sync_sentinel` is a thin helper over `BeadsAdvanced::add_comment` that formats the `spur-sync v1` body per §4.1.

### 5.3 Comment ingestion

Per-comment idempotency uses the embedded marker from §4.2 — no second store needed. The flow for each `RemoteComment` on an issue:

```
existing_markers = scan_import_markers(adv.list_comments(beads_id).await?)
for rc in node.comments:
    if existing_markers.contains(rc.remote_id): continue
    adv.add_comment(beads_id, format_import(rc)).await?
```

`format_import` produces the body documented in §4.2, with the `<!-- spur-import gh:<id> -->` first line. `scan_import_markers` is a 10-line helper over `Vec<Comment>` that returns `HashSet<String>` of GitHub comment node_ids.

For an issue with N existing Beads comments and M new remote comments, this is one `list_comments` call plus M inserts. Bounded scan; acceptable for Phase 1.

### 5.4 Conflict detection — three-way merge

Implemented in `ingest::watermark::is_three_way_conflict`. Conflict iff **both** sides moved since the last successful sync:

```rust
fn is_three_way_conflict(
    local: &Issue,
    watermark: &SyncWatermark,
    remote: &RemoteNode,
) -> bool {
    let remote_moved = remote.updated_at > watermark.last_synced_remote_updated_at
        || remote.etag.as_deref() != watermark.last_synced_etag.as_deref();
    let local_moved = local.updated_at > watermark.last_synced_at;
    remote_moved && local_moved
}
```

When both moved, return `RemoteConflict { ... }` from §2 and **do not write**. Phase 1 surfaces conflicts in `IngestReport.conflicts` and prints a counted line; conflict resolution UX (TUI / brain decision flow) lands in Phase 2. The Beads issue and the local watermark are preserved as-is.

When only the remote moved, the cheap-path check in §5.2 step (a) already short-circuited, OR the diff is non-empty and we update. When only the local moved, the diff is non-empty and we update — the remote is the authoritative view of the upstream side.

### 5.5 Locking & transaction discipline

One lock: `.beads/.write.lock`, held by `BeadsCrateAdapter::write`. The apply step composes adapter methods; per-node it runs:

```
adapter.write(|s| {
    create_or_update_issue
    add_or_skip_imported_comments
    add_dep_hint_sentinels
    add_sync_sentinel
})
```

Every per-node update is one BEGIN/COMMIT in `beads_rust`. If the process crashes mid-batch, partial nodes are persisted but each persisted node is internally consistent. Re-running ingest is safe because:

1. `find_by_external_ref` returns the partially-applied issue.
2. The latest `spur-sync v1` sentinel records the remote state we'd persisted.
3. Re-ingest of an unchanged remote short-circuits via §5.2 (a).

**No two-store consistency problem exists.** This is the architectural reason for the §4 redesign.

### 5.6 Dependency hints

`dep_hints::extract(body, timeline) -> Vec<DepHint>` runs after the node is materialized in Beads. Hints are persisted as structured comments with sentinel `spur-dep-hint v1`:

```
spur-dep-hint v1
kind:        closes
remote_keyword: Closes
remote_ref:  #42
resolved_beads_id: bd-XYZ           # or "unresolved"
raw_span:    Closes #42
source:      body
remote_node: <node_id of the issue/PR containing the hint>
```

`resolved_beads_id` resolution at write time:

- **Full node-id refs** (e.g., the GraphQL timeline already gives us `I_kwDO…`): direct lookup via `find_by_external_ref("github:" + node_id)`.
- **Numeric refs** (e.g., `Closes #42` in body text): two-step lookup. First check for a recently-ingested node in the current batch with `remote_number = 42` AND `source_repo = <current>`. If miss, fall back to a `list_issues` filter scanning `external_ref LIKE "github:%"` within this `source_repo`. If still unresolved, write `unresolved`.
- **Cross-repo refs** (e.g., `owner2/repo2#7`): same as numeric, scoped to that `source_repo` instead. May resolve to nothing if we've never ingested that repo.

A final re-resolution pass at the end of `apply_remote_delta` re-walks `unresolved` hints once all nodes are in place — this catches the case where issue A's hint points to issue B that was ingested later in the same batch.

Hints **never** mutate the local DAG. They are queryable via `BeadsAdvanced::list_comments` filtered to the `spur-dep-hint v1` sentinel. The brain decides whether to call `IssueTracker::add_dependency`.

## 6. Failure model and `SyncError` mapping

| `SyncError` | Source observable | Apply-step behavior | Surfaced to user |
|---|---|---|---|
| `NeedsAuth` | 401 | Abort run cleanly; mark in-flight links → no state change; set link.state=`needs_auth` only after the Phase 2 spec lands | "Authentication failed. Run `gh auth login` or `spur pm reauth github`." (exit 1) |
| `Gone(remote_id)` | 404 on a previously-known node | Mark link.state=`disconnected`; append one `spur-audit kind:disconnected` comment; Beads issue untouched | Counted in `IngestReport.deletions`. |
| `RateLimited{retry_after_s}` | 403 with `Retry-After` or `x-ratelimit-remaining=0` | Sleep `min(retry_after_s, 600)`; retry once; on second hit, abort with `RateLimited` propagated to CLI | "GitHub rate limit hit; retrying in N seconds." (single live update line) |
| `Transient` | Network timeout, 5xx | Three-attempt exponential backoff (250ms, 1s, 5s); on third failure, propagate | "Network error; partial progress saved. Re-run to resume." (exit 2) |
| `Malformed` | JSON parse failure / schema mismatch | Abort run; do not mutate further; log the response excerpt at WARN | "GitHub returned an unexpected response. Please file a bug." (exit 3) |
| `Other` | anything else | Abort, propagate | Default message + cause chain. |

Crucially: every variant except `Malformed` leaves the system in a **resumable** state. Re-running `spur pm ingest github X/Y` is always safe.

## 7. GitHub adapter — `crates/spur-pm/src/ingest/github/`

### 7.1 `auth.rs`

```rust
pub async fn resolve_token() -> Result<GitHubToken, AuthError>;

pub struct GitHubToken {
    pub token: String,
    pub source: TokenSource,
}

pub enum TokenSource {
    GhCli,        // shelled out to `gh auth token`
    DeviceFlow,   // ran OAuth Device Flow
    EnvVar,       // SPUR_GITHUB_TOKEN (escape hatch for CI)
}
```

Resolution order:

1. `SPUR_GITHUB_TOKEN` env var — for CI and power users. Skipped if empty.
2. `gh auth token` — `Command::new("gh").args(["auth", "token"]).output()`. Trim stdout; ignore stderr. If the binary is missing or the call returns non-zero, fall through.
3. OAuth Device Flow — prints the user code + URL, polls the token endpoint. Stores the resulting token in the OS keyring (via the `keyring` crate, behind a config flag — Phase 2 wires the persistence; Phase 1 may hold the token in process only and ask again next run).

No PAT prompt. If all three fail, return `AuthError::NoTokenSource` and the CLI prints a one-paragraph remediation.

### 7.2 `client.rs`

Wraps `octocrab::Octocrab`. Two responsibilities:

R-7.2.1 Rate-limit governor. Every response runs through `Governor::observe(headers)`, which extracts `x-ratelimit-remaining`, `x-ratelimit-reset`, and (for GraphQL) the `rateLimit { remaining resetAt cost }` block from the JSON. When `remaining` drops below a configurable floor (default 50 for REST, 100 points for GraphQL), `Governor::throttle().await` sleeps until reset. Implementation uses `tokio::time::sleep_until` with a `Notify` so concurrent requests share the same wake.

R-7.2.2 GraphQL execution. Wraps `octocrab.graphql::<T>(query)` so the client returns `Result<T, SyncError>` instead of `octocrab::Error`. Concrete error mapping table:

| `octocrab::Error` shape | `SyncError` |
|---|---|
| `GitHubError { status: 401, .. }` | `NeedsAuth` |
| `GitHubError { status: 403, headers: rate-limit-remaining=0 }` | `RateLimited { retry_after_s }` |
| `GitHubError { status: 403, headers: retry-after=N }` | `RateLimited { retry_after_s: N }` |
| `GitHubError { status: 404, .. }` | `Gone(remote_id)` |
| `Hyper(_)` / connection reset | `Transient(...)` |
| `Serde(_)` / `Json(_)` | `Malformed(...)` |
| anything else | `Other(...)` |

> **Crate-API note:** Verify the exact variant names against the current `octocrab` release at implementation time — they've moved between versions. The mapping *intent* is fixed even if the syntax shifts.

### 7.3 GraphQL ingest query

`ingest/github/graphql/ingest_repo.graphql`:

```graphql
query IngestRepo($owner: String!, $repo: String!, $cursor: String, $pageSize: Int!) {
  repository(owner: $owner, name: $repo) {
    issues(first: $pageSize, after: $cursor,
           orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title body url state stateReason
        createdAt updatedAt
        author { login }
        assignees(first: 10) { nodes { login } }
        labels(first: 50)    { nodes { name } }
        comments(first: 100) {
          pageInfo { hasNextPage endCursor }
          nodes { id author { login } body createdAt updatedAt }
        }
        timelineItems(first: 50,
            itemTypes: [CROSS_REFERENCED_EVENT, CLOSED_EVENT]) {
          nodes {
            __typename
            ... on CrossReferencedEvent {
              source { __typename
                ... on Issue       { id number repository { nameWithOwner } }
                ... on PullRequest { id number repository { nameWithOwner } }
              }
            }
            ... on ClosedEvent { stateReason }
          }
        }
      }
    }
    pullRequests(first: $pageSize, after: $cursor,
                 orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title body url state isDraft
        createdAt updatedAt
        author { login }
        assignees(first: 10) { nodes { login } }
        labels(first: 50)    { nodes { name } }
        comments(first: 100) {
          pageInfo { hasNextPage endCursor }
          nodes { id author { login } body createdAt updatedAt }
        }
        closingIssuesReferences(first: 10) {
          nodes { id number repository { nameWithOwner } }
        }
      }
    }
    rateLimit { cost remaining resetAt nodeCount }
  }
}
```

Pagination strategy:

P-1. Issues and PRs paginate independently. The client runs them as two cursored loops.
P-2. `pageSize` starts at 50. If `rateLimit.cost` for a page exceeds `floor` (configurable, default 5% of remaining), halve and retry on the next iteration. If a single page costs >2% of the per-minute cap, halve permanently for this run.
P-3. Inner connections (`comments(first: 100)`, `timelineItems(first: 50)`) on an outer node that reports `hasNextPage=true` trigger a follow-up `IngestNodeExtras($id)` query (defined alongside `ingest_repo.graphql`) to drain the remaining pages. Phase 1: accept truncation at 100 comments per node and emit a warning; the follow-up query lands when someone has a real repo that exceeds the limit.

`graphql_client::GraphQLQuery` generates the Rust types into `ingest/github/graphql/types.rs`.

### 7.4 `mapping.rs`

Pure functions, no I/O. `to_issue_create(node, opts) -> IssueCreate`, `to_remote_node(graphql_response_node) -> RemoteNode`, `diff_against_local(local: &Issue, remote: &RemoteNode) -> IssueUpdate`.

The mapping table (full canonical version, supersedes §3.3 of the research):

```
GitHub                          → Beads
─────────────────────────────────────────────────────────────────
node.id                         → external_ref = "github:" + node.id
"github"                        → source_system
"<owner>/<repo>"                → source_repo
node.title                      → title
node.body                       → description
node.url                        → url

# state
issue.state = OPEN              → Status::Open
issue.state = CLOSED,
  stateReason = COMPLETED       → Status::Closed
  stateReason = NOT_PLANNED     → Status::Closed + label "gh:not-planned"
  stateReason = REOPENED        → Status::Open
pr.state    = OPEN, isDraft=t   → Status::Draft
pr.state    = OPEN, isDraft=f   → Status::Open
pr.state    = MERGED            → Status::Closed + label "gh:merged"
pr.state    = CLOSED            → Status::Closed

# people
node.assignees[0].login         → assignee = "gh:" + login
node.assignees[1..].login       → labels: "gh:also-assigned:<login>"   (Phase 1: log + drop; revisit in Phase 2)

# labels
node.labels[].name              → labels: "gh:" + name
                                  AND infer IssueType (see below)
                                  AND infer Priority (see below)

# heuristics
labels contains "bug"           → IssueType::Bug
        "enhancement"|"feature" → IssueType::Feature
        "documentation"|"docs"  → IssueType::Docs
        "question"              → IssueType::Question
        "chore"                 → IssueType::Chore
        (PR with none above)    → IssueType::Feature
        (Issue with none above) → IssueType::Task

labels matching /^p[0-4]$|^priority[-/:]p?[0-4]/i  → Priority(0..4)
                                                     (label-driven; default Backlog)

# kind tag
PR                              → label "gh:pull-request"
Issue                           → label "gh:issue"

# timing
node.createdAt, node.updatedAt  → created_at, updated_at
```

Label namespace `gh:` is configurable via `IngestOptions.label_namespace`. The default makes collisions with Spur's own labels structurally impossible.

`diff_against_local` only emits an `IssueUpdate` for fields that actually changed — the goal is to keep beads_rust's audit history clean.

### 7.5 `dep_hints.rs`

Pure functions. Two extractors compose into the final `Vec<DepHint>`:

E-1. Body extractor: regex-driven over `RemoteNode.body`.

```
PATTERN_CLOSING:
  (?i)\b(?P<kw>close[sd]?|fix(?:es|ed)?|resolve[sd]?)\s+
  (?P<ref>(?:[\w.-]+/[\w.-]+)?#\d+)
  → DepHint { kind: closes|fixes|resolves, remote_ref: <ref>, source: Body }

PATTERN_DEPENDS:
  (?im)^\s*(?P<kw>depends\s+on|blocked\s+by|blocks)\s*:?\s+
  (?P<ref>(?:[\w.-]+/[\w.-]+)?#\d+)
  → DepHint { kind: depends-on|blocked-by|blocks, remote_ref: <ref>, source: Body }

PATTERN_TASKLIST:
  (?m)^\s*-\s*\[\s*[ x]\s*\]\s*(?P<ref>(?:[\w.-]+/[\w.-]+)?#\d+)
  → DepHint { kind: task-list, remote_keyword: "- [ ]", remote_ref: <ref>, source: Body }
```

E-2. Timeline extractor: walks `timelineItems.nodes`:

- `ClosedEvent.stateReason = COMPLETED` with `closer` pointing to a PR → DepHint { kind: closes, source: TimelineItem, remote_ref: "<pr.repo>#<pr.number>" } **on the issue side**.
- `CrossReferencedEvent` from a PR → DepHint { kind: closes (if the PR has a matching `closingIssuesReferences`) else depends-on, source: TimelineItem }.

Timeline extraction is preferred over body extraction when both produce a hint for the same `(remote_id, kind)` pair — it's GitHub's already-resolved view. Deduplicate at the end of extraction by `(remote_ref, kind)`.

Resolution of `remote_ref → resolved_beads_id` happens at apply time (§5.6), not in this module.

## 8. CLI surface — `crates/spur/src/cli/`

New subcommand:

```
spur pm ingest github <owner>/<repo>
    [--since <iso8601>]
    [--label-namespace <prefix>]      # default "gh"
    [--page-size <N>]                 # default 50
    [--dry-run]
    [--json]                          # machine-readable report

spur pm ingest github --help          # standard help
```

Behavior:

- `--dry-run`: runs `fetch_changes_since` and the mapping step, prints the report, *does not* call `apply_remote_delta`.
- `--json`: prints `IngestReport` as JSON to stdout; exits 0 even if conflicts were detected (caller is responsible for inspecting the JSON).
- Default (human) output: a progress line per page, a final summary block, and a non-zero exit code if there were any conflicts or errors.

Wiring: the CLI calls `PmService::sync_target("github")` which (Phase 1) constructs a `GitHubSync` lazily from the existing `BeadsCrateAdapter`'s cwd. The `PmBackendInner` enum is **not** extended; `sync_target` is a separate accessor on `PmService` that returns `Option<Arc<dyn ExternalPmSync>>` and is gated on Beads being the active backend (preserves I-1, I-7).

```rust
impl PmService {
    pub fn sync_target(&self, source_system: &str) -> Option<Arc<dyn ExternalPmSync>> {
        match (&self.backend, source_system) {
            (PmBackendInner::Beads { .. }, "github") => Some(self.github_sync.clone()?),
            _ => None,
        }
    }
}
```

`self.github_sync` is `Option<Arc<GitHubSync>>` populated by `PmService::try_new_with_actor` when GitHub config is present (token resolvable, repo detected). When `None`, the CLI prints the auth/repo-detection error from §7.1 and exits.

## 9. Configuration

Lives in the existing Spur config file (no new file required). Schema:

```toml
[pm.github]
# Optional explicit repo when running `spur pm ingest github` without an arg.
default_repo = "getspur/spur"

# Token resolution preference; overrides the default order from §7.1.
auth_preference = ["env", "gh_cli", "device_flow"]

# Rate-limit floors.
rate_limit_floor_rest = 50
rate_limit_floor_graphql_points = 100

# Default ingest options.
[pm.github.ingest]
label_namespace = "gh"
page_size = 50
auto_label = "spur-managed"

# Persisted token cache (Phase 2 wires this; Phase 1 may set it but ignores).
[pm.github.cache]
keyring_service = "spur.github"
```

All keys are optional; defaults match the constants in this spec.

## 10. Test plan

T-1 **Unit — mapping (`mapping_test.rs`):** fixture JSON for 12 representative issues and 6 PRs (open, closed-completed, closed-not_planned, draft, merged, reopened, multi-assignee, with all four label types, with priority labels, with no body). For each, assert that `to_issue_create` produces the expected `IssueCreate` and `to_remote_node` round-trips through `RemoteNode` losslessly except for declared drops.

T-2 **Unit — dep_hints (`dep_hints_test.rs`):** fixture bodies covering each pattern from §7.5. Add fuzz tests that confirm:
   - No panic on arbitrary UTF-8 bodies (proptest with arbitrary strings, 1000 iterations).
   - No extraction from inside fenced code blocks (\`\`\` … \`\`\`) — Phase 1 may accept this limitation; document it.
   - Deduplication when body and timeline emit the same hint.

T-3 **Unit — watermark (`watermark_test.rs`):** round-trip a `spur-sync v1` sentinel through `format_sync_sentinel` → `parse_sync_sentinel`. Cases: every state value, every optional field present/absent, malformed bodies returning `Err` cleanly, oldest-vs-newest selection across multiple sentinels on the same issue. Plus the marker parser from §4.2: extract marker from various leading-line shapes, reject lookalikes inside body text.

T-4 **Integration — apply_remote_delta (`apply_test.rs`):** in-memory `MockSync` returning a scripted `RemoteDelta`. Tests use a tempdir-backed `BeadsCrateAdapter`. Cases:
   - Fresh repo: 0 existing issues → N created, each with a `spur-sync v1` sentinel.
   - Re-ingest unchanged: cheap-path short-circuit hits; `unchanged == N`.
   - Re-ingest with mutated remote: titles/labels/state change → updated with correct diffs; new sentinel appended.
   - Re-ingest with deletions: simulated 404 surfaces as `Gone` → sentinel `state: disconnected`.
   - Conflict: local mutated since last sync, remote also mutated → `RemoteConflict` returned, Beads issue and sentinel untouched.
   - Idempotency under partial-batch crash: simulate by aborting after writing N/2 nodes; re-run, verify the remaining N/2 land cleanly and no duplicates appear (relies on `external_ref` UNIQUE index).
   - Comment dedup: import the same RemoteComment twice in two separate runs; second run is a no-op via §4.2 marker scan.
   - Recovery branch: pre-create an issue manually with the right `external_ref` but no sentinel; first ingest must adopt it and produce the first sentinel without conflict.

T-5 **Integration — auth (`auth_test.rs`):** mock `gh` binary via `PATH` prepending a shell script that prints a known token to stdout and a warning to stderr. Verify `resolve_token` returns the token cleanly.

T-6 **Smoke — live GitHub (`live_smoke.rs`, ignored by default):** runs against a real public read-only repo (e.g. `octocat/Hello-World`) with the developer's own token. Expects exit 0 and a non-empty report. Not run in CI; documented for contributors to verify locally.

T-7 **Snapshot — CLI output (`cli_snapshot.rs`):** insta snapshots of the human and `--json` outputs against scripted `MockSync` results.

All Phase 1 PRs must add to T-1 through T-5; T-6 is optional; T-7 lands with the CLI PR.

## 11. Acceptance criteria

Phase 1 is complete when, with all PRs merged on a clean checkout:

A-1. `cargo test -p spur-pm` and `cargo test -p spur` pass.
A-2. `cargo test -p spur-pm --test apply_test -- --ignored` (live smoke) passes against a known public repo when `SPUR_GITHUB_TOKEN` or `gh auth status` is set.
A-3. `spur pm ingest github octocat/Hello-World` on a fresh `.beads/` lands ≥1 issue with all three provenance fields populated and exactly one `spur-sync v1` sentinel comment per ingested issue. Idempotent on re-run (`bd list --label gh:issue` count is stable).
A-4. `bd show <ingested-id>` displays `external_ref`, `source_system`, `source_repo`. (Requires the provenance fields to be surfaced through whatever CLI / display path is wired; coordinate with `beads_rust` upstream if needed.)
A-5. Dep hints appear as `spur-dep-hint v1` sentinel comments queryable via `BeadsAdvanced::list_comments`.
A-6. Re-running ingest after a Beads-side title change without a corresponding remote-side change does *not* overwrite the local title (three-way merge per §5.4: local-moved-only → update is dropped). The reverse (remote-moved-only) takes remote. Both-moved → conflict, counted in `IngestReport.conflicts`, Beads untouched.
A-7. No clippy warnings on touched files.
A-8. No code path turns a `DepHint` into an `add_dependency` call — verified by a grep gate in CI.
A-9. **Single-store invariant:** `.beads/` contains exactly the files `beads_rust` already manages — no `external_links.db`, no separate write lock. Verified by a `ls .beads/` check in T-4.

## 12. Sequencing — implementation order

Each entry is one PR. They land in order; each PR is reviewable in isolation.

PR-1. **Surface provenance + `find_by_external_ref` on `spur-pm` types.** Touches `types.rs`, the five hardcoded `None` sites (issue_tracker.rs:325, 465; adapter.rs:570; snapshot.rs:397; test_workspace.rs:116), the adapter create/update/get paths. Adds `IssueTracker::find_by_external_ref` and threads it to `beads_rust`'s existing storage method. Tests: T-1 fixtures pass; existing tests unchanged. (~350 lines.)

PR-2. **`crates/spur-pm/src/sync.rs` + types.** Trait + all data types from §2. `MockSync` impl behind `#[cfg(test)]` for downstream tests. No GitHub yet. Tests: trait compiles, `MockSync` round-trips through the type model. (~400 lines.)

PR-3. **`ingest/watermark.rs` + `dep_hints.rs` + `ingest/mod.rs::apply_remote_delta`.** All state lives in `beads.db` via §4. Sentinel format helpers, marker scanner, three-way conflict detection, the per-node apply flow. Driven by `MockSync`. Tests: T-2, T-3, T-4. (~700 lines.)

PR-4. **`ingest/github/{auth,client,mapping}.rs` + GraphQL query.** No CLI yet. Tests: T-1 mapping fixtures, T-5 auth shim. Pins `octocrab` version and verifies the device-flow surface. (~700 lines.)

PR-5. **`PmService::sync_target` + CLI subcommand.** End-to-end wiring per §8. Tests: T-7 CLI snapshot; manual T-6 live smoke documented in the PR description. (~400 lines.)

Total: five PRs, ≈2,550 LoC. Each PR independently mergeable; behaviors are compile-only callsites until PR-5 exposes the CLI.

## 13. Risks and what we accept

R-1. **octocrab API drift.** Variant names and helper signatures have changed across releases; the spec flags this. Mitigation: PR-4 pins an exact `octocrab` version in `Cargo.toml` and verifies the device-flow API surface against the current docs before merging.

R-2. **GraphQL cost spikes on large repos.** A `pullRequests(first: 50)` query on a 50k-PR repo will exceed per-page budgets. Mitigation: §7.3 P-2 (adaptive page sizing). If even `pageSize=1` busts budget, the run aborts cleanly with `RateLimited`; user re-runs with `--since` to narrow.

R-3. **Comment / timeline truncation at 100 items per node.** Phase 1 accepts truncation with a warning. Mitigation: §7.3 P-3 follow-up `IngestNodeExtras` query, deferred. Real-world impact small for typical OSS issues.

R-4. **Watermark scan cost.** Phase 1 reads watermarks by scanning all comments per issue and filtering for `spur-sync v1`. For an issue with 200 comments this is fine; for a pathological 10k-comment thread it isn't. Mitigation: §4.5 future column migration. Acceptance: measure on `octocat/Hello-World` and a moderately-busy upstream (e.g. ~50-comment issues) — if ingest of 1k issues exceeds 30s of wall time *purely on watermark reads* with WAL mode, fast-track §4.5. Otherwise accept.

R-5. **Dep hint regex misfires.** Body parsing is heuristic; fenced code blocks aren't excluded in Phase 1. Mitigation: hints are never edges (I-8); brain has final say. Accept the false-positive risk in exchange for never blocking the DAG on misparses.

R-6. **Multi-assignee data loss.** Phase 1 drops `assignees[1..]` with a log line. Mitigation: re-visit in Phase 2 alongside the `also-assigned` label scheme. Accept.

R-7. **Token caching out of scope.** Phase 1 may ask for the device flow on every run if `gh` is not installed. Mitigation: keyring persistence in Phase 2. Accept short-term friction.

R-8. **Append-only sentinel growth.** Each ingest of an issue appends a new `spur-sync v1` comment. A weekly-polled issue accumulates ~52 sentinels per year. Mitigation: Phase 2 compaction pass that keeps only the newest sentinel per issue. Until then, accept — sentinels are short (≤300 bytes each).

## 14. What this spec does not cover (Phase 2+)

- ETag-based REST incremental polling (`spur pm sync github`). Adds a `fetch_one` REST path; the sentinel already carries `remote_etag` so no schema change is needed.
- Webhook receiver + smee.io relay. Adds `ingest/github/webhook.rs`; pushes deltas through `apply_remote_delta` unchanged.
- TUI surfacing of `state: needs_auth` / `state: disconnected` sentinels and the conflict review flow.
- Push direction (`push_mutations`): projecting Beads status/comment/label changes back to GitHub. The trait method exists; Phase 1 returns `Skipped { reason: "phase-2" }`.
- Sentinel compaction pass (per R-8).
- **Upstream `beads_rust` PR** adding indexed `remote_etag` / `last_synced_at` / `last_synced_remote_updated_at` / `link_state` columns to `issues`. One-time migration projects the latest sentinel per issue into the columns; reads switch from comment-scan to column-read. Optional optimization, not a correctness fix.
- Linear and Plane sync targets — they reuse `ExternalPmSync` unchanged.

Each of those gets its own design doc, layered on this one.

---

*Spec authored: 2026-05-12. Builds on research in `docs/research/github-ingestion-rust.md`. Companion to `docs/architecture/spur-pm-beads-source-of-truth.md`. Implementation epic: TBD — file with the punch list in §12 as PR descriptions.*
