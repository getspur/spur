# GitHub Issues & PR Ingestion (Rust) — Research

Status: Research only. No production code is changed by this document.

Companion to: [`docs/architecture/spur-pm-beads-source-of-truth.md`](../architecture/spur-pm-beads-source-of-truth.md).

Beads issues: epic `bd-321`, Track A `bd-n0m` (Gemini), Track B `bd-1wx` (claude-code).

## 0. Why this document exists

The architecture doc declares Beads (`.beads/beads.db` via `beads_rust`) the operational source of truth and explicitly carves out a Phase 1/2 path where GitHub becomes an **ingest source** and **sync target**, never a peer authority. Today's `crates/spur-pm/src/github.rs` shells out to the `gh` CLI, populates none of the provenance fields that already exist in the Beads schema, and has no webhook surface. An open-source contributor cannot point Spur at an upstream repository and have its issues land in their local Beads DAG.

This document is the contributor-facing handoff: it gives one person enough to land Phase 1 and the bulk of Phase 2 in a small number of focused PRs.

## 1. Executive summary

- **HTTP client:** `octocrab` (current de-facto Rust GitHub client) for REST + auth + connection pooling, paired with `graphql_client` for compile-time-typed GraphQL queries through `octocrab.graphql()`.
- **Auth UX:** zero-config — shell out to `gh auth token` first; fall back to OAuth Device Flow if `gh` is absent. No PAT-management UX, no GitHub App.
- **Workload split:** GraphQL for bulk ingest of an issue/PR graph (avoids REST N+1); REST + `ETag`/`If-None-Match` for incremental polling, because 304s cost 0 against the rate-limit budget and **GraphQL has no ETag**.
- **Webhooks:** `axum` receiver verifying `X-Hub-Signature-256` against raw body bytes with constant-time comparison; for OSS-contributor local dev, an `smee.io` SSE relay so the local CLI works behind NAT.
- **Beads side:** the Identity Registry primitive is *already in the SQLite schema* — `external_ref` has a UNIQUE partial index and `find_by_external_ref` exists. The work is to (a) surface `source_system` / `source_repo` / `external_ref` through `spur-pm`'s public types, (b) populate them on ingest, (c) add an `external_links` sidecar table for conflict watermarks (etag, `updated_at` watermark, `last_synced_remote_version`).
- **Trait shape:** new `ExternalPmSync` trait in `crates/spur-pm/src/sync.rs`, ingest module at `crates/spur-pm/src/ingest/github/`. `IssueTracker` stays Beads-only; `GitHubAdapter` becomes a sync target + PR service, not an issue authority.
- **Markdown dep extraction is hints, not edges.** Per the architecture doc's lossy-projection stance, parsed `Closes #N` / `Depends on #N` references are stored as structured `spur-dep-hint v1` comments and require Brain verification before mutating the local DAG.

## 2. Findings from Track A — Rust GitHub API substrate

> Source: Gemini (delegation `a15c135b-…`, issue `bd-n0m`). Verified against the architecture doc constraints. **Crate-API specifics flagged with ⚠️ should be confirmed against current `octocrab` docs at implementation time.**

### 2.1 Client choice — `octocrab` + `graphql_client`

- `hubcaps` is effectively unmaintained.
- Raw `reqwest` + hand-rolled types reinvents pagination, auth, and rate-limit plumbing for no benefit.
- `octocrab` provides typed REST endpoints, OAuth + token auth, connection reuse, and an `octocrab.graphql()` escape hatch that accepts arbitrary JSON. Combining that escape hatch with `graphql_client` lets us keep `.graphql` files in-repo and generate typed Rust structs at compile time, while `octocrab` still owns the HTTP lifecycle.

### 2.2 Auth for OSS contributors

Decision: do **not** force PAT management on contributors.

1. **Primary:** `Command::new("gh").args(["auth", "token"])` → trim stdout. Most OSS contributors already have `gh` authenticated. Anything written to stderr (e.g., an update notice) is discarded after trimming stdout.
2. **Fallback:** OAuth Device Flow. Print an 8-character user code + `https://github.com/login/device`, then poll the token endpoint. ⚠️ Verify `octocrab`'s current device-flow API surface (`authenticate_as_device` / `DeviceCodes::poll` etc.) against the published docs before relying on the names — they have changed across versions.
3. GitHub Apps are explicitly *not* the right shape for a local CLI used by an arbitrary contributor — they're built for server-to-server installations.

### 2.3 Rate limits and pagination

- Baseline authenticated quota: **5,000 units / hour** for both REST and GraphQL. REST = 1 unit/request. GraphQL = points based on query complexity, capped at **~2,000 points/min**. ⚠️ Re-check current numbers in GitHub's rate-limit docs at implementation — secondary limits in particular have evolved.
- Pagination:
  - REST: use `octocrab`'s page iteration helpers (⚠️ confirm current API — `all_pages` / `into_stream`).
  - GraphQL: Relay-style cursors via `pageInfo.hasNextPage` / `pageInfo.endCursor`.
- Backoff:
  - REST: read `x-ratelimit-remaining` and `x-ratelimit-reset` on every response. When remaining drops below a configurable floor (e.g. 50), sleep until reset.
  - 403/429 with `Retry-After` header → honor the header before retrying.
  - GraphQL: read the `rateLimit { cost remaining resetAt }` block embedded in each response. Same floor strategy.
  - Use `tokio::time::sleep_until`, not busy spinning.

### 2.4 Conditional requests / incremental sync

- REST supports `ETag` + `If-None-Match`. A `304 Not Modified` **costs 0 units** against the rate-limit budget.
- GraphQL **does not support ETag/If-None-Match.** Every GraphQL query bills points regardless of whether anything changed.
- Implication for the ingest substrate: persist the GitHub `ETag` per remote node in the sidecar table (§3.7 below). Background polling that walks "known nodes" uses REST + ETag and is essentially free until something actually changes.

### 2.5 REST vs GraphQL for our workload

Workload = `repository → issues → (labels, assignees, comments, timeline items, linked PRs/issues)`. That's the canonical N+1 shape for REST: one call per nesting level per parent.

Recommendation:

- **Initial bulk ingest:** one GraphQL query per repository page, deeply nested. Costs more points than one REST call but orders of magnitude less than the equivalent N+1 REST traversal.
- **Incremental polling:** REST with ETags, until webhooks (Phase 4) replace polling entirely.

### 2.6 Webhook receiver

- Events to subscribe to for issue/PR ingestion: `issues`, `pull_request`, `issue_comment`, `pull_request_review`, `pull_request_review_comment`. Add `repository` for transfer/visibility/delete handling.
- Signature verification (HMAC-SHA256, header `X-Hub-Signature-256`):
  1. Compute HMAC against the **raw body bytes** (e.g., `axum::body::Bytes`). Parsing the JSON first would re-serialize the body and break the signature.
  2. Compare using **constant-time equality**: `hmac::Mac::verify_slice` or the `subtle` crate. `==` on byte slices is a timing-attack vulnerability.
  3. Reject without ambiguity on mismatch — do not surface a different error code on "missing header" vs "bad signature."
- Local-dev path: contributor machines are behind NAT, so GitHub can't reach them directly. The receiver should also run as an `smee.io` SSE client that streams events to the local axum handler. Document the `smee` channel creation step in the contributor onboarding.

### 2.7 Failure modes

| Failure | Detection | Response |
|---|---|---|
| Auth expired/revoked | `401 Unauthorized` | Transition link to `NeedsAuth`; surface a Spur TUI prompt to re-auth. Do not panic the sync worker. |
| Repo deleted/private/transferred | `404 Not Found` on previously-valid node | Mark the external link `Disconnected`. Preserve the local Beads issue. Inject a `spur-audit` comment recording the disconnect. |
| Label rename | label `name` changes but `node_id` stable | Key everything on `node_id`. Map name → label string at write time only. |
| Comment deletion | `deleted` webhook action, or `404` on a previously-known comment | Soft-delete in Beads. |
| Rate limit hit despite throttling | `403` with `x-ratelimit-remaining: 0` | Sleep until `x-ratelimit-reset`; emit a metric so the user can see ingest is paused. |

## 3. Findings from Track B — Beads side

### 3.1 What `beads_rust` already gives us (and Spur isn't using)

From `resource/beads_rust/src/storage/schema.rs`:

```sql
issues (
    ...,
    external_ref     TEXT,                          -- nullable
    source_system    TEXT DEFAULT '',
    source_repo      TEXT NOT NULL DEFAULT '.',
    ...
);
CREATE UNIQUE INDEX idx_issues_external_ref_unique
    ON issues(external_ref) WHERE external_ref IS NOT NULL;
```

Plus `beads_rust::storage::sqlite::SqliteStorage::find_by_external_ref(&str) -> Result<Option<Issue>>` and `IssueUpdate::external_ref: Option<Option<String>>`.

Net: **deduplication-by-remote-id is already a SQL-level invariant**. The Identity Registry primitive doesn't have to be built — it has to be *used*. The collision check at insert time (sqlite.rs:1378) means double-ingesting the same GitHub node is a hard error, not silent duplication.

### 3.2 What `spur-pm` is missing

`spur-pm`'s public `Issue`, `IssueCreate`, and `IssueUpdate` in `crates/spur-pm/src/types.rs` **don't expose `source_system` / `source_repo` / `external_ref` at all.** The fields exist on the underlying `beads_rust::model::Issue` and are hardcoded to `None` at five sites:

- `crates/spur-pm/src/beads_crate/issue_tracker.rs:325, 465`
- `crates/spur-pm/src/beads_crate/adapter.rs:570`
- `crates/spur-pm/src/graph_engine/snapshot.rs:397`
- `crates/spur-pm/src/test_workspace.rs:116`

Step zero of any ingest work is surfacing them through `spur-pm`'s contract.

### 3.3 GitHub → Beads field mapping

| GitHub (REST/GraphQL) | Beads | Notes |
|---|---|---|
| `node_id` (GraphQL global ID, e.g. `I_kwDO…`) | `external_ref = "github:<node_id>"` | Survives renames/transfers; stable. Use as the key. |
| `number` | preserved in `external_links` sidecar | Display only. Mutable across transfers in edge cases. |
| `owner/repo` | `source_repo` | Replaces default `"."`. |
| `"github"` | `source_system` | Replaces default `""`. |
| `title` | `title` | Direct. |
| `body` | `description` | Direct. Markdown preserved. |
| `state="open"` | `Status::Open` | |
| `state="closed", state_reason="completed"` | `Status::Closed` | |
| `state="closed", state_reason="not_planned"` | `Status::Closed` + label `gh:not-planned` | Closed-reason fidelity via label. |
| PR `draft=true` | `Status::Draft` | PRs only. |
| `assignees[0].login` | `assignee` | Beads is single-assignee; rest preserved as labels `gh:also-assigned:<login>` *or* dropped (decide explicitly; recommend drop + comment). |
| `labels[].name` | Beads labels, prefixed `gh:<name>` | Namespacing prevents collisions with Spur's own labels. |
| `labels` containing `bug` / `enhancement` / `documentation` / `question` | also infer `IssueType` heuristically | Mapping table in `ingest/github/mapping.rs`. |
| `labels` containing `priority:p0` / `p0` / `priority/critical` | `Priority::CRITICAL` | Heuristic. Default `Priority::BACKLOG`. |
| `created_at` / `updated_at` | direct | |
| `html_url` | `url` field on PM `Issue` (already exists) | |
| `comments` (paginated) | one-time: ingest as Beads comments with `spur-audit v1 kind:imported` sentinel | Body preserved verbatim. |
| `pull_request` presence | `IssueType::Feature` (default) | Treat PRs as work items. Refine later. |
| GraphQL `timelineItems` of type `CrossReferencedEvent` / `ClosedEvent { closer }` | `spur-dep-hint v1` comment | Hints only. |
| `etag` (HTTP header for REST node fetch) | sidecar `external_links.remote_etag` | For 304 short-circuit. |

### 3.4 Provenance population

When inserting:

```text
external_ref  = "github:<node_id>"
source_system = "github"
source_repo   = "<owner>/<repo>"
```

These three fields together identify any remote node. Because `external_ref` has a UNIQUE partial index, a re-ingest collides at SQL level — we lift that collision into the ingest path as "found existing local bead; switch to update flow."

### 3.5 `ExternalPmSync` trait sketch

Lives in new module `crates/spur-pm/src/sync.rs`. Separate from `IssueTracker`; `IssueTracker` remains Beads-only (the invariant from the architecture doc).

```rust
#[async_trait]
pub trait ExternalPmSync: Send + Sync {
    /// Stable identifier — "github", "linear", "plane".
    fn source_system(&self) -> &str;
    /// e.g. "getspur/spur".
    fn source_repo(&self) -> &str;

    /// Pull all remote changes (issues, PRs, comments, links) since `since`.
    /// If `since` is None the implementation may return the full repo state.
    /// The returned `RemoteDelta` is purely descriptive — applying it to Beads
    /// is the brain's job and goes through `BeadsCrateAdapter`'s write lock.
    async fn fetch_changes_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<RemoteDelta>;

    /// Fetch one remote node by stable id. Used for webhook deliveries and
    /// for ETag-based incremental re-polls of known nodes.
    async fn fetch_one(
        &self,
        remote_id: &str,
        if_none_match: Option<&str>,
    ) -> Result<Option<RemoteNode>>;

    /// Project a vector of local Beads mutations onto the remote.
    /// "Eventually consistent side effect" per the architecture doc.
    async fn push_mutations(&self, diff: Vec<LocalMutation>) -> Result<Vec<PushOutcome>>;

    /// Compare local watermarks against the remote and return any links
    /// where the remote moved without our knowledge. Used before push to
    /// satisfy the "no last-write-wins" rule.
    async fn detect_conflicts(
        &self,
        watermarks: &[SyncWatermark],
    ) -> Result<Vec<RemoteConflict>>;
}

pub struct RemoteDelta {
    pub nodes: Vec<RemoteNode>,
    pub deletions: Vec<RemoteRef>,
    pub watermark: DateTime<Utc>,  // server-time cursor for next call
}

pub struct RemoteNode {
    pub remote_id: String,            // GitHub node_id
    pub remote_number: Option<u64>,
    pub kind: RemoteKind,             // Issue | PullRequest
    pub title: String,
    pub body: String,
    pub state: RemoteState,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub html_url: String,
    pub etag: Option<String>,         // REST polls only
    pub dep_hints: Vec<DepHint>,      // extracted from body
    pub comments: Vec<RemoteComment>,
    pub raw: serde_json::Value,       // preserves anything we didn't map
}

pub enum RemoteState {
    Open,
    Closed { reason: Option<String> },
    Draft,
}

pub enum RemoteKind { Issue, PullRequest }

pub struct SyncWatermark {
    pub beads_id: String,
    pub remote_id: String,
    pub last_synced_at: DateTime<Utc>,
    pub last_synced_etag: Option<String>,
    pub last_synced_remote_updated_at: DateTime<Utc>,
}
```

`PmService::sync_target(source_system: &str) -> Option<&dyn ExternalPmSync>` exposes this without ever touching `PmBackendInner` — preserving the architecture-doc invariant against peer issue backends.

### 3.6 Identity Registry — sidecar table

`beads_rust` already has `(external_ref, source_system, source_repo)`, but **not** a remote-version-hash column. Two options:

- **(A) Upstream PR to `beads_rust`** adding `remote_version_hash TEXT` and `last_synced_at DATETIME` to `issues`. Cleanest long-term. Requires upstream coordination and migration discipline.
- **(B) Sidecar table managed by `spur-pm`.** Faster to land, decouples Spur from upstream cadence.

Recommend **(B)** for Phase 1, with (A) as a follow-up if upstream is receptive. Two implementation choices for (B):

1. Same DB (`.beads/beads.db`), new table in a `spur_*` namespace. Requires reaching past `BeadsCrateAdapter`'s typed API into a raw `Connection`. Schema management collides with `beads_rust` migrations.
2. **Separate SQLite file `.beads/external_links.db`.** No schema collision risk; spur-pm owns it end-to-end. Recommended.

Sidecar schema:

```sql
CREATE TABLE IF NOT EXISTS external_links (
    beads_id            TEXT NOT NULL,
    source_system       TEXT NOT NULL,
    source_repo         TEXT NOT NULL,
    remote_id           TEXT NOT NULL,    -- node_id
    remote_number       INTEGER,
    remote_etag         TEXT,
    remote_updated_at   DATETIME NOT NULL,
    last_synced_at      DATETIME NOT NULL,
    last_synced_remote_version_hash TEXT,
    state               TEXT NOT NULL,    -- "active" | "needs_auth" | "disconnected"
    PRIMARY KEY (source_system, remote_id)
);
CREATE INDEX idx_external_links_beads_id
    ON external_links(beads_id);
CREATE INDEX idx_external_links_state
    ON external_links(state);
```

Per the architecture doc, `last_synced_remote_version_hash` is the **watermark** used by `detect_conflicts`: if `remote.updated_at > watermark.last_synced_remote_updated_at` AND `remote.etag != watermark.last_synced_etag`, the remote moved without us — return a `RemoteConflict` instead of overwriting.

### 3.7 Markdown dependency-hint extraction

Per the architecture doc: ingested dependencies from Markdown are **hints, not edges.** They surface to the brain; only the brain mutates the local Beads DAG.

Extraction targets in issue/PR `body` (case-insensitive, `\b`-boundaried):

- GitHub closing keywords (REST default + linked-PR rendering): `close[sd]?`, `fix(es|ed)?`, `resolve[sd]?`
- Custom Spur conventions documented for OSS contributors: `Depends on #N`, `Blocked by #N`, `Blocks #N`
- Cross-repo: `<owner>/<repo>#<N>` after any of the above keywords
- GitHub task lists: `- [ ] #N` or `- [ ] <owner>/<repo>#N` — GitHub renders these as tracked-by relations
- GraphQL `timelineItems` of type `CrossReferencedEvent` / `ClosedByPullRequestsConnection` (PRs only) — preferred over Markdown when available because GitHub already did the resolution.

Storage: persist as `spur-dep-hint v1` sentinel comments on the local Beads issue. Schema:

```
spur-dep-hint v1
kind: closes|fixes|resolves|depends-on|blocks|blocked-by|task-list
remote_keyword: "Closes"
remote_ref: "owner/repo#42"        // raw, before resolution
resolved_beads_id: bd-XYZ | null   // null if we haven't ingested it yet
raw_span: "Closes #42"             // for audit
source: body | timeline_item       // provenance of the hint
```

The brain consumes these via existing `BeadsAdvanced::list_comments` and decides whether to materialize a real `blocks` / `parent-child` edge. **Never write edges from ingest.**

### 3.8 Module layout

```
crates/spur-pm/src/
  sync.rs                        # ExternalPmSync trait + shared types
  ingest/
    mod.rs                       # apply_remote_delta() — runs under write lock
    external_links.rs            # sidecar table CRUD
    dep_hints.rs                 # Markdown + timeline extraction
    github/
      mod.rs                     # GitHubSync impl of ExternalPmSync
      client.rs                  # octocrab wrapper + rate-limit governor
      auth.rs                    # gh-cli shell-out → device-flow fallback
      mapping.rs                 # GitHub → Beads field mapping
      graphql/
        ingest_repo.graphql      # bulk ingest query
        types.rs                 # graphql_client-generated
      webhook.rs                 # axum receiver + smee.io relay (Phase 4)
```

All writes still flow through `BeadsCrateAdapter::write` (the `.beads/.write.lock` holder). The ingest module never opens `SqliteStorage` directly except for the sidecar table, which lives in its own DB file and uses its own lock.

### 3.9 Conflict watermark / 3-way merge

The architecture doc explicitly forbids last-write-wins. Implementation:

1. Before any `push_mutations` call, fetch the remote node's current `updated_at` + `etag`.
2. Compare against `external_links.last_synced_remote_updated_at` + `last_synced_etag`.
3. If they diverged, the remote moved without us → return `RemoteConflict { local, remote, base }` to the brain. **Do not push.**
4. After a successful push or ingest, update the watermark within the same DB transaction that wrote the Beads issue.

For Phase 1, conflict resolution is "surface to the user via TUI / brain decision," not auto-merge. Auto-merge is out of scope until at least Phase 3 (Linear) when we have a higher-fidelity remote model to merge against.

## 4. End-to-end ingest flow (Phase 1+2 reference)

```
$ spur pm ingest github getspur/spur

1. Auth                  -> auth::resolve_token()
                            -> try `gh auth token`; else device flow
2. Client                -> octocrab::OctocrabBuilder::personal_token(token).build()
3. Bulk fetch            -> GitHubSync::fetch_changes_since(None)
                            -> graphql ingest_repo.graphql (paginated)
                            -> returns RemoteDelta { nodes, deletions, watermark }
4. Apply, under write lock:
   For each node:
     a. external_links lookup by (source_system, remote_id)
     b. If not found:
          beads_id = create_issue(IssueCreate { ..., external_ref, source_system, source_repo })
          external_links insert
        Else:
          watermark check; if diverged → emit RemoteConflict and skip
          update_issue(...) with mapped fields
          external_links update (etag, updated_at, version_hash)
     c. dep_hints::extract(body) + timeline → spur-dep-hint v1 comments
     d. comments[] → Beads comments (idempotent on remote_comment_id)
5. Print:
   "Ingested 247 issues, 89 PRs, 14 dep hints. Use `spur ready` to start working."
```

The contributor's first 30 seconds with Spur on an OSS repo is now: install, `gh auth login` (one-time), `spur pm ingest github <owner>/<repo>`, `spur ready`.

## 5. Punch list — beads tasks to file as the Phase 1/2 roadmap

Each item below is sized to be filed as a `spur-pm` task. Suggested epic: `Phase 1+2: GitHub ingestion`.

1. **types: surface provenance fields on `spur-pm` PM types.** Add `source_system`, `source_repo`, `external_ref` to `Issue`, `IssueCreate`, and `IssueUpdate`; thread them through `BeadsCrateAdapter::create_issue` / `update_issue`; update the five `None` sites; add round-trip tests.
2. **trait: introduce `ExternalPmSync` in `crates/spur-pm/src/sync.rs`.** Trait + types per §3.5. No GitHub impl yet — just the contract + a stub `MockSync` for tests.
3. **schema: `external_links` sidecar DB.** New module `ingest/external_links.rs`, owns `.beads/external_links.db`, schema per §3.6, plus CRUD + a `migrate()` that runs on first use. Decide and document file-lock discipline.
4. **client: `octocrab` + token resolver.** `ingest/github/{client,auth}.rs`. Implement `auth::resolve_token()` with `gh auth token` primary + OAuth Device Flow fallback. Verify current `octocrab` device-flow API names before wiring.
5. **bulk fetch: GraphQL ingest query.** Author `ingest_repo.graphql`, integrate `graphql_client`, return `RemoteDelta`. Include rate-limit observation (`rateLimit { cost remaining resetAt }`) and floor-based backoff.
6. **mapping: GitHub → Beads.** `ingest/github/mapping.rs` per §3.3. Include heuristics for `IssueType` and `Priority` from labels. Tests against fixture JSON.
7. **dep-hint extraction.** `ingest/dep_hints.rs`. Regex set for closing keywords, `Depends on / Blocked by / Blocks`, cross-repo refs, task lists. Output `spur-dep-hint v1` sentinel comments. Fuzz tests against real GitHub body samples.
8. **apply step under write lock.** `ingest/mod.rs::apply_remote_delta()`. Idempotent. Conflict-aware. Emits `RemoteConflict` instead of overwriting on watermark divergence.
9. **CLI subcommand: `spur pm ingest github <owner>/<repo>`.** Wire steps 4–8. Show progress + final summary. Exit non-zero on auth failure.
10. **REST + ETag incremental poll.** `ingest/github/client.rs` REST path. Cache ETags from `external_links`, issue `GET /issues/:id` with `If-None-Match`, no-op on `304`. Add a `spur pm sync github` command for one-shot incremental sync.
11. **webhook receiver + smee.io relay.** `ingest/github/webhook.rs`. Axum route, raw-bytes HMAC-SHA256 verification with constant-time comparison, `smee.io` SSE client for local dev. Defer to Phase 4 in the architecture doc but the receiver itself is small.
12. **failure-state plumbing.** Wire 401 → `external_links.state = needs_auth`, 404 → `disconnected`, surfaced via `BeadsAdvanced` queries so the brain and TUI can see the state without reading the sidecar.
13. **(optional) upstream PR to `beads_rust`** adding `remote_version_hash` + `last_synced_at` to `issues`. Migrate the watermark out of the sidecar if accepted.

## 6. Open questions

- **Multi-repo ingest.** A single `.beads/` may want to ingest from multiple GitHub repos (e.g. `rust-lang/rust` + `rust-lang/rust-clippy`). `source_repo` already supports this, but the CLI/TUI surface for picking one needs design.
- **Comment authorship preservation.** Beads comments have an `actor` field. Should imported comments preserve `<github-login>@github`, drop the actor, or store the GitHub login verbatim? Recommendation: prefix with `gh:<login>`.
- **GraphQL query budget.** A naive `ingest_repo` query on a large repo can exceed the 2,000-points/minute cap in one shot. Either split into smaller paged queries upfront or implement an adaptive paginator that observes `cost` and shrinks page size if a single page exceeds budget.
- **GitHub Projects (Beta).** Out of scope for this research. Linear-style projects/views are a Phase-3 concern, not Phase 1/2.
- **Bidirectional dep edges.** Once dep hints are reliably extracted, when (if ever) should ingest auto-materialize them? Current answer: never; always brain-mediated. Worth a separate design discussion before Phase 2 ships.

## 7. Verification checklist (before merging Phase 1)

- [ ] Round-trip test: create local issue → push → fetch_one → fields match → push again → conflict detected because watermark moved.
- [ ] Re-ingest is idempotent: running `spur pm ingest github X/Y` twice produces no new Beads rows.
- [ ] Webhook signature verification: hand-crafted payload with known secret matches; tampered payload rejected; missing header rejected; payload with valid signature but mutated body rejected.
- [ ] Rate-limit floor honored under load: simulate ramping requests, verify the client sleeps before exhausting budget.
- [ ] 304 short-circuit measurable: poll an unchanged issue twice, second call records `rate_limit_used: 0`.
- [ ] Provenance: every ingested issue has all three of `external_ref`, `source_system`, `source_repo` set.
- [ ] Dep hints never become edges automatically — search `crates/spur-pm` for any code path that turns a `DepHint` into an `add_dependency` call. There should be none outside brain-mediated flows.

---

*Research authored: 2026-05-12. Brain: claude-opus-4-7. Track A research: gemini (`bd-n0m`, delegation `a15c135b-…`). Track B research: claude-code (`bd-1wx`). Synthesis: claude-opus-4-7 against `bd-321`.*
