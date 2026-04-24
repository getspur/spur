# Brain continuation — artifact-store-at-orchestrator design

- **Date:** 2026-04-25
- **Status:** Draft (ready for human review)
- **Authors:** Kevin Truong (kevin.truong.ds@gmail.com), with Claude Opus 4.7 as design pair
- **Reviewers:** codex (rounds 1–2 + round 5), kimi (rounds 3–4 + round 5), gemini (round 5)
- **Supersedes:** `docs/superpowers/specs/2026-04-24-brain-continuation-producer-envelope-fit-design.md` (Plan-4 truncation-ladder approach)
- **Related:**
  - `docs/superpowers/specs/2026-04-24-brain-continuation-delivery-guarantees.md` (v3.1, merge `6b1e6980`)
  - `docs/superpowers/specs/2026-04-13-spur-context-engine-design.md` (approved, Phase 1 not yet shipped — see §13)
  - `docs/superpowers/reviews/2026-04-24-brain-continuation-rca.md`
  - Review log: `docs/rca/log.md`

---

## 1. Problem

After v3.1's merge of brain-continuation delivery guarantees, a production drop surfaced:

```
⚠ Continuation dropped for 9b2c84e0-…: OversizedSingleItem { continuation_bytes: 4478, budget_bytes: 4096 }
```

A worker continuation overshot the merger's 4 KB envelope budget by 382 bytes and was terminally dropped. The brain never learned the delegation completed.

A subsequent review-design-rounds investigation produced two paths:

- **Plan-4 (truncation-at-producer)**: shrink continuation fields at the producer to fit the budget. Lossy but localized.
- **Plan-5 (this spec): artifact-store-at-orchestrator**: persist full payload to an artifact store; deliver a thin handle to the brain; brain fetches details on demand.

**The user explicitly chose Plan-5** for long-term architectural correctness: *"I prefer artifact store which is more long term, but I need to handle at orchestrator level."*

A short-term mitigation (`MERGE_BUDGET_DEFAULT_BYTES: 4096 → 8192`) was committed as `0718592` to buy operational breathing room while this design lands. Continuations >8 KB still drop until Plan-5 ships.

## 2. Goals and non-goals

### Goals

- Close [INV-D8](#4-invariants) structurally: per-continuation envelope is bounded by construction (lean handle), regardless of worker output size.
- Decouple per-continuation persistence from per-prompt context cost. With 20 parallel workers, raw budget bumping doesn't scale (`20 × 64 KB = 1.28 MB > 200K-token brain context window`). Artifact store is the only architectural primitive that scales.
- Repair a latent bug in the existing `WorkerArtifact` mapping that drops `object_ref/blob_sha`, making the existing oversized-stdout artifacts unfetchable from the brain (write-only today).
- Subsume the existing `worktrees.persist_artifact` git-blob path under a single trait abstraction. No parallel persistence systems.
- Provide operator-visible telemetry: where artifacts live, how many exist, how big, when they were last accessed, when they were GC'd.
- Compose cleanly with the planned `spur-context` ContextEngine (per the 2026-04-13 spec), without blocking on its Phase 1 implementation.

### Non-goals (explicit)

- **Not fixed this spec:** legitimate terminal drops by other reasons — `StaleSession`, `SessionSwap`, `OverflowFull`, `OverflowChannelClosed`, `AlreadyDelivered`. These are by-design under v3.1's INV-D1..D7.
- **Not fixed this spec:** changes to merger packing algorithm or v3.1's checkout/commit handshake.
- **Not fixed this spec:** cloud storage backends (S3, GCS). Trait abstraction permits future addition; FS + git-blob impls suffice for current single-machine deployment.
- **Not fixed this spec:** ContextEngine Phase 1 implementation (decisions/observations tables). Integration hook documented in §13.
- **Deferred:** brain-side caching of fetched artifacts. Brain currently re-fetches on each prompt; LLM cache invalidation analysis in §9.4.

## 3. Architecture overview

```
Worker completes
  → spur-mcp collector receives DelegationResult
  → invokes orchestrator-injected OutcomeMaterializer
                            │
                            ▼
       ┌────────────────────────────────────────────┐
       │ OutcomeMaterializer (in spur-core)         │
       │ uses OutcomeStore trait (in spur-blob-store)│
       └────────────────────┬───────────────────────┘
                            │
                            ├── 1. Persist FULL DelegationResult to OutcomeStore
                            │    (key: brain_session/delegation/attempt; sha256 dedup)
                            │
                            ├── 2. Build LEAN BrainContinuation v3:
                            │    - status, summary (capped 512B), diff_summary (counts),
                            │      worker_branch (capped 256B), estimated_cost_usd,
                            │      artifact_id (Some if backing artifact exists),
                            │      fetch_hint (explicit recovery instruction)
                            │
                            └── 3. push_continuation through existing v3.1 ingress
                                 (envelope bounded by construction → INV-D8 holds)

Brain receives lean continuation
  → sees inline headline + artifact_id
  → optionally calls fetch_outcome_artifact(delegation_id, section?) MCP tool
  → returns DelegationResult subset on demand (paginated by section)
  → makes next-action decision

Background sweeper
  → on session terminate (commit/abort): drop entire <brain_session_id>/ namespace
  → on orchestrator startup: TTL-sweep namespaces older than SPUR_OUTCOME_TTL_DAYS (7d default)
  → manual: spur gc outcomes [--dry-run] [--older-than=30d]
  → also covers existing refs/spur/artifacts/* debt (no GC today)
```

## 4. Invariants

### INV-D8 (this spec, structural)

> For every `BrainContinuation` delivered to `pack_continuations`, the post-`OutcomeMaterializer` envelope satisfies `continuation_cost_bytes(cont) ≤ MERGE_BUDGET_DEFAULT_BYTES` by construction.

The lean payload's fixed-cap fields total ~768 B inline; the artifact reference is a small content-addressed handle. Any single delegation's content beyond the cap lives in the artifact store, fetched on demand. The merger's `OversizedSingleItem` branch becomes unreachable for `OutcomeMaterializer`-produced continuations.

### INV-D9 (preserved from Plan-4 superseded spec, schema-evolution guard)

> Every `DelegationStatus` and `TimeoutFallback` variant containing `String`, `Vec<String>`, or `Vec<PathBuf>` fields must have a registered clip in the truncation-ladder fallback (used only when artifact persistence fails). Enforced by exhaustive-`match` proptest strategy.

The truncation ladder from Plan-4 survives in this fallback role. Its full design lives in the superseded spec (`2026-04-24-brain-continuation-producer-envelope-fit-design.md`); this spec references it without re-stating.

### INV-α (rescoped from earlier rounds)

> No continuation produced through `OutcomeMaterializer` is dropped for `DropReason::OversizedSingleItem`.

Other v3.1 drops (stale session, overflow, etc.) are out of scope here, preserved as-is.

## 5. Phase 1 — Brain-visible fetch fix (no new crate)

**Independently shippable. Brain-visible value. Foundation for Phase 2.**

### 5.1 Repair `WorkerArtifact` → `ArtifactRef` metadata loss

Current state at `crates/spur-mcp/src/server.rs:237-249`:

```rust
fn map_worker_artifact_ref(
    delegation_id: &DelegationId,
    artifact: Option<&spur_acp::domain::artifact::WorkerArtifact>,
) -> Option<spur_acp::domain::ArtifactRef> {
    artifact.map(|artifact| spur_acp::domain::ArtifactRef {
        kind: ContinuationArtifactKind::Other("worker_artifact".into()),
        uri: format!("spur://artifact/{}", delegation_id.as_str()),
        byte_size: artifact.size_bytes as u64,
        sha256: None,                 // ← drops blob_sha (40-char SHA-1)
                                      // ← drops object_ref ("refs/spur/artifacts/<session>")
    })
}
```

The mapping discards both `object_ref` and `blob_sha`. The brain receives a URI `spur://artifact/<delegation_id>` but no path back to the actual git blob. **The existing oversized-stdout artifacts are unfetchable today.**

Fix: extend `ArtifactRef` to preserve git-blob metadata.

```rust
// in spur-acp/src/domain/continuation.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub kind: ArtifactKind,
    pub uri: String,
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Git ref path (e.g., "refs/spur/artifacts/<session>") when stored as a git blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_object_ref: Option<String>,
    /// 40-char hex SHA-1 of the git blob; survives ref deletion until git GC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_blob_sha: Option<String>,
}
```

Update `map_worker_artifact_ref` to populate both new fields. Existing consumers see `Option::None` for both — backward-compatible.

### 5.2 Add `fetch_outcome_artifact` MCP tool (minimal)

New tool in `spur-mcp/src/server.rs`. Phase 1 surface:

```rust
// JSON-RPC tool name: "fetch_outcome_artifact"
// Args: { "delegation_id": String }
// Returns: { "content": [{ "type": "text", "text": <full text> }] }

async fn handle_fetch_outcome_artifact(&self, args: &Value) -> Result<String, String> {
    let delegation_id = args["delegation_id"].as_str().ok_or("missing delegation_id")?;
    // ... look up the ArtifactRef for this delegation in completed_delegations
    // ... if git_object_ref present: invoke `git cat-file -p <object_ref>` via spur-worktree
    // ... return text content
}
```

**Authorization scoping (per codex SF3):** the tool reads `self.brain_session_id` from `McpCallbackServer` (server.rs:301), NOT from a user-supplied argument. Cross-session reads rejected.

**Phase 1 scope:** read-only access to the existing git-blob-backed `WorkerArtifact` payloads. Section pagination NOT yet supported (deferred to Phase 3 when section-aware blob layout is introduced).

### 5.3 Tests

- Unit: `map_worker_artifact_ref_preserves_git_metadata` — assert `git_object_ref` and `git_blob_sha` round-trip through the mapping.
- Integration: `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs` — produce an oversized-stdout worker run, fetch via the new tool, assert content matches the original output text.
- Auth: `fetch_outcome_artifact_rejects_cross_session` — call with a `delegation_id` whose `WorkerArtifact` belongs to a different `brain_session_id`; assert error.

### 5.4 Migration / rollback

- Additive schema change (two new `Option<String>` fields). Old wire payloads deserialize cleanly via `#[serde(default)]`. New brains see existing-shape continuations unchanged.
- Single-commit revert safe.
- No new crate.

## 6. Phase 2 — Introduce `spur-blob-store` crate

**Trait abstraction earns its keep when ≥2 backends + ≥2 consumers exist. Phase 2 satisfies both.**

### 6.1 New crate

```
crates/spur-blob-store/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs           # pub mod + re-exports
    ├── trait_def.rs     # OutcomeStore async trait
    ├── types.rs         # OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError
    ├── fs_store.rs      # FsOutcomeStore impl (default for new outcomes)
    ├── memory_store.rs  # MemoryOutcomeStore impl (test/dev)
    └── measured.rs      # MeasuredOutcomeStore<S> decorator (tracing emit)
```

### 6.2 Cargo.toml

```toml
[package]
name = "spur-blob-store"
description = "Content-addressed outcome blob storage for SPUR delegations"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
spur-acp = { workspace = true }     # for typed identifiers (BrainSessionId, DelegationId) only

[dev-dependencies]
tempfile = "3"
proptest = "1"
```

### 6.3 Public API

```rust
// trait_def.rs
#[async_trait::async_trait]
pub trait OutcomeStore: Send + Sync {
    /// Persist a payload. Returns OutcomeRef with content addressing.
    /// Idempotent: same key + same content → same OutcomeRef, no rewrite.
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError>;

    /// Retrieve content by key, optionally narrowed to a section.
    /// Phase 2: section is ignored (returns full payload).
    /// Phase 3: section-aware retrieval avoids context bloat.
    async fn get(
        &self,
        key: &OutcomeKey,
        section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError>;

    /// Drop all artifacts for a brain_session. Called on session terminate.
    /// Returns count of artifacts dropped.
    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError>;

    /// Sweep namespaces whose newest artifact is older than `ttl`.
    /// Crash-recovery + ops fallback. Returns sweep report.
    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError>;
}

// types.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutcomeKey {
    pub brain_session_id: BrainSessionId,
    pub delegation_id: DelegationId,
    pub attempt: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeMetadata {
    pub created_at: DateTime<Utc>,
    pub content_type: ContentType,           // Diff | Stdout | Stderr | Json
    pub original_byte_size: u64,
    pub stored_byte_size: u64,                // post-truncation if applicable
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRef {
    pub key: OutcomeKey,
    pub sha256: String,                       // 64-char hex
    pub byte_size: u64,
    pub backend: BackendTag,                  // Fs | GitBlob (+ future: S3, etc.)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Section {
    StatusOnly,
    Summary,
    DiffOnly,
    Full,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("not found: {0:?}")] NotFound(OutcomeKey),
    #[error("authorization: caller session != artifact session")] Unauthorized,
    #[error("content too large: {actual} > {limit}")] TooLarge { actual: u64, limit: u64 },
    #[error("backend: {0}")] Backend(String),
}
```

### 6.4 Implementations

**`FsOutcomeStore`** (in `spur-blob-store::fs_store`):
- Path layout: `<root>/<brain_session_id>/<delegation_id>/<attempt>.json`
- `put` writes atomically via `tempfile` + rename.
- `get` reads via `tokio::fs::read`.
- `delete_namespace` recursively deletes `<root>/<brain_session_id>/`.
- `sweep_older_than` walks namespaces, checks newest mtime, deletes older ones.
- Default root: `$SPUR_DATA_DIR/outcomes/` (uses `directories` crate fallback per `spur-context` precedent).

**`GitBlobOutcomeStore`** (in `spur-worktree`, NOT in `spur-blob-store`):
- spur-worktree gains a `spur-blob-store` workspace dependency.
- `worktrees.persist_artifact` (currently at `crates/spur-worktree/src/manager.rs:295`) is wrapped to also implement `OutcomeStore::put`.
- Existing `WorkerArtifact { object_ref, blob_sha, size_bytes, kind }` shape preserved on the wire.
- Internal layout: continues using `refs/spur/artifacts/<session-id>` git refs.
- `delete_namespace` invokes `git update-ref -d refs/spur/artifacts/<session-id>`.

**`MemoryOutcomeStore`** (test impl, in `spur-blob-store::memory_store`):
- `Arc<RwLock<HashMap<OutcomeKey, (Vec<u8>, OutcomeMetadata)>>>`.
- Used by all consumer crates' tests.

**`MeasuredOutcomeStore<S: OutcomeStore>`** (decorator, in `spur-blob-store::measured`):
- Wraps any `OutcomeStore`. Emits `tracing::event!` for put/get latency, content size, namespace deletions.
- Target: `spur.metrics.blob_store.*` (matches structured-tracing pattern from Plan-4 §12.1).
- Optional in production; mandatory in dev/CI for visibility.

### 6.5 Migration of existing `worktrees.persist_artifact`

The orchestrator call site at `crates/spur-core/src/orchestrator.rs:4755-4773` switches from direct `worktrees.persist_artifact(...)` to:

```rust
let store: Arc<dyn OutcomeStore> = ...;  // injected at startup, GitBlobOutcomeStore wrapper
let key = OutcomeKey { brain_session_id: brain_session.clone(), delegation_id, attempt };
let metadata = OutcomeMetadata { created_at: Utc::now(), content_type: ContentType::Stdout, ... };
let outcome_ref = store.put(&key, output_text.as_bytes(), &metadata).await?;
let artifact_ref = ArtifactRef::from_outcome_ref(outcome_ref);   // populates git_object_ref, git_blob_sha if backend == GitBlob
```

`worktrees.persist_artifact` retains its public signature for in-place use during the transition window; the trait wrapper is the new orchestrator entrypoint. Deprecation of the direct call is Phase 3 cleanup.

### 6.6 Tests

Unit (`crates/spur-blob-store/src/*.rs` inline `#[cfg(test)]`):
- `fs_store_put_get_roundtrip` — round-trip a payload, verify identity + sha256.
- `fs_store_idempotent_put` — same key + same content twice → no rewrite, same `OutcomeRef`.
- `fs_store_namespace_isolation` — `delete_namespace` for session A doesn't affect session B.
- `fs_store_sweep_respects_ttl` — fresh namespaces survive sweep.
- `fs_store_concurrent_puts` — same-process concurrent puts to distinct keys succeed.
- `memory_store_*` — same shape as fs_store.
- `measured_store_emits_tracing` — wraps a memory store, captures tracing events via `tracing-test`.

Integration (`crates/spur-blob-store/tests/`):
- `fs_store_atomicity_under_crash` — abort mid-write via process kill in subprocess; subsequent open detects partial state, recovers cleanly.

Cross-crate integration (`crates/spur-worktree/tests/git_blob_store_impl.rs`):
- `git_blob_store_implements_outcome_store_trait` — exercise the full trait surface against a tempfile-based git repo.
- `git_blob_store_subsumes_persist_artifact` — assert legacy persist_artifact callers see equivalent behavior through the trait.

Property-based (`crates/spur-blob-store/tests/proptest_invariants.rs`):
- 256 cases. Random keys, random content (incl. binary, control chars), random sequences of put/get/delete. Assert: idempotence, namespace isolation, content roundtrip, no panics.

### 6.7 Migration / rollback

- New crate is additive; existing `worktrees.persist_artifact` remains during transition.
- Orchestrator call site switch is one-commit revert.
- `WorkerArtifact` wire format preserved.

## 7. Phase 3 — Lean schema v3 + extended fetch + truncation fallback

### 7.1 Lean `BrainContinuation` payload (schema_version: 3)

```rust
// in spur-acp/src/domain/continuation.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationPayload {
    pub status: DelegationStatus,
    /// Always inline. Capped at 512 B with "…" sentinel if longer.
    pub summary: Option<String>,
    /// Counts only — no `files` path list inline. Full file list via fetch_outcome_artifact.
    pub diff_summary: Option<DiffSummary>,
    /// Always inline. Capped at 256 B.
    pub worker_branch: Option<String>,
    /// NEW field — required for cost-aware brain reasoning.
    pub estimated_cost_usd: Option<f64>,
    /// Reference to a backing artifact when full payload exceeds inline capacity.
    /// Some(_) ⇒ brain may call fetch_outcome_artifact for fuller context.
    pub artifact_id: Option<OutcomeKey>,
    /// Explicit human-readable hint when artifact_id is Some.
    /// Example: "Full diff truncated. Call fetch_outcome_artifact(delegation_id, section='diff_only')."
    pub fetch_hint: Option<String>,
}
```

**Schema bump rationale (per codex MF4):** ops-debug visibility, not wire-protocol break. LLMs are tolerant of optional fields via serde defaults; this bump is for log/dashboard correlation when investigating brain behavior pre/post Plan-5.

**Renderer change:** `crates/spur-core/src/continuation_bridge.rs:149` updates `ContinuationResourceBody.schema_version` constant from `2` to `3`.

### 7.2 `OutcomeMaterializer` (in spur-core)

```rust
// crates/spur-core/src/outcome_materializer.rs (new module)
pub struct OutcomeMaterializer {
    store: Arc<dyn OutcomeStore>,
    summary_cap_bytes: usize,        // = 512
    worker_branch_cap_bytes: usize,  // = 256
}

impl OutcomeMaterializer {
    /// Build the lean BrainContinuation. Persists full payload to store
    /// BEFORE returning the lean handle. If persist fails, falls through
    /// to the truncation-ladder fallback (Plan-4 spec §6).
    pub async fn materialize(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation { ... }
}
```

**Persist-then-build sequence:**

1. Serialize the full `DelegationResult` (status + diff + summary + diff_summary + worker_branch + artifact + estimated_cost_usd) to JSON bytes.
2. Call `store.put(&key, &bytes, &metadata).await`.
3. On success: build lean `BrainContinuation` with `artifact_id: Some(key)` and a `fetch_hint`.
4. On failure: emit `tracing::error!(target: "spur.metrics.outcome_persist_failed", ...)`; fall through to truncation-ladder fallback (Plan-4 spec §6 unchanged); produce a fitted `BrainContinuation` with `artifact_id: None`.

The fallback ensures INV-α holds even when the store is unavailable.

### 7.3 Extended `fetch_outcome_artifact` (with section pagination)

Phase 3 adds the `section` parameter (per gemini's recommendation):

```rust
// JSON-RPC tool args:
// {
//   "delegation_id": String,
//   "section": Option<"status_only" | "summary" | "diff_only" | "full">  // default "full"
// }
```

Section semantics:
- `status_only` — just `{ status, attempt, brain_session, estimated_cost_usd }` (~100 B).
- `summary` — adds full `summary` field (no cap).
- `diff_only` — adds full `diff` text + `diff_summary` with file list.
- `full` — entire `DelegationResult`.

Brain calls the right section to avoid context bloat (gemini's concern about deferred context exhaustion). The MCP tool reads from `OutcomeStore::get` with the matching `Section` arg.

### 7.4 GC integration

The orchestrator gains a session-terminate hook:

```rust
// in scheduler.rs end-of-session path
self.outcome_store.delete_namespace(&brain_session_id).await?;
```

On orchestrator startup:

```rust
// in spawn-time setup
let ttl_days = std::env::var("SPUR_OUTCOME_TTL_DAYS").ok()
    .and_then(|s| s.parse().ok()).unwrap_or(7);
let _report = self.outcome_store
    .sweep_older_than(Duration::from_days(ttl_days)).await?;
```

Manual ops escape: `spur gc outcomes [--dry-run] [--older-than=30d]` CLI subcommand (in `spur-cli`). Adds a wrapper around `OutcomeStore::sweep_older_than`.

### 7.5 Truncation-ladder fallback (preserved from Plan-4)

When `OutcomeMaterializer::materialize` fails to persist, it falls back to the truncation ladder defined in the superseded Plan-4 spec (`docs/superpowers/specs/2026-04-24-brain-continuation-producer-envelope-fit-design.md` §6). The full ladder definition is NOT re-stated here; reader is referred to that spec.

Key requirements preserved from Plan-4:
- Steps 0a–0g + 1–4 + step 5 emergency re-clip + step 6 release-mode fallback (drop `artifact_ref`).
- INV-D9 schema-evolution guard (proptest with exhaustive `arb_delegation_status`).
- All `ContinuationFieldTruncated` events emitted via `event_sink`.

**Bit-rot guard (per gemini's failure-mode coverage):** the proptest harness uses a `MockFailingOutcomeStore` (returns `StoreError::Backend(...)` from `put`) to force the fallback path on every CI run. Without this, the ladder is rarely exercised in practice and would bit-rot.

## 8. GC and lifecycle policy

### 8.1 Session-scoped namespace (primary)

Artifacts are written under `<brain_session_id>/<delegation_id>/<attempt>` paths. When the brain session terminates (success, abort, timeout), the entire namespace is dropped via `OutcomeStore::delete_namespace`.

Session-terminate triggers:
- Orchestrator-driven session close (graceful).
- Brain-issued session abort.
- Scheduler timeout cleanup.

### 8.2 TTL backup (crash recovery)

If the orchestrator crashes mid-session, the namespace persists. On restart:

1. Sweep any namespace whose newest artifact mtime is older than `SPUR_OUTCOME_TTL_DAYS` (default 7).
2. Emit `tracing::info!(target: "spur.metrics.outcome_swept", ...)` with namespace count.

This handles ungraceful crashes without explicit reference counting (rejected by all reviewers as over-engineered).

### 8.3 Manual operator escape

```
spur gc outcomes [--dry-run] [--older-than=30d] [--namespace=<session_id>]
```

Lists or removes namespaces matching criteria. Useful for ops emergencies (disk pressure, debug cleanup).

### 8.4 Existing `refs/spur/artifacts/*` debt

The current git-blob path accumulates refs without cleanup. The unified GC sweeper handles them too: when `GitBlobOutcomeStore::sweep_older_than` runs, it checks `refs/spur/artifacts/<session>` mtimes and prunes accordingly.

**Net win:** existing debt that pre-dates Plan-5 gets cleaned up by the same mechanism.

## 9. Failure modes

### 9.1 Atomicity gap: persist+enqueue (accepted, documented)

Sequence: persist artifact → build lean continuation → enqueue → deliver.

If the orchestrator crashes between persist and enqueue, the artifact is stored but no continuation reaches the brain. Two options were considered:

- **(a) Transactional persist+enqueue** — would require disk-backed scheduler state. Heavy.
- **(b) Accept the loss** — orchestrator restart already drops in-memory scheduler state per v3.1's INV-D1 boundary.

**Decision: (b).** Consistent with current invariants. The TTL sweeper eventually GC's the orphan artifact. Document this as a known boundary in the spec; do not engineer around it.

### 9.2 Persist failure → truncation fallback

If `OutcomeStore::put` fails (disk full, IO error, git failure), the materializer falls through to the Plan-4 truncation ladder. INV-α holds; brain still receives a continuation (possibly lossy but not dropped).

CI must exercise this path on every run via `MockFailingOutcomeStore`.

### 9.3 Authorization mismatch on fetch

If the brain calls `fetch_outcome_artifact` with a `delegation_id` belonging to a different `brain_session_id`, the tool returns `StoreError::Unauthorized`. The brain receives a clean error, not silent failure.

### 9.4 LLM cache invalidation cost (documented trade-off)

Anthropic prompt caching has 5-minute TTL, content-addressed. With lean continuations:

- **"Skim many continuations" workflows:** less inline content cached → pure win.
- **"Review then act" workflows (brain calls fetch):** the fetch tool result becomes part of the next prompt, invalidating cache for that turn. Cost is paid every turn the fetch is in prompt history.

Net: depends on workflow. Most workflows skim more than they fetch. Documented; not optimized further.

### 9.5 Bounded artifact size (per kimi MISS)

Existing `WorkerArtifact` git blobs are capped at 512 KiB (`SPUR_ARTIFACT_MAX_BYTES` default in `crates/spur-worktree/src/artifact.rs:21`). The new `FsOutcomeStore` adopts the same cap by default. Truncation marker is appended when content is clipped to fit the cap.

**Spec is honest about this:** "Artifact store preserves bounded full content" — not "preserves unlimited content." Operators tune `SPUR_OUTCOME_MAX_BYTES` if larger payloads are required.

## 10. Observability

### 10.1 Structured tracing events

| Target | Level | Source | Trigger |
|---|---|---|---|
| `spur.metrics.outcome_persisted` | INFO | `OutcomeMaterializer` | Successful put. Fields: `key, byte_size, sha256, backend, latency_ms` |
| `spur.metrics.outcome_persist_failed` | ERROR | `OutcomeMaterializer` | Put failure → fallback path engaged. Fields: `key, error, fallback_engaged` |
| `spur.metrics.outcome_fetched` | INFO | `fetch_outcome_artifact` | Successful get. Fields: `key, section, byte_size, latency_ms, brain_session` |
| `spur.metrics.outcome_fetch_unauthorized` | WARN | `fetch_outcome_artifact` | Cross-session attempt rejected. Fields: `requested_session, actual_session, delegation_id` |
| `spur.metrics.outcome_namespace_deleted` | INFO | GC sweeper | On session terminate or TTL sweep. Fields: `brain_session_id, artifact_count, total_bytes` |
| `spur.metrics.outcome_swept` | INFO | Startup sweeper | TTL-based cleanup. Fields: `namespaces_swept, total_bytes_freed, ttl_days` |
| `spur.metrics.continuation_dropped_oversized` | ERROR | merger fallback | Should be unreachable post-Plan-5; ops alert if non-zero |

Ops stacks (Loki, Vector, OTel) scrape `target = "spur.metrics.*"` to derive counters. No new workspace dep.

### 10.2 TUI surfacing

`crates/spur-tui/src/views/session_detail.rs` gains a render branch for `SpurEventBody::ContinuationDropped { reason: OversizedSingleItem, .. }`:

```rust
format!("✖ PRODUCER-BUG: Continuation {} dropped — cost {}B > budget {}B",
    delegation_id, continuation_bytes, budget_bytes)
```

Distinct severity from routine `⚠ Continuation dropped` (StaleSession, OverflowFull, etc.).

### 10.3 New `BrainContinuation` event source: artifact-driven

When `OutcomeMaterializer` persists successfully, the lean continuation is emitted with `artifact_id: Some(_)`. TUI should surface this as "outcome stored at <ref>" rather than truncating-display the headline.

## 11. Migration / rollback per phase

### Phase 1
- Schema: additive (`Option<String>` fields on `ArtifactRef`). Backward-compatible.
- New MCP tool: independent. Old brains ignore it.
- Rollback: single revert restores pre-fix mapping (drops the metadata again, but brains haven't been using it).

### Phase 2
- New crate. Existing `worktrees.persist_artifact` retained during transition.
- Orchestrator call site uses trait wrapper but git-blob backend behaviorally identical.
- Rollback: orchestrator switches back to direct `worktrees.persist_artifact`. Crate stays in tree but unused.

### Phase 3
- Schema bump: `2 → 3`. Wire-format-compatible (additive optional fields). Old brains see `artifact_id` and `fetch_hint` as unknown-but-ignored.
- Rollback: orchestrator wires `OutcomeMaterializer` back to a no-op materializer that builds full continuations inline. Trait abstraction stays in place.
- Plan-4 truncation ladder remains as fallback regardless.

## 12. Plan-4 supersession notice

The prior spec at `docs/superpowers/specs/2026-04-24-brain-continuation-producer-envelope-fit-design.md` is **superseded as the primary path** by this spec. The user's explicit choice ("artifact store, long term") promotes Plan-5 to primary; Plan-4's truncation-at-producer becomes the artifact-write-failure fallback only.

**What survives from Plan-4:**
- Truncation ladder (steps 0a–0g + 1–6) — preserved verbatim, demoted to fallback role.
- INV-D9 (schema-evolution guard) — same proptest.
- `effective_merge_budget()` env var wiring — orthogonal, valuable.
- Observability deltas (TUI prefix, structured tracing target) — valuable independent.

**What's discarded:**
- Producer-side primary fit path — replaced by `OutcomeMaterializer`.
- Per-step envelope arithmetic proofs — moot with lean handle (always small by construction).
- Drain-time re-fit safety net — moot for the same reason.

The Plan-4 spec file gets a header amendment:

```markdown
> **STATUS UPDATE (2026-04-25):** Superseded as primary by
> `2026-04-25-brain-continuation-artifact-store-design.md`.
> The truncation ladder defined here survives as the artifact-write-failure
> fallback referenced from §7.5 of the superseding spec.
```

## 13. Future integration with `spur-context` (2026-04-13 spec)

The 2026-04-13 spec at `docs/superpowers/specs/2026-04-13-spur-context-engine-design.md` describes an approved (not-yet-implemented) DuckDB-backed unified context engine — orchestrator memory with `decisions`, `observations`, knowledge graph (Phase 2), and vector embeddings (Phase 3).

**Cross-check status:** The shipped `crates/spur-context` is a *different scope* (cost analytics). The 2026-04-13 spec's Phase 1 (decisions + observations tables) has not been implemented at the time of this writing.

**Composition path (when ContextEngine Phase 1 lands):**

| Concern | spur-blob-store | spur-context (specced) |
|---|---|---|
| Access pattern | Per-delegation in-flight fetch; write-once-read-rarely | Cross-session recall + analytics |
| Lifetime | Session-scoped + TTL (days) | Long-term retention (months) |
| Storage shape | Content-addressed blobs (sha256 dedup) | Relational rows + JSON metadata |
| Brain usage | "Fetch THIS outcome's full diff" | "Find similar past decisions" |

**They are orthogonal — no duplication if designed to compose.**

**Integration hook (for ContextEngine spec author to consume):**

- `OutcomeKey { brain_session_id, delegation_id, attempt }` aligns 1:1 with what `observations.session_id` + `observations.decision_id` will reference.
- `OutcomeRef { key, sha256, byte_size, backend }` is JSON-serializable. ContextEngine's `observations.artifacts_json` field (per 2026-04-13 spec line 436) is "a JSON array of file paths, diffs, or other artifacts." A blob ref slots in cleanly.
- `observations.content` stays as the inline summary text (short); `observations.artifacts_json` carries blob references for full payload retrieval via `OutcomeStore::get`.
- Future Phase 2+ MCP tools (`recall_context`, `query_history` per ContextEngine spec) can join on blob refs to retrieve full historical payloads.

When ContextEngine Phase 1 ships, no spur-blob-store changes are required. The composition is additive: ContextEngine becomes a *consumer* of OutcomeStore for retrieving historical artifacts during recall queries.

## 14. Review history

| Round | Reviewer | Date | Verdict | Key contributions |
|---|---|---|---|---|
| 1 | codex | 2026-04-24 | — | 5 MUST-FIX (crate cycle, unbounded status strings, env wiring, invariant α over-claim, test privacy). Plan-4 deltas Δ1–Δ5. |
| 2 | codex | 2026-04-24 | APPROVE-WITH-CHANGES | 5 SHOULD-FIX + 3 NITS + 5 open questions. Plan-4 deltas Δ6–Δ11. |
| 2.5 | self (L9 first-principles) | 2026-04-24 | — | Envelope arithmetic, budget drift, schema-evolution guard. Plan-4 deltas Δ12–Δ15. |
| 3 | kimi | 2026-04-24 | APPROVE-WITH-CHANGES | 5 MUST-FIX + 4 SHOULD-FIX. Plan-4 deltas Δ16–Δ24. |
| 3.5 | self (spec-time) | 2026-04-24 | — | `ArtifactKind::Other`/`sha256` clip. Plan-4 deltas Δ25–Δ29. |
| 4 | kimi | 2026-04-25 | RECONSIDER | JSON-escape arithmetic defect; spec_v.JSON-cost mismatch. Triggered architectural pivot. |
| 5a | kimi | 2026-04-25 | PROCEED-WITH-CHANGES | Identified existing `WorkerArtifact` subsumption; refcount-on-`delivered_ids` rejected; 768B headline. |
| 5b | codex | 2026-04-25 | PROCEED-WITH-CHANGES | `map_worker_artifact_ref` metadata loss bug (MF1); ContinuationPayload missing `estimated_cost_usd` (MF3); schema_version → 3 (MF4); session-scoped GC (gemini-aligned). |
| 5c | gemini | 2026-04-25 | PROCEED-WITH-CHANGES | Polymorphic Q2c rejected → flat-stable; explicit `fetch_hint`; `MockFailingOutcomeStore` for CI; section-paginated fetch tool. |
| 5.5 | self (L9 + spur-context cross-check) | 2026-04-25 | — | Phase 1 brain-visible refinement (F1); persist-enqueue gap accepted (F2); separate `spur-blob-store` crate; ContextEngine integration hook. |

## 15. Appendix

### 15.1 Crate file layout (Phase 2 deliverable)

```
crates/spur-blob-store/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs
    ├── trait_def.rs       # OutcomeStore trait
    ├── types.rs           # OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError
    ├── fs_store.rs        # FsOutcomeStore (default for new outcomes)
    ├── memory_store.rs    # MemoryOutcomeStore (test/dev)
    └── measured.rs        # MeasuredOutcomeStore<S> tracing decorator
```

### 15.2 Updated workspace dependency graph

```
                       spur-blob-store ◀── NEW
                       ┌───────┬───────┐
                       ↓       ↓       ↓
            spur-worktree  spur-mcp  spur-core
                ↓             ↓        ↓
              spur-acp ◀─ depends on ──┘
                ↓
        agent-client-protocol
```

No cycles. `spur-blob-store` is a sibling leaf to `spur-acp`, depending only on it for typed identifiers.

### 15.3 Configuration surface

| Env var | Default | Description |
|---|---|---|
| `SPUR_OUTCOME_TTL_DAYS` | `7` | Crash-recovery sweep TTL. |
| `SPUR_OUTCOME_MAX_BYTES` | `524_288` | Per-outcome stored cap (512 KiB; matches existing `SPUR_ARTIFACT_MAX_BYTES`). |
| `SPUR_DATA_DIR` | platform-specific via `directories` crate | Root for `<root>/outcomes/`. |
| `SPUR_MERGE_BUDGET_BYTES` | `8192` (post-hotfix) | Merge envelope budget. |

### 15.4 Implementation plan handoff

The spec is structured for ingestion by `superpowers:writing-plans`. The expected plan structure:

- **Phase 1** (1–2 weeks): metadata-preservation fix + minimal MCP fetch tool + tests.
- **Phase 2** (2–3 weeks): `spur-blob-store` crate, FS impl, git-blob impl wrapping, migration of existing call site, MockFailingOutcomeStore, full test suite.
- **Phase 3** (2–3 weeks): `OutcomeMaterializer`, lean schema v3, extended fetch tool, GC integration, fallback exercise tests, end-to-end scenarios.

Each phase should be a separate beads epic with internal task DAG. Phase 1 and Phase 2 are sequential (Phase 2's git-blob impl wraps Phase 1's metadata-preservation work). Phase 3 depends on Phase 2.

### 15.5 Open questions deferred to implementation

These should be resolved during plan-writing or implementation, not in this spec:

- **Q1:** Exact JSON-RPC schema for `fetch_outcome_artifact` Phase 1 vs. Phase 3 — backwards-compatible evolution path.
- **Q2:** `Section` enum precise boundaries (e.g., does `Section::DiffOnly` include diff_summary metadata, or just diff text?).
- **Q3:** Concrete TTL sweep cadence beyond startup (interval-based background sweep, or on-demand only?).
- **Q4:** `MeasuredOutcomeStore` decorator default-on or feature-gated?
