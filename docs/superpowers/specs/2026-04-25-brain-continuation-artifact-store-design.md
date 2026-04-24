# Brain continuation — artifact-store-at-orchestrator design

- **Date:** 2026-04-25
- **Status:** Draft (ready for human review)
- **Authors:** Kevin Truong (kevin.truong.ds@gmail.com), with Claude Opus 4.7 as design pair
- **Reviewers:** codex (rounds 1–2, 5b, 6a, 7), kimi (rounds 3–4, 5a, 6b), gemini (rounds 5c, 7)
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
Worker completes (TWO entry paths — see §7.3)
  ① Direct delegate_to_worker → spur-mcp::server::build_detached_continuation
  ② Plan-mode submit_plan/execute_epic → spur-mcp::plan::reconciler
                            │
                            ▼ both paths invoke
       ┌────────────────────────────────────────────┐
       │ OutcomeMaterializer (in spur-mcp)          │
       │ uses OutcomeStore trait (in spur-blob-store)│
       │ + clip_status_strings + clip_diff_files    │
       │   (Plan-4 helpers, run unconditionally on  │
       │    every success path — INV-D8 by clip)    │
       └────────────────────┬───────────────────────┘
                            │
                            ├── 1. Persist FULL DelegationResult to OutcomeStore
                            │    (key: brain_session/delegation/attempt; sha256 dedup)
                            │
                            ├── 2. Build LEAN BrainContinuation v3:
                            │    - clipped_status (DelegationStatus with bounded inline strings),
                            │      summary (capped 512B), diff_stats (counts only, no file list),
                            │      worker_branch (capped 256B), estimated_cost_micros (u64),
                            │      artifact_id (Some if backing artifact exists),
                            │      fetch_hint (explicit recovery instruction, capped 256B)
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

### INV-D8 (this spec, enforced-by-clip)

> For every `BrainContinuation` produced by `OutcomeMaterializer`, the envelope satisfies `continuation_cost_bytes(cont) ≤ MERGE_BUDGET_DEFAULT_BYTES`. Enforcement is procedural: the materializer is the **single producer** of `BrainContinuation` on the success path, and it unconditionally calls `clip_status_strings` + `clip_diff_files` (Plan-4 helpers) before constructing the lean payload — regardless of whether persistence succeeded.

**Why "enforced-by-clip" not "by construction":** the wire types `DelegationStatus` and `DiffSummary` carry unbounded fields (`Failed.error: String`, `Conflict.files: Vec<PathBuf>`, `diff_summary.files: Vec<PathBuf>`, etc.) for the artifact-store path's full-fidelity persistence. The lean continuation reuses these types so the brain sees a familiar shape, but every variant's String/Vec fields are clipped to a fixed budget at the materializer boundary.

**Bound:** clipped status + clipped diff_summary (counts + capped file list, max 16 entries × 128 B paths) + capped summary (512 B) + capped worker_branch (256 B) + capped fetch_hint (256 B) + ArtifactRef (~400 B) + OutcomeKey (~200 B) ≤ ~3.5 KB inline, comfortably under `MERGE_BUDGET_DEFAULT_BYTES = 8192`.

**Enforcement test** (in `crates/spur-mcp/tests/`): proptest with `arb_delegation_status` (every variant with adversarial-large strings) → call `OutcomeMaterializer::materialize` → assert `continuation_cost_bytes(cont) ≤ MERGE_BUDGET_DEFAULT_BYTES` for 1024 cases. **CI gate.** Co-located with INV-D9's exhaustive-match proptest.

The merger's `OversizedSingleItem` branch becomes unreachable for `OutcomeMaterializer`-produced continuations. Other producers (none expected post-Plan-5) remain subject to the merger fallback.

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

**Phase 1 scope:** read-only access to the existing git-blob-backed `WorkerArtifact` payloads.

**SF9 (round 6) — backport `section` parameter to Phase 1.** Original phasing deferred section pagination to Phase 3, but kimi flagged the sharp edge: a brain calling `fetch_outcome_artifact` against a 512 KiB stdout blob blows its own context. Cheap fix — add the `section` parameter in Phase 1 with a single supported value `Full` (current behavior). Phase 3 widens the supported sections (`StatusOnly`, `Summary`, `DiffOnly`). The wire schema is forward-compatible:

```rust
// Phase 1 args: { "delegation_id": String, "section": Option<"full"> }   // default "full"
// Phase 3 args: { "delegation_id": String, "section": Option<...all sections...> }
```

Brains running against Phase 1 see `section=full` only; Phase 3 brains gain pagination. No breaking change between phases.

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

### 6.3 Public API — type ownership split (MF1: avoids spur-acp ↔ spur-blob-store cycle)

The wire-shape types (`OutcomeKey`, `OutcomeRef`, `BackendTag`) live in **`spur-acp/src/domain/outcome.rs`** so that `ContinuationPayload.artifact_id: Option<OutcomeKey>` can reference them without forcing `spur-acp → spur-blob-store`. The trait, store-only types, and impls stay in `spur-blob-store`.

**Owned by `spur-acp/src/domain/outcome.rs` (NEW module):**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutcomeKey {
    pub brain_session_id: BrainSessionId,
    pub delegation_id: DelegationId,
    pub attempt: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRef {
    pub key: OutcomeKey,
    pub sha256: String,                       // 64-char hex
    pub byte_size: u64,
    pub backend: BackendTag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendTag {
    Fs,
    GitBlob,
    // Future: S3, GCS, ...
}
```

**Owned by `spur-blob-store` (trait + store-internal types):**

```rust
// trait_def.rs
use spur_acp::domain::outcome::{OutcomeKey, OutcomeRef, BackendTag};

#[async_trait::async_trait]
pub trait OutcomeStore: Send + Sync {
    /// Idempotent: same key + same content → same OutcomeRef, no rewrite.
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError>;

    /// Retrieve content by key, optionally narrowed to a section.
    async fn get(
        &self,
        key: &OutcomeKey,
        section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError>;

    /// Drop all artifacts for a brain_session. Called on session terminate.
    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError>;

    /// Sweep namespaces whose newest artifact is older than `ttl`.
    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError>;
}

// types.rs (store-internal only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeMetadata {
    pub created_at: DateTime<Utc>,
    pub content_type: ContentType,           // Diff | Stdout | Stderr | Json
    pub original_byte_size: u64,
    pub stored_byte_size: u64,                // post-truncation if applicable
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

**MF4 fix (round 6) — backcompat adapter for `DelegationResult.artifact`:**

The existing orchestrator at `crates/spur-core/src/orchestrator.rs:4755-4773` consumes `Result<WorkerArtifact, String>` and stores into `DelegationResult.artifact: Option<WorkerArtifact>`. Phase 2's switch to `OutcomeStore::put -> OutcomeRef` must preserve that shape. Adapter lives in `spur-acp/src/domain/outcome.rs` (next to `OutcomeRef`):

```rust
// spur-acp/src/domain/outcome.rs
impl OutcomeRef {
    /// Backcompat adapter: project a GitBlob-backed OutcomeRef into the
    /// legacy WorkerArtifact shape. Returns None for non-git backends.
    /// Phase 2 callers use this to preserve DelegationResult.artifact behavior
    /// during transition; Phase 3 cleanup may remove or deprecate.
    pub fn as_worker_artifact(&self, kind: WorkerArtifactKind) -> Option<WorkerArtifact> {
        match self.backend {
            BackendTag::GitBlob => Some(WorkerArtifact {
                object_ref: format!("refs/spur/artifacts/{}", self.key.brain_session_id),
                blob_sha: self.sha256.clone(),
                size_bytes: self.byte_size as usize,
                kind,
            }),
            _ => None,
        }
    }
}
```

Orchestrator call site at `:4755` becomes:
```rust
let outcome_ref = store.put(&key, output_text.as_bytes(), &metadata).await?;
let worker_artifact = outcome_ref.as_worker_artifact(legacy_kind);
// store into DelegationResult.artifact as before — observable behavior preserved
```

Phase 2 tests verify byte-exact equivalence between the wrapped path and the direct `worktrees.persist_artifact` path for the existing call sites.

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

**MF5 (round 6) — coexistence of `artifact_ref` and `artifact_id`:** the existing `artifact_ref: Option<ArtifactRef>` field (currently at `crates/spur-acp/src/domain/continuation.rs:67-68`, narrow scope = oversized worker stdout via git blob) and the new `artifact_id: Option<OutcomeKey>` field (broad scope = full delegation outcome) **coexist during transition**. Semantically `artifact_id` supersedes `artifact_ref`; for backward compat, both are populated where applicable. Phase 1's `artifact_ref` enrichment (`git_object_ref`, `git_blob_sha`) keeps working; Phase 3 brains check `artifact_id` first, fall back to `artifact_ref`. Cleanup of `artifact_ref` deferred to a future release after one stabilization cycle.

```rust
// in spur-acp/src/domain/continuation.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationPayload {
    /// Inline status. Materializer applies `clip_status_strings` so that all
    /// String / Vec<PathBuf> fields inside variants (Failed.error, Conflict.files,
    /// Rejected.reason, Modified.reviewer_note, Cancelled.reason,
    /// TimedOut.fallback.Reject.reason) are bounded at the materializer boundary.
    /// Full unclipped status lives in OutcomeStore; brain fetches via section='status_only'.
    pub status: DelegationStatus,
    /// Always inline. Capped at 512 B with "…" sentinel if longer.
    pub summary: Option<String>,
    /// Inline diff summary. Materializer applies `clip_diff_files` so that
    /// `files: Vec<PathBuf>` is capped at 16 entries × 128 B each.
    /// Full file list via fetch_outcome_artifact(section='diff_only').
    pub diff_summary: Option<DiffSummary>,
    /// Always inline. Capped at 256 B.
    pub worker_branch: Option<String>,
    /// NEW (round 8 / MF3) — cost in micro-USD (1e-6 USD).
    /// `u64` chosen over `f64` so `ContinuationPayload` keeps deriving `Eq`
    /// (f64 does not impl Eq). Brain converts to display USD as needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_micros: Option<u64>,
    /// EXISTING (Phase 1 enriched) — reference to oversized stdout artifact (legacy narrow scope).
    /// Coexists with artifact_id during transition; deprecated after stabilization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ArtifactRef>,
    /// NEW (Phase 3) — reference to full delegation outcome in OutcomeStore.
    /// Some(_) ⇒ brain may call fetch_outcome_artifact for fuller context.
    /// Brains check artifact_id FIRST, fall back to artifact_ref if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<OutcomeKey>,
    /// Explicit human-readable hint when artifact_id is Some.
    /// Capped at 256 B. Example:
    /// "Full diff truncated. Call fetch_outcome_artifact(delegation_id, section='diff_only')."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hint: Option<String>,
}
```

**Round 8 (MF2/MF3) — design changes:**

- **Clipping is mandatory on every materializer entrypoint, not just the truncation-fallback path.** The success path persists the full `DelegationResult` to OutcomeStore (preserves fidelity for brain-fetch), then constructs the lean continuation by **calling the same Plan-4 clip helpers** that the fallback path uses. INV-D8 holds because the materializer is the single producer (§7.2). The Plan-4 truncation ladder remains the persist-failure fallback only (§7.7).
- **`estimated_cost_micros: Option<u64>`** replaces the round-6 `estimated_cost_usd: Option<f64>`. Rationale: `f64` does not implement `Eq`, which would break `#[derive(Eq)]` on `ContinuationPayload` (used by `DelegationKey`'s eq/hash). Cost is fundamentally integer (LLM token pricing is denominated in fractions of cents); micros gives 6-digit precision without floats. Conversion is a trivial `cost_usd = cost_micros as f64 / 1_000_000.0` at display time.

**Why keep DelegationStatus inline rather than introduce a `LeanStatus` enum:**

A round-7 alternative considered was replacing `status: DelegationStatus` with a structurally bounded `LeanStatus { Success, Failed, Conflict, ... }` (no inline String fields). This would make INV-D8 "true by construction" (no clipping needed). Rejected for two reasons:

1. **Variant duplication.** Every new variant requires double-write (DelegationStatus + LeanStatus), inviting drift.
2. **Brain UX regression.** Brains read `Failed.error` / `Rejected.reason` as inline signal *all the time* — not just when fetching the artifact. A LeanStatus that carried zero error context would force a fetch on every failure, defeating the lean payload's purpose.

Clipping at the materializer boundary preserves brain UX (clipped error string is still the most important inline signal) while bounding the envelope. INV-D9's exhaustive-match proptest enforces that every new variant gets a clipping rule, eliminating bit-rot risk.

**Schema bump rationale (per codex MF4):** ops-debug visibility, not wire-protocol break. LLMs are tolerant of optional fields via serde defaults; this bump is for log/dashboard correlation when investigating brain behavior pre/post Plan-5.

**Renderer change:** `crates/spur-core/src/continuation_bridge.rs:149` updates `ContinuationResourceBody.schema_version` constant from `2` to `3`.

**SF6 (round 6) — `ContinuationPayload` direct-construction sites that need updating** when `estimated_cost_micros`, `artifact_id`, and `fetch_hint` are added (all are additive `Option<>` with `#[serde(default)]`, but Rust struct literals must still include the new fields):

- `crates/spur-acp/src/domain/continuation.rs:184-231` — round-trip serialization test.
- `crates/spur-acp/src/domain/continuation.rs:240-269` — `delegation_key_equality_and_hashing_use_attempt` test (constructs `ContinuationPayload` literals).
- `crates/spur-mcp/src/server.rs:273` — `build_detached_continuation` (production callsite).
- Plan-writers must `grep -rn 'ContinuationPayload {' crates/` after Phase 3 lands to catch any newly-added construction sites and update them.

### 7.2 `OutcomeMaterializer` (in spur-mcp)

**Round 8 (MF1) — relocated from spur-core to spur-mcp.** The round-6 spec placed the materializer in `spur-core`, but `spur-core` already depends on `spur-mcp` (existing edge: `crates/spur-core/Cargo.toml`); wiring the materializer into `spur-mcp` callsites (§7.3) would have forced `spur-mcp → spur-core`, creating a cycle.

`spur-mcp` is the natural home: both completion callsites (`server.rs::build_detached_continuation` and `plan/mod.rs::persist_completion_result_and_notify`) live there, `McpEventSink` is defined there, and `spur-mcp` already depends on `spur-acp` (for `DelegationResult`/`BrainContinuation`/`OutcomeKey`) and on `spur-worktree`. Phase 2 adds `spur-mcp → spur-blob-store` as one new workspace dep. The orchestrator at `spur-core/orchestrator.rs:4755` reaches the materializer through its existing `spur-core → spur-mcp` edge. No cycles.

```rust
// crates/spur-mcp/src/outcome_materializer.rs (new module)
use spur_acp::domain::{ContinuationPayload, DelegationResult, OutcomeKey};
use spur_blob_store::{OutcomeStore, OutcomeMetadata, ContentType};

pub struct OutcomeMaterializer {
    store: Arc<dyn OutcomeStore>,
    summary_cap_bytes: usize,        // = 512
    worker_branch_cap_bytes: usize,  // = 256
    fetch_hint_cap_bytes: usize,     // = 256
    diff_files_cap_count: usize,     // = 16  (per-file path cap = 128 B)
    status_string_cap_bytes: usize,  // = 512 (Failed.error, Rejected.reason, etc.)
}

impl OutcomeMaterializer {
    /// Single entrypoint. Both callsites (§7.3) have full `DelegationResult`
    /// available at runtime — the round-6 dual-entrypoint design was based
    /// on a stale read of reconciler.rs:409 (rx.await yields full result).
    pub async fn materialize(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation { /* see persist-then-clip-then-build below */ }
}
```

**Persist-then-clip-then-build sequence (success path):**

1. **Build OutcomeKey** from `(brain_session, delegation_id, attempt)`.
2. **Serialize** the full `DelegationResult` (untouched, full fidelity) to JSON bytes.
3. **Store.put** — `store.put(&key, &bytes, &metadata).await`.
4. **On store success**: clip a *copy* of the relevant fields (do NOT mutate the persisted full version):
   - `clipped_status = clip_status_strings(&result.status, status_string_cap_bytes)`
   - `clipped_diff = result.diff_summary.as_ref().map(|d| clip_diff_files(d, diff_files_cap_count))`
   - `clipped_summary = clip(&result.summary, summary_cap_bytes)` with "…" sentinel
   - `clipped_branch = clip(&result.worker_branch, worker_branch_cap_bytes)`
   - `fetch_hint = build_fetch_hint(&clipped_status, &clipped_diff)` (≤ 256 B)
5. **Build lean `BrainContinuation`** with `artifact_id: Some(key)`, `artifact_ref: legacy_artifact_ref(&result)` (Phase 1 backcompat), and the clipped fields above.
6. **Debug-assert envelope size** (panic in test/debug, INV-D9 proptest catches violations in CI):
   ```rust
   debug_assert!(continuation_cost_bytes(&cont) <= MERGE_BUDGET_DEFAULT_BYTES);
   ```
7. **Emit telemetry** — `tracing::info!(target: "spur.metrics.outcome_persisted", ...)`.

**On store failure** (persist-then-clip-then-build aborts at step 3):

1. Emit `tracing::error!(target: "spur.metrics.outcome_persist_failed", ...)`.
2. **Fall through to the Plan-4 truncation-ladder fallback** (§7.7). The ladder runs its own clipping + emergency steps and produces a fitted `BrainContinuation` with `artifact_id: None`.

INV-α holds either way. The success path's clipping helpers are the *same functions* the fallback uses; one set of clip rules to maintain.

**Clipping helpers — moved from spur-core to spur-acp::domain::clip (round 8):**

The clip helpers (`clip_status_strings`, `clip_diff_files`) are needed by both the materializer (in spur-mcp) and the truncation-ladder fallback. Round-6's spec placed them in spur-core. Round 8 moves them to `spur-acp::domain::clip` so both consumers (spur-mcp materializer + spur-core continuation_bridge fallback) can call them without a back-edge. spur-acp gains no new deps (clip helpers are pure functions over its own domain types). The Plan-4 spec §6 ladder is amended to reference `spur_acp::domain::clip::*` instead of its current local helpers.

**Persist-failure metrics** include the truncation events from the fallback, so operators see both "store failed" and "fallback engaged" in a single trace event group.

### 7.3 Materializer call sites — TWO completion paths, ONE entrypoint

`OutcomeMaterializer::materialize` is invoked from **two distinct callsites** that both signal "worker delegation completed." Both paths have the **full `DelegationResult` in scope at runtime** — round 6's design assumed the reconciler had reduced state, but inspection of `crates/spur-mcp/src/plan/reconciler.rs:409` confirms `rx.await` yields the complete `DelegationResult` before the closure forwards it to `persist_completion_result_and_notify`. **Round 8 simplifies to a single materializer entrypoint.** The reduced-fidelity `materialize_metadata_only` from round 6 is dropped.

| # | Callsite | Trigger path | State at runtime |
|---|---|---|---|
| 1 | `crates/spur-mcp/src/server.rs::build_detached_continuation` (currently line 251) | Direct `delegate_to_worker` → MCP background collector receives `DelegationResult` | full `DelegationResult`, `delegation_id`, `brain_session`, `attempt` |
| 2 | `crates/spur-mcp/src/plan/reconciler.rs:409` → `crates/spur-mcp/src/plan/mod.rs::persist_completion_result_and_notify` (currently line 998) | Reconciler-driven plan-mode (`submit_plan` / `execute_epic`); reconciler awaits the same completion `oneshot::Receiver` as callsite 1 | full `DelegationResult`, `delegation_id`, `plan_id`, `brain_session_id`, `attempt`, `completion_state` |

**State plumbing for callsite 2:**

The reconciler's `ReconcilerDispatchCtx` (at `crates/spur-mcp/src/plan/reconciler.rs:155`) already carries `brain_session_id`. `task.attempt` is in scope at the dispatch site. Both are captured into the spawned task closure that calls `persist_completion_result_and_notify`. The function signature gains a `&DelegationResult` parameter (replacing the existing `worker_branch: Option<&str>` + `result_summary: Option<&str>` pair, which become projections of the new param) plus the materializer:

```rust
// crates/spur-mcp/src/plan/mod.rs:998 — Phase 3 amended signature
pub(crate) async fn persist_completion_result_and_notify(
    pm: &dyn PmLike,
    issue_id: &str,
    plan_id: &str,
    delegation_id: &str,
    completion_state: CompletionState,
    fast_forward: &Option<Arc<tokio::sync::Notify>>,
    // NEW for Phase 3:
    result: &spur_acp::domain::DelegationResult,    // replaces worker_branch + result_summary
    brain_session_id: &spur_acp::BrainSessionId,
    attempt: u32,
    materializer: &OutcomeMaterializer,
) -> anyhow::Result<()>;
```

Production callsite at `reconciler.rs:421` passes `&result` (already in scope from `rx.await` at line 409). Test callsites in `crates/spur-mcp/tests/*` (e.g., `submit_plan_persist.rs`, `epic_completion.rs`, `reconciler_tick.rs`) construct a `DelegationResult` literal — these are easy to update because they currently construct `result_summary`/`worker_branch` literals already. Plan-writers must `grep -rn 'persist_completion_result_and_notify' crates/spur-mcp/` after Phase 3 lands.

**Fallback behavior is identical at both sites:** on `OutcomeStore::put` failure, the materializer falls through to the Plan-4 truncation-ladder fallback. Both sites emit `spur.metrics.outcome_persist_failed` via the materializer's internal telemetry path.

**Why the round-6 dual-entrypoint design was unnecessary:** the assumption was that callsite 2 saw only beads-polled metadata (status + summary + worker_branch as strings). In fact, the reconciler awaits the same `oneshot::Receiver<DelegationResult>` that callsite 1 receives — beads is the *durable audit log*, but the *runtime completion signal* travels through the same in-memory channel. Single materializer entrypoint is cleaner, easier to test, and matches the data flow.

### 7.4 Beads audit-comment composition (composition, not coupling)

The reconciler currently calls `beads.add_comment` to post `[[spur-audit v1]] Completion` audit trails carrying `result_summary` and `worker_branch` as durable, user-visible records on the beads issue (`crates/spur-pm/src/beads.rs:842-865`; comment composed in `crates/spur-mcp/src/plan/mod.rs:686-708::emit_completion_audit`). The encoder shape is `{prefix}\n{json}` (`audit_sentinel.rs:160-167`), and the parser does `serde_json::from_str` over the JSON body (`audit_sentinel.rs:172-176`). This is **orthogonal to the artifact store** — beads is the audit/state-of-record system; the blob store is the blob byte system.

**MF3 fix (round 6):** Plan-5's earlier proposal to append a raw URI line after the JSON would **break `parse_comment`** because `serde_json::from_str` rejects trailing non-JSON content. The clean fix is to add a JSON field to the `Completion` variant in `crates/spur-mcp/src/plan/audit_sentinel.rs`:

```rust
// audit_sentinel.rs — AuditSentinelKind enum, Completion variant amendment
Completion {
    delegation_id: String,
    completion_state: CompletionState,
    superseded: bool,
    worker_branch: Option<String>,
    result_summary: Option<String>,
    // NEW — Some(_) when OutcomeMaterializer succeeded; carries OutcomeKey-derived URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact_uri: Option<String>,
},
```

Reconciler populates `artifact_uri` from the `BrainContinuation.payload.artifact_id` returned by `OutcomeMaterializer::materialize` (single entrypoint per round 8 §7.3) when `Some(_)`. Field is additive + serde-default — no parser changes needed for existing comments without it.

Concrete shape (post-encode):
```
[[spur-audit v1]]
{"kind":"completion","delegation_id":"<uuid>","completion_state":"awaiting_review","superseded":false,"worker_branch":"spur/worker-foo-abc","result_summary":"<truncated>","artifact_uri":"spur://outcome/<brain_session>/<delegation>/<attempt>"}
```

Operators viewing the beads issue can extract `artifact_uri` from the JSON and resolve via `fetch_outcome_artifact`. **Additive JSON field, no encoder/parser changes beyond the variant struct field.**

### 7.5 Extended `fetch_outcome_artifact` (with section pagination + attempt)

Phase 3 adds the `section` parameter (per gemini's recommendation) and the `attempt` parameter (round 8 / codex SF8 — disambiguates retried delegations):

```rust
// JSON-RPC tool args:
// {
//   "delegation_id": String,
//   "attempt": Option<u32>,    // default = latest known attempt for this delegation
//   "section": Option<"status_only" | "summary" | "diff_only" | "full">  // default "full"
// }
```

Section semantics:
- `status_only` — just `{ status, attempt, brain_session, estimated_cost_micros }` (~100 B).
- `summary` — adds full `summary` field (no cap).
- `diff_only` — adds full `diff` text + `diff_summary` with file list.
- `full` — entire `DelegationResult`.

**Why `attempt` is needed:** `OutcomeKey { brain_session_id, delegation_id, attempt }` is the actual storage key. A delegation that retried (attempt 1 → attempt 2) has two artifacts. Without `attempt`, the tool either (a) silently returns the latest, masking earlier failures the brain may want to inspect, or (b) requires the brain to manage attempt selection through some other channel. Default behavior is "latest known attempt" so existing callers (Phase 1) continue working; Phase-3 brains can pin a specific attempt for forensic queries.

Brain calls the right section to avoid context bloat (gemini's concern about deferred context exhaustion). The MCP tool reads from `OutcomeStore::get` with the matching `Section` arg.

### 7.6 GC integration

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

### 7.7 Truncation-ladder fallback (preserved from Plan-4)

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

### 9.6 Brain-session crash auth wall (round 6 / kimi NIT)

Artifacts are scoped to `brain_session_id`; `fetch_outcome_artifact` rejects cross-session reads (per §10 + codex SF3). Consequence: if the brain process crashes and a NEW brain session starts, prior artifacts are unreadable from the new session. This is a **deliberate trade-off** prioritizing cross-session security over crash recovery — accepting brief data loss is preferable to leaking artifacts across unrelated sessions.

Operator escape (deferred to a future ops-tooling phase): `spur outcomes copy --from <session> --to <session>` CLI to migrate artifacts across sessions when needed. Not in Plan-5 scope; documented as a known boundary.

### 9.7 Concurrent fetch-during-GC race (round 6 / kimi NIT)

When `OutcomeStore::delete_namespace` runs on session terminate, an in-flight `fetch_outcome_artifact` for the same namespace may race the cleanup. Behavior differs by backend:

- **`FsOutcomeStore`**: directory walking + atomic file reads tolerate concurrent deletes (Linux/Darwin POSIX semantics — open file handles survive unlink). Fetch sees either the file or `NotFound`; both are clean outcomes.
- **`GitBlobOutcomeStore`**: `git update-ref -d <ref>` and `git cat-file -p <blob_sha>` race window. Blobs survive ref deletion until git GC (typically days), so `cat-file -p <blob_sha>` succeeds even after `update-ref -d`. The race is benign.

Operator-visible behavior: in-flight fetch during session terminate **may succeed or return `StoreError::NotFound`**; clients should retry on transient error, treat persistent `NotFound` as terminal. Spec acknowledges; no implementation work required.

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
> fallback referenced from §7.7 of the superseding spec.
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

- `OutcomeKey { brain_session_id, delegation_id, attempt }` aligns with what `observations.session_id` + `observations.decision_id` will reference. **Coordination note (round 6 / kimi):** ContextEngine's `observations` schema (2026-04-13 spec §Phase 1 Schema, line 422-430) does NOT currently include an `attempt` column. Either (a) add `attempt INTEGER NOT NULL DEFAULT 1` to `observations`, or (b) embed `attempt` in `observations.artifacts_json` payload via `OutcomeRef` serialization. Option (b) is non-breaking for Phase 1 of either spec; option (a) is cleaner long-term. **Decision deferred to ContextEngine Phase 1 implementation;** Plan-5 makes no assumption about which is chosen.
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
| 5.6 | self (reconciler + beads cross-check) | 2026-04-25 | — | Found second materializer call site at `plan/reconciler.rs::persist_completion_result_and_notify` (§7.3); beads audit-comment composition (§7.4); section renumbering 7.4→7.6, 7.5→7.7. |
| 6a | codex (round-6 parallel) | 2026-04-25 | APPROVE-WITH-CHANGES | 4 MUST-FIX: cycle on `OutcomeKey` placement (MF1), callsite-2 signature gap (MF2), beads parser regression on raw URI (MF3), `WorkerArtifact` adapter (MF4). Plus 2 SHOULD-FIX (schema struct-construction sites, citation drift `:998`→`:1166`) and 1 NIT (encoder example shape). |
| 6b | kimi (round-6 parallel) | 2026-04-25 | APPROVE-WITH-CHANGES | Round 5.6 amendment soundness: insufficient at callsite 2 (confirms codex MF2). §15.2 dep graph arrow direction reversed (SF8). `artifact_ref` vs `artifact_id` coexistence ambiguity (MF5). Phase 1 unbounded fetch sharp edge (SF9 backport). Brain-crash auth wall + concurrent fetch-during-GC race (NIT11/NIT12). ContextEngine schema `attempt` column coordination. |
| 6.5 | self (round-6 amendment fold) | 2026-04-25 | — | Folded all MF1–MF5, SF6–SF9, NIT11/NIT12 into spec; type ownership split (§6.3); dual-callsite signatures (§7.3); JSON-field beads composition (§7.4); WorkerArtifact adapter (§6.5); coexistence clarification (§7.1); construction-site list (§7.1); Phase 1 section backport (§5.2); dep graph arrow fix (§15.2); failure-mode §9.6/§9.7 added; ContextEngine coordination note (§13). |
| 7a | gemini (round-7 verification) | 2026-04-25 | REJECT-WITH-MUST-FIX | INV-D8 still false on success path: `DelegationStatus::Failed.error` and `Conflict.files`/`diff_summary.files` are unbounded inline; lean payload can carry MB-scale stderr. Round-6 "lean by construction" claim was wrong. Plus 1 SHOULD-FIX (Phase-1↔3 fetch tool content-type discontinuity) and 1 NIT (legacy WorkerArtifactKind caller note). |
| 7b | codex (round-7 verification) | 2026-04-25 | REJECT-WITH-MUST-FIX | (1) MF1 only moved key types; `OutcomeMaterializer` in spur-core still creates `spur-mcp → spur-core` cycle once §7.3 wires it into spur-mcp callsites. (2) INV-D8 still false on success path (same finding as gemini). (3) `ContinuationPayload` derives `Eq` but `estimated_cost_usd: Option<f64>` does not impl Eq → won't compile. Plus SF7/SF8 (reconciler runtime path has full `DelegationResult`; fetch tool needs `attempt`). |
| 8 | self (round-7 fold) | 2026-04-25 | — | **MF1**: relocate `OutcomeMaterializer` to spur-mcp (single hop downstream of all callers; no cycle). **MF2**: clipping is mandatory on every materializer success path — same Plan-4 helpers as fallback; INV-D8 reframed as "enforced-by-clip" (single producer; debug_assert + CI proptest). Move `clip_status_strings`/`clip_diff_files` to `spur-acp::domain::clip` so both materializer and fallback share them. **MF3**: `estimated_cost_usd: Option<f64>` → `estimated_cost_micros: Option<u64>`; restores `Eq` derive. **SF7**: drop `materialize_metadata_only`; reconciler has full `DelegationResult` from `rx.await`; single materializer entrypoint with `&DelegationResult` parameter. **SF8**: `fetch_outcome_artifact` accepts `attempt: Option<u32>` (default = latest). **NITs**: `fetch_hint` gains `#[serde(default, skip_serializing_if)]`. |

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

Arrows point from consumer → dependency (i.e. "depends on"):

```
                spur-core
                    │
                    ▼ (existing edge — orchestrator calls spur-mcp)
                spur-mcp ──────► spur-worktree
                    │                 │
                    │                 ▼
                    └──────► spur-blob-store (NEW)
                                  │
                                  ▼
                              spur-acp
                                  │
                                  ▼
                          agent-client-protocol
```

**Round 8 (MF1) — materializer location:** `OutcomeMaterializer` lives in **`spur-mcp`** (not `spur-core` as round 6 had it). This avoids the `spur-mcp → spur-core` back-edge that would have been created by exposing `&OutcomeMaterializer` (a spur-core type) to the spur-mcp callsites in `server.rs::build_detached_continuation` and `plan/mod.rs::persist_completion_result_and_notify`. With the materializer in spur-mcp, both callsites use it directly; the orchestrator at `spur-core/orchestrator.rs:4755` reaches it through the existing `spur-core → spur-mcp` edge.

**Type ownership** (post-round-8):

| Crate | Owns |
|---|---|
| `spur-acp` | `ContinuationPayload`, `BrainContinuation`, `DelegationResult`, `DelegationStatus`, `DiffSummary`, `OutcomeKey`, `OutcomeRef`, `BackendTag`, `clip_status_strings` / `clip_diff_files` (round 8 — pure helpers) |
| `spur-blob-store` (NEW Phase 2) | `OutcomeStore` trait, `OutcomeMetadata`, `Section`, `StoreError`, `FsOutcomeStore`, `MemoryOutcomeStore`, `MeasuredOutcomeStore` |
| `spur-worktree` | `GitBlobOutcomeStore` (impls trait from spur-blob-store) |
| `spur-mcp` (round 8) | `OutcomeMaterializer`, `McpEventSink`, MCP tool handlers (incl. `fetch_outcome_artifact`) |
| `spur-core` | orchestrator wiring, scheduler GC hook, continuation_bridge fallback (calls clip helpers from spur-acp) |

`spur-acp` retains its position as the leaf-domain crate. No cycles.

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
