# Brain Continuation Phase 2 — `spur-blob-store` Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a content-addressed `OutcomeStore` trait + three implementations (`MemoryOutcomeStore`, `FsOutcomeStore`, `GitBlobOutcomeStore`) and a `MeasuredOutcomeStore` decorator. Migrate the orchestrator's `worktrees.persist_artifact` call site to use the trait while preserving the legacy `WorkerArtifact` wire shape. No Phase 3 changes (lean schema v3 / `OutcomeMaterializer` / `artifact_id` are explicitly out of scope).

**Architecture:** Wire-shape types (`OutcomeKey`, `OutcomeRef`, `BackendTag`) live in `spur-acp/src/domain/outcome.rs` to avoid the `spur-acp ↔ spur-blob-store` cycle (MF1). The `OutcomeStore` trait + `Fs/Memory/Measured` impls live in a new `spur-blob-store` crate. `GitBlobOutcomeStore` lives in `spur-worktree` (which already owns git ref operations) and writes to a new `refs/spur/outcomes/<session>/<delegation>-<attempt>.{blob,meta}` namespace (Round 11 MF1+MF2 — per-(session, delegation, attempt) granularity, no D/F conflict with the legacy `refs/spur/artifacts/<session>` ref). UUID validation guards `put`. `OutcomeMetadata.sha256` is the single source of truth for `ContentMismatch` detection (Round 11 SF1).

**Tech Stack:** Rust workspace (`spur-acp`, `spur-blob-store` NEW, `spur-worktree`, `spur-core`), `async-trait`, `sha2`, `tokio::fs`, `tempfile`, `proptest`, `tracing`, git CLI (existing `run_git_capture` in spur-worktree).

**Spec:** `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §6 (Phase 2).

**Phase 1 status:** complete on main as of `2c11195` (commits `f349859..2c11195`). Phase 1 added `git_object_ref` + `git_blob_sha` to `ArtifactRef` and shipped the `fetch_outcome_artifact` MCP tool against the legacy `refs/spur/artifacts/<session>` namespace. Phase 2 introduces the new namespace alongside it; legacy ref remains read-only during transition.

---

## File Structure

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-blob-store/Cargo.toml` | Create | Crate manifest |
| `crates/spur-blob-store/src/lib.rs` | Create | Public re-exports + module declarations |
| `crates/spur-blob-store/src/types.rs` | Create | `OutcomeMetadata`, `Section`, `OutcomeContent`, `StoreError`, `SweepReport`, `ContentType` |
| `crates/spur-blob-store/src/trait_def.rs` | Create | `OutcomeStore` async trait |
| `crates/spur-blob-store/src/memory_store.rs` | Create | `MemoryOutcomeStore` impl + tests |
| `crates/spur-blob-store/src/fs_store.rs` | Create | `FsOutcomeStore` impl + tests |
| `crates/spur-blob-store/src/measured.rs` | Create | `MeasuredOutcomeStore<S>` decorator + tests |
| `crates/spur-blob-store/tests/proptest_invariants.rs` | Create | Property-based tests (256 cases) |
| `crates/spur-acp/src/domain/outcome.rs` | Create | Wire-shape types (`OutcomeKey`, `OutcomeRef`, `BackendTag`) + `as_worker_artifact` adapter |
| `crates/spur-acp/src/domain/mod.rs` | Modify | Re-export `outcome::*` |
| `crates/spur-worktree/Cargo.toml` | Modify | Add `spur-blob-store` workspace dependency |
| `crates/spur-worktree/src/lib.rs` | Modify | Add `mod git_blob_store;` |
| `crates/spur-worktree/src/git_blob_store.rs` | Create | `GitBlobOutcomeStore` impl |
| `crates/spur-worktree/tests/git_blob_store_impl.rs` | Create | Cross-crate integration test |
| `crates/spur-core/Cargo.toml` | Modify | Add `spur-blob-store` workspace dependency |
| `crates/spur-core/src/orchestrator.rs:4755-4778` | Modify | Switch `worktrees.persist_artifact` callsite to `OutcomeStore::put` via backcompat adapter |
| `Cargo.toml` (workspace root) | Modify | Add `spur-blob-store` to `members` array + workspace dep entry |

Phase 2's surface is larger than Phase 1: one new crate, ~7 new source files, ~2 modifications to existing crates, ~1 callsite migration. Each task ships independently and produces working code.

---

## Task 1: Create `spur-blob-store` crate skeleton

**Files:**
- Create: `crates/spur-blob-store/Cargo.toml`
- Create: `crates/spur-blob-store/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

**What:** Stand up the new crate as an empty library that compiles cleanly under `cargo check --workspace`. The crate has no public API yet — Tasks 2–7 fill it in. Adding the crate first is a clean checkpoint (registry, paths, manifest all wired) before substantive code lands.

- [ ] **Step 1: Create the crate directory**

Run: `mkdir -p crates/spur-blob-store/src`
Expected: directory created silently.

- [ ] **Step 2: Write `crates/spur-blob-store/Cargo.toml`**

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
chrono = { workspace = true, features = ["serde"] }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
spur-acp = { workspace = true }

[dev-dependencies]
tempfile = "3"
proptest = "1"
tokio = { version = "1", features = ["full", "test-util"] }
```

- [ ] **Step 3: Write `crates/spur-blob-store/src/lib.rs`**

```rust
//! Content-addressed outcome blob storage for SPUR delegations.
//!
//! This crate owns the [`OutcomeStore`] trait and its in-process
//! implementations (`MemoryOutcomeStore`, `FsOutcomeStore`,
//! `MeasuredOutcomeStore`). The `GitBlobOutcomeStore` impl lives in
//! `spur-worktree` because it owns git ref operations.
//!
//! The wire-shape types ([`OutcomeKey`], [`OutcomeRef`], [`BackendTag`])
//! live in `spur-acp::domain::outcome` (re-exported below) so that
//! `spur-acp::ContinuationPayload` can reference them without a circular
//! dependency on this crate.
//!
//! Spec: `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §6.

pub use spur_acp::domain::outcome::{BackendTag, OutcomeKey, OutcomeRef};

pub mod fs_store;
pub mod measured;
pub mod memory_store;
pub mod trait_def;
pub mod types;

pub use fs_store::FsOutcomeStore;
pub use measured::MeasuredOutcomeStore;
pub use memory_store::MemoryOutcomeStore;
pub use trait_def::OutcomeStore;
pub use types::{
    ContentType, OutcomeContent, OutcomeMetadata, Section, StoreError, SweepReport,
};
```

(The `pub mod` declarations all reference modules created in Tasks 3–6. We stub them in Step 4 so this Task 1 commit compiles.)

- [ ] **Step 4: Stub the empty modules so lib.rs compiles**

For each of the five files below, create them with this exact one-line content (`//! placeholder, populated in later tasks`). This keeps Task 1 a clean checkpoint.

```bash
for f in trait_def types memory_store fs_store measured; do
  echo "//! placeholder, populated in later tasks of plan-5 phase 2" \
    > "crates/spur-blob-store/src/${f}.rs"
done
```

We will need to comment out the `pub use` re-exports in `lib.rs` until those types exist, so:

Edit `crates/spur-blob-store/src/lib.rs` — replace the bottom block with:

```rust
pub mod fs_store;
pub mod measured;
pub mod memory_store;
pub mod trait_def;
pub mod types;

// Re-exports activated as types land in Tasks 3–6.
// pub use fs_store::FsOutcomeStore;
// pub use measured::MeasuredOutcomeStore;
// pub use memory_store::MemoryOutcomeStore;
// pub use trait_def::OutcomeStore;
// pub use types::{
//     ContentType, OutcomeContent, OutcomeMetadata, Section, StoreError, SweepReport,
// };
```

The `pub use spur_acp::domain::outcome::{BackendTag, OutcomeKey, OutcomeRef};` line at the top will FAIL to compile because Task 2 has not landed those types yet. Comment it out for Task 1:

```rust
// pub use spur_acp::domain::outcome::{BackendTag, OutcomeKey, OutcomeRef};
```

(Task 2 uncomments it.)

- [ ] **Step 5: Add the crate to workspace `Cargo.toml`**

Edit the workspace `Cargo.toml`. Find the `members = [...]` array and add `"crates/spur-blob-store",` immediately after `"crates/spur-acp",`:

Run: `grep -n '"crates/spur-acp"' Cargo.toml`
Expected: a single match. Use it to locate the line.

Insert AFTER that line:
```toml
    "crates/spur-blob-store",
```

Then find the `[workspace.dependencies]` table and add (alphabetical order, after `spur-acp`):

```toml
spur-blob-store = { path = "crates/spur-blob-store", version = "0.4.5" }
```

Run: `grep -n '^spur-acp ' Cargo.toml` to find the right anchor.

- [ ] **Step 6: Verify the workspace builds**

Run: `cargo check -p spur-blob-store`
Expected: clean exit. The crate has no public surface yet so this just validates the manifest.

Run: `cargo check --workspace`
Expected: clean exit. Confirms no other crate accidentally depends on the new one.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-blob-store/ Cargo.toml
git commit -m "feat(spur-blob-store): add empty crate skeleton (Phase 2)

New crate that will own the OutcomeStore trait + Fs/Memory/Measured
implementations. GitBlobOutcomeStore stays in spur-worktree (which
already owns git ref operations).

Wire-shape types (OutcomeKey, OutcomeRef, BackendTag) will land in
spur-acp::domain::outcome in Task 2 to avoid the
spur-acp ↔ spur-blob-store cycle (MF1).

Phase 2 of plan-5 brain-continuation artifact store
(docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md §6.1)."
```

---

## Task 2: Add wire-shape types to `spur-acp::domain::outcome`

**Files:**
- Create: `crates/spur-acp/src/domain/outcome.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs` (add `pub mod outcome;` and re-export)

**What:** Owns the types that cross the `spur-acp` boundary (continuation payload, persisted state, MCP wire). Putting them in `spur-acp` (not `spur-blob-store`) avoids a back-edge cycle: `spur-acp → spur-blob-store` would force the latter to depend on `spur-acp`, but `spur-blob-store` already depends on `spur-acp` for `BrainSessionId` and `DelegationId`.

- [ ] **Step 1: Read existing `domain/mod.rs` for the re-export pattern**

Run: `cat crates/spur-acp/src/domain/mod.rs`
Note the existing `pub mod` declarations and any `pub use` re-exports.

- [ ] **Step 2: Write `crates/spur-acp/src/domain/outcome.rs`**

```rust
//! Wire-shape types for SPUR's content-addressed outcome storage.
//!
//! Lives in `spur-acp` (not `spur-blob-store`) so that
//! `ContinuationPayload.artifact_id: Option<OutcomeKey>` can reference
//! these types without forcing `spur-acp` to depend on `spur-blob-store`.
//! The trait, store-only types, and impls live in `spur-blob-store` (and
//! `spur-worktree::git_blob_store` for the git backend).
//!
//! Spec: `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §6.3.

use serde::{Deserialize, Serialize};

use crate::{BrainSessionId, DelegationId};

/// Identifier for a single delegation outcome blob.
///
/// Granularity is `(brain_session, delegation, attempt)` — each retry
/// gets its own key so historical outcomes are addressable. Round 11
/// (MF1) — earlier per-session granularity caused overwrites.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutcomeKey {
    pub brain_session_id: BrainSessionId,
    pub delegation_id: DelegationId,
    pub attempt: u32,
}

/// Identifies which storage backend produced this outcome blob.
///
/// **NOT `Copy`** — Round 9 (P2-S1). Future cloud variants will need
/// to carry `String` config (region, bucket); removing `Copy` later
/// would be a breaking change. Doing it now costs one `.clone()` per use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendTag {
    Fs,
    GitBlob,
    // Future: Cloud { region: String, bucket: String }, ...
}

/// Strong reference to a stored outcome blob, returned by
/// `OutcomeStore::put`.
///
/// Carries the SHA-256 hash of the stored content (single source of
/// truth — Round 11 SF1) and the backend tag so consumers can branch
/// on backend-specific affordances (e.g., git-blob retrieval).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRef {
    pub key: OutcomeKey,
    /// 64-char lowercase hex of the stored content's SHA-256 digest.
    pub sha256: String,
    /// Size in bytes of the STORED content (post-truncation if applicable).
    pub byte_size: u64,
    pub backend: BackendTag,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionId;

    fn key() -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        }
    }

    #[test]
    fn outcome_key_round_trips_through_serde() {
        let k = key();
        let s = serde_json::to_string(&k).unwrap();
        let back: OutcomeKey = serde_json::from_str(&s).unwrap();
        assert_eq!(back, k);
    }

    #[test]
    fn outcome_ref_round_trips_through_serde() {
        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(64),
            byte_size: 1024,
            backend: BackendTag::Fs,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: OutcomeRef = serde_json::from_str(&s).unwrap();
        assert_eq!(back.sha256, r.sha256);
        assert_eq!(back.byte_size, r.byte_size);
        assert_eq!(back.backend, BackendTag::Fs);
    }

    #[test]
    fn backend_tag_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&BackendTag::Fs).unwrap(), "\"fs\"");
        assert_eq!(
            serde_json::to_string(&BackendTag::GitBlob).unwrap(),
            "\"git_blob\""
        );
    }

    #[test]
    fn outcome_key_is_hashable() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(key());
        assert!(s.contains(&key()));
    }
}
```

- [ ] **Step 3: Update `crates/spur-acp/src/domain/mod.rs`**

Add the new module declaration alongside the existing `pub mod` lines, and re-export the public types.

Run: `grep -n "pub mod" crates/spur-acp/src/domain/mod.rs | head -10`
Inspect the existing modules. Add (alphabetical placement is fine; if existing code uses a particular order, follow it):

```rust
pub mod outcome;
```

Then add a re-export line near other `pub use domain::...` blocks:

```rust
pub use outcome::{BackendTag, OutcomeKey, OutcomeRef};
```

If the existing file has both `pub mod xxx;` lines AND `pub use xxx::...;` lines, follow that convention. If only one form is used, follow that one.

- [ ] **Step 4: Run cargo check + tests on spur-acp**

Run: `cargo check -p spur-acp`
Expected: clean.

Run: `cargo test -p spur-acp --lib outcome::tests`
Expected: 4 tests pass.

- [ ] **Step 5: Uncomment the re-export in `spur-blob-store/src/lib.rs`**

Edit `crates/spur-blob-store/src/lib.rs` — uncomment:

```rust
pub use spur_acp::domain::outcome::{BackendTag, OutcomeKey, OutcomeRef};
```

Run: `cargo check -p spur-blob-store`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/outcome.rs crates/spur-acp/src/domain/mod.rs crates/spur-blob-store/src/lib.rs
git commit -m "feat(spur-acp): add OutcomeKey, OutcomeRef, BackendTag wire-shape types

New module spur_acp::domain::outcome owns the types that cross the
spur-acp boundary (continuation payload, persisted state, MCP wire).
Living in spur-acp (not spur-blob-store) avoids the
spur-acp ↔ spur-blob-store cycle (MF1).

BackendTag is intentionally NOT Copy (Round 9 P2-S1) so future cloud
variants can carry String config without breaking-change.

Phase 2 of plan-5; spec §6.3."
```

---

## Task 3: Define `OutcomeStore` trait + supporting types

**Files:**
- Modify: `crates/spur-blob-store/src/types.rs` (replace placeholder with full content)
- Modify: `crates/spur-blob-store/src/trait_def.rs` (replace placeholder with trait)
- Modify: `crates/spur-blob-store/src/lib.rs` (uncomment re-exports)

**What:** Define the trait surface and the store-internal types. No implementations yet.

- [ ] **Step 1: Write `crates/spur-blob-store/src/types.rs`**

Replace the placeholder with:

```rust
//! Store-internal types: metadata, sections, errors, sweep reports.
//!
//! These types are owned by `spur-blob-store` because they don't cross
//! the spur-acp boundary. Wire-shape types live in `spur-acp::domain::outcome`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use spur_acp::BrainSessionId;
use thiserror::Error;

use crate::{OutcomeKey, OutcomeRef};

/// What kind of payload this artifact captures. Carried in
/// `OutcomeMetadata` so consumers can branch (e.g., the materializer
/// renders diffs differently from raw stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Diff,
    Stdout,
    Stderr,
    Json,
}

/// Sidecar metadata persisted alongside each blob. Single source of
/// truth for the SHA-256 (Round 11 SF1) — `ContentMismatch` detection
/// reads this directly rather than re-hashing the stored content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeMetadata {
    pub created_at: DateTime<Utc>,
    pub content_type: ContentType,
    pub original_byte_size: u64,
    /// Size of the STORED content (after stored-cap truncation).
    pub stored_byte_size: u64,
    /// 64-char lowercase hex SHA-256 of the stored content. Authoritative.
    pub sha256: String,
}

/// Section selector for partial reads. Phase 2 supports the union
/// `Full`; Phase 3 adds the narrower variants used by the lean schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    StatusOnly,
    Summary,
    DiffOnly,
    Full,
}

/// What `OutcomeStore::get` returns. Tied to `OutcomeMetadata.content_type`.
#[derive(Debug, Clone)]
pub struct OutcomeContent {
    pub bytes: Vec<u8>,
    pub metadata: OutcomeMetadata,
}

/// Reported by `OutcomeStore::sweep_older_than`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Number of namespaces (i.e., distinct `brain_session_id`s) deleted.
    pub namespaces_swept: usize,
    /// Number of individual blob+meta pairs deleted.
    pub blobs_swept: usize,
    /// Total bytes freed (sum of `stored_byte_size`).
    pub bytes_freed: u64,
    /// Effective TTL the store enforced. Never less than `Duration::from_secs(86_400)`
    /// for `FsOutcomeStore` (Round 9 P2-S3 — sub-day TTLs unsupported).
    pub effective_ttl: Duration,
}

/// All errors `OutcomeStore` impls can return. `Box` `io::Error` so
/// the enum stays cheap to clone where needed.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: {0:?}")]
    NotFound(OutcomeKey),
    #[error("authorization: caller session != artifact session (requested={requested:?}, actual={actual:?})")]
    Unauthorized {
        requested: OutcomeKey,
        actual: BrainSessionId,
    },
    #[error("content too large: {actual} > {limit}")]
    TooLarge { actual: u64, limit: u64 },
    /// Round 9 (N2) + Round 11 (SF1): same key, different content.
    /// Surfaces an upstream invariant violation: each
    /// `(brain_session, delegation, attempt)` triple should produce
    /// exactly one content blob.
    #[error("content mismatch for {key:?}: existing sha={existing_sha}, new sha={new_sha}")]
    ContentMismatch {
        key: OutcomeKey,
        existing_sha: String,
        new_sha: String,
    },
    /// Catch-all for backend-specific failures (e.g., `git update-ref`
    /// failed, S3 returned 5xx). The string is human-readable for logs.
    #[error("backend: {0}")]
    Backend(String),
}

#[allow(dead_code)]
fn _outcome_ref_unused_workaround(_r: &OutcomeRef) {
    // Suppress unused-import lint until Task 4 actually uses OutcomeRef.
    // Task 4 deletes this stub.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_metadata_round_trips_through_serde() {
        let m = OutcomeMetadata {
            created_at: Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: 2048,
            stored_byte_size: 1024,
            sha256: "a".repeat(64),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: OutcomeMetadata = serde_json::from_str(&s).unwrap();
        assert_eq!(back.content_type, ContentType::Stdout);
        assert_eq!(back.stored_byte_size, 1024);
        assert_eq!(back.sha256, m.sha256);
    }

    #[test]
    fn section_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&Section::StatusOnly).unwrap(), "\"status_only\"");
        assert_eq!(serde_json::to_string(&Section::DiffOnly).unwrap(), "\"diff_only\"");
        assert_eq!(serde_json::to_string(&Section::Full).unwrap(), "\"full\"");
    }

    #[test]
    fn store_error_renders_content_mismatch_clearly() {
        let key = OutcomeKey {
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        };
        let err = StoreError::ContentMismatch {
            key: key.clone(),
            existing_sha: "a".repeat(64),
            new_sha: "b".repeat(64),
        };
        let msg = format!("{err}");
        assert!(msg.contains("content mismatch"));
        assert!(msg.contains(&"a".repeat(64)));
        assert!(msg.contains(&"b".repeat(64)));
    }
}
```

- [ ] **Step 2: Write `crates/spur-blob-store/src/trait_def.rs`**

Replace placeholder with:

```rust
//! The `OutcomeStore` trait — content-addressed blob storage for
//! delegation outcomes.
//!
//! Implementations:
//! - `MemoryOutcomeStore` (test/dev) — in-process `HashMap`.
//! - `FsOutcomeStore` (default for new outcomes in production).
//! - `GitBlobOutcomeStore` (lives in `spur-worktree` to keep git
//!   knowledge in one place; depends on this crate for the trait).
//! - `MeasuredOutcomeStore<S>` decorator that emits `tracing` events.

use std::time::Duration;

use async_trait::async_trait;
use spur_acp::BrainSessionId;

use crate::{OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError, SweepReport};

/// Content-addressed outcome blob storage.
///
/// All methods are idempotent where stated and **must** be safe to call
/// concurrently from multiple async tasks (the trait requires `Send + Sync`).
#[async_trait]
pub trait OutcomeStore: Send + Sync {
    /// Store `content` under `key`. Idempotent: two `put` calls with
    /// the same key + the same content return the same `OutcomeRef`
    /// without rewriting. Differing content under the same key is an
    /// upstream invariant violation: returns
    /// [`StoreError::ContentMismatch`] (Round 11 SF1).
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError>;

    /// Read the content at `key`. `section` is currently `Some(Section::Full)`
    /// or `None` (treated as `Full`); Phase 3 widens the section selector.
    /// Implementations MAY reject non-`Full` sections with
    /// `StoreError::Backend("section not supported")` until Phase 3 lands.
    async fn get(
        &self,
        key: &OutcomeKey,
        section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError>;

    /// Delete every blob owned by `brain_session_id`. Returns the
    /// number of blobs deleted (zero is allowed). Used on session
    /// teardown.
    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError>;

    /// Sweep namespaces whose newest artifact is older than `ttl`.
    /// `FsOutcomeStore` requires `ttl >= 1 day` (Round 9 P2-S3); other
    /// impls may relax this.
    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError>;
}
```

- [ ] **Step 3: Re-enable the re-exports in `lib.rs`**

Edit `crates/spur-blob-store/src/lib.rs`. Uncomment:

```rust
pub use trait_def::OutcomeStore;
pub use types::{
    ContentType, OutcomeContent, OutcomeMetadata, Section, StoreError, SweepReport,
};
```

(Leave `MemoryOutcomeStore`, `FsOutcomeStore`, `MeasuredOutcomeStore` re-exports commented out — they land in Tasks 4–6.)

- [ ] **Step 4: Run check + tests**

Run: `cargo check -p spur-blob-store`
Expected: clean.

Run: `cargo test -p spur-blob-store`
Expected: 3 tests pass (the ones in `types.rs`).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-blob-store/src/
git commit -m "feat(spur-blob-store): define OutcomeStore trait + supporting types

Trait surface: put (idempotent + ContentMismatch), get (Section), 
delete_namespace, sweep_older_than (returns SweepReport).

Supporting types: OutcomeMetadata (single source of truth for sha256
per Round 11 SF1), ContentType, Section, OutcomeContent, SweepReport,
StoreError (with Unauthorized + ContentMismatch + TooLarge + Backend
variants).

Phase 2 of plan-5; spec §6.3."
```

---

## Task 4: Implement `MemoryOutcomeStore`

**Files:**
- Modify: `crates/spur-blob-store/src/memory_store.rs` (replace placeholder)
- Modify: `crates/spur-blob-store/src/lib.rs` (uncomment re-export)

**What:** Trivial in-process implementation backed by `Arc<RwLock<HashMap<OutcomeKey, (Vec<u8>, OutcomeMetadata)>>>`. Used in unit and integration tests across consumer crates. Behavior must match `FsOutcomeStore`'s contract for idempotence + `ContentMismatch`.

- [ ] **Step 1: Write `crates/spur-blob-store/src/memory_store.rs`**

```rust
//! In-process `OutcomeStore` for tests and dev.
//!
//! Same contract as `FsOutcomeStore` for idempotence and
//! `ContentMismatch`, just held in a `HashMap` instead of on disk.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use spur_acp::BrainSessionId;
use tokio::sync::RwLock;

use crate::trait_def::OutcomeStore;
use crate::{
    BackendTag, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError,
    SweepReport,
};

#[derive(Debug, Default, Clone)]
pub struct MemoryOutcomeStore {
    inner: Arc<RwLock<HashMap<OutcomeKey, (Vec<u8>, OutcomeMetadata)>>>,
}

impl MemoryOutcomeStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

#[async_trait]
impl OutcomeStore for MemoryOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        let new_sha = sha256_hex(content);
        if new_sha != metadata.sha256 {
            return Err(StoreError::Backend(format!(
                "metadata.sha256 ({}) does not match hashed content ({})",
                metadata.sha256, new_sha
            )));
        }

        let mut map = self.inner.write().await;
        if let Some((_, existing_meta)) = map.get(key) {
            if existing_meta.sha256 != new_sha {
                return Err(StoreError::ContentMismatch {
                    key: key.clone(),
                    existing_sha: existing_meta.sha256.clone(),
                    new_sha,
                });
            }
            // Idempotent re-put: return the existing ref.
            return Ok(OutcomeRef {
                key: key.clone(),
                sha256: existing_meta.sha256.clone(),
                byte_size: existing_meta.stored_byte_size,
                backend: BackendTag::Fs, // memory tags as Fs for testability
            });
        }

        map.insert(key.clone(), (content.to_vec(), metadata.clone()));
        Ok(OutcomeRef {
            key: key.clone(),
            sha256: new_sha,
            byte_size: metadata.stored_byte_size,
            backend: BackendTag::Fs,
        })
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let map = self.inner.read().await;
        match map.get(key) {
            Some((bytes, meta)) => Ok(OutcomeContent {
                bytes: bytes.clone(),
                metadata: meta.clone(),
            }),
            None => Err(StoreError::NotFound(key.clone())),
        }
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError> {
        let mut map = self.inner.write().await;
        let before = map.len();
        map.retain(|k, _| &k.brain_session_id != brain_session_id);
        Ok(before - map.len())
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(ttl).unwrap_or_else(|_| {
            chrono::Duration::seconds(0)
        });

        let mut map = self.inner.write().await;
        let mut report = SweepReport {
            effective_ttl: ttl,
            ..Default::default()
        };
        let mut sessions_swept: std::collections::HashSet<BrainSessionId> =
            std::collections::HashSet::new();

        map.retain(|k, (bytes, meta)| {
            if meta.created_at < cutoff {
                report.blobs_swept += 1;
                report.bytes_freed += bytes.len() as u64;
                sessions_swept.insert(k.brain_session_id.clone());
                false
            } else {
                true
            }
        });
        report.namespaces_swept = sessions_swept.len();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentType;
    use chrono::Utc;
    use spur_acp::SessionId;

    fn key(session: &str, delegation: &str, attempt: u32) -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(session.into())),
            delegation_id: delegation.into(),
            attempt,
        }
    }

    fn metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    #[tokio::test]
    async fn memory_store_put_get_roundtrip() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"hello world".to_vec();
        let meta = metadata(&body);

        let ref_a = store.put(&k, &body, &meta).await.expect("put");
        assert_eq!(ref_a.byte_size, body.len() as u64);
        assert_eq!(ref_a.sha256, sha256_hex(&body));

        let got = store.get(&k, Some(Section::Full)).await.expect("get");
        assert_eq!(got.bytes, body);
        assert_eq!(got.metadata.sha256, sha256_hex(&body));
    }

    #[tokio::test]
    async fn memory_store_idempotent_put_same_content() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"same".to_vec();
        let meta = metadata(&body);

        let ref_a = store.put(&k, &body, &meta).await.expect("first put");
        let ref_b = store.put(&k, &body, &meta).await.expect("second put");
        assert_eq!(ref_a.sha256, ref_b.sha256);
        assert_eq!(ref_a.byte_size, ref_b.byte_size);
    }

    #[tokio::test]
    async fn memory_store_content_mismatch_on_diff_content() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);

        let body_a = b"first".to_vec();
        let meta_a = metadata(&body_a);
        store.put(&k, &body_a, &meta_a).await.expect("first put");

        let body_b = b"second".to_vec();
        let meta_b = metadata(&body_b);
        let err = store.put(&k, &body_b, &meta_b).await.unwrap_err();
        match err {
            StoreError::ContentMismatch {
                key: ek,
                existing_sha,
                new_sha,
            } => {
                assert_eq!(ek, k);
                assert_eq!(existing_sha, meta_a.sha256);
                assert_eq!(new_sha, meta_b.sha256);
            }
            other => panic!("expected ContentMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_store_get_not_found() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-missing", 1);
        let err = store.get(&k, None).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(ref nf) if nf == &k));
    }

    #[tokio::test]
    async fn memory_store_delete_namespace_removes_only_that_session() {
        let store = MemoryOutcomeStore::new();
        let session_a = "550e8400-e29b-41d4-a716-446655440000";
        let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let k_a = key(session_a, "d-a", 1);
        let k_b = key(session_b, "d-b", 1);
        let body = b"body".to_vec();
        let meta = metadata(&body);

        store.put(&k_a, &body, &meta).await.unwrap();
        store.put(&k_b, &body, &meta).await.unwrap();

        let removed = store
            .delete_namespace(&BrainSessionId::new(SessionId(session_a.into())))
            .await
            .unwrap();
        assert_eq!(removed, 1);

        assert!(store.get(&k_a, None).await.is_err());
        assert!(store.get(&k_b, None).await.is_ok());
    }

    #[tokio::test]
    async fn memory_store_metadata_sha_must_match_content() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"body".to_vec();
        let mut meta = metadata(&body);
        meta.sha256 = "0".repeat(64); // wrong

        let err = store.put(&k, &body, &meta).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(_)), "expected Backend error, got {err:?}");
    }

    #[tokio::test]
    async fn memory_store_sweep_drops_old_namespaces() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"x".to_vec();
        let mut meta = metadata(&body);
        // Backdate so the entry is older than the sweep cutoff.
        meta.created_at = Utc::now() - chrono::Duration::seconds(10);

        store.put(&k, &body, &meta).await.unwrap();
        let report = store
            .sweep_older_than(Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(report.blobs_swept, 1);
        assert_eq!(report.namespaces_swept, 1);
    }
}
```

- [ ] **Step 2: Re-enable the re-export**

Edit `crates/spur-blob-store/src/lib.rs`. Uncomment:

```rust
pub use memory_store::MemoryOutcomeStore;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-blob-store --lib memory_store`
Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-blob-store/src/memory_store.rs crates/spur-blob-store/src/lib.rs
git commit -m "feat(spur-blob-store): MemoryOutcomeStore impl

In-process HashMap-backed OutcomeStore for tests and dev. Honors the
full contract: idempotent put, ContentMismatch on conflicting content,
namespace deletion, TTL sweep. metadata.sha256 must match the hashed
content (defense against caller bugs).

Phase 2 of plan-5; spec §6.4."
```

---

## Task 5: Implement `FsOutcomeStore`

**Files:**
- Modify: `crates/spur-blob-store/src/fs_store.rs` (replace placeholder)
- Modify: `crates/spur-blob-store/src/lib.rs` (uncomment re-export)

**What:** Filesystem-backed implementation. Path layout: `<root>/<session>/<delegation>/<attempt>.json` (the bytes file) plus `<root>/<session>/<delegation>/<attempt>.meta.json` sidecar (the `OutcomeMetadata` JSON). Atomic via tempfile + rename. UUID validation guards `put`. Reads existing sidecar to compare SHAs without re-hashing the data file (Round 11 SF1).

- [ ] **Step 1: Write `crates/spur-blob-store/src/fs_store.rs`**

```rust
//! Filesystem-backed `OutcomeStore`.
//!
//! Path layout:
//!   <root>/<brain_session_id>/<delegation_id>/<attempt>.bin   # content bytes
//!   <root>/<brain_session_id>/<delegation_id>/<attempt>.meta  # OutcomeMetadata JSON
//!
//! Both `brain_session_id` and `delegation_id` MUST parse via
//! `uuid::Uuid::parse_str` (Round 9 P2-S2) — defense against
//! directory traversal and shell-meta injection.
//!
//! Idempotent `put`: reads `<attempt>.meta` first, compares sha256.
//! Equal → return existing `OutcomeRef`. Different → `ContentMismatch`.
//! Missing → atomic write via tempfile-then-rename (Round 11 SF1
//! single-source-of-truth: meta.sha256 not re-hashed from disk).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::DateTime;
use sha2::{Digest, Sha256};
use spur_acp::BrainSessionId;
use tokio::fs;

use crate::trait_def::OutcomeStore;
use crate::{
    BackendTag, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError,
    SweepReport,
};

const MIN_TTL_SECS: u64 = 86_400; // Round 9 P2-S3: 1 day floor.

#[derive(Debug, Clone)]
pub struct FsOutcomeStore {
    root: Arc<PathBuf>,
}

impl FsOutcomeStore {
    /// Construct a store rooted at `root`. The directory is created on
    /// first `put`; readiness is not eagerly probed.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
        }
    }

    fn validate_uuid(value: &str, field: &str) -> Result<(), StoreError> {
        // Lightweight UUID-shape check: 36 chars, hex + 4 dashes at fixed positions.
        // Avoids pulling in the `uuid` crate just for validation.
        if value.len() != 36 {
            return Err(StoreError::Backend(format!(
                "non-uuid {field}: wrong length ({})",
                value.len()
            )));
        }
        for (i, c) in value.chars().enumerate() {
            let ok = match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            };
            if !ok {
                return Err(StoreError::Backend(format!(
                    "non-uuid {field}: bad char at position {i}"
                )));
            }
        }
        Ok(())
    }

    fn paths_for(&self, key: &OutcomeKey) -> (PathBuf, PathBuf, PathBuf) {
        let session_dir = self.root.join(key.brain_session_id.as_str());
        let delegation_dir = session_dir.join(key.delegation_id.as_str());
        let bin = delegation_dir.join(format!("{}.bin", key.attempt));
        let meta = delegation_dir.join(format!("{}.meta", key.attempt));
        (delegation_dir, bin, meta)
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

#[async_trait]
impl OutcomeStore for FsOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        Self::validate_uuid(key.brain_session_id.as_str(), "brain_session_id")?;
        Self::validate_uuid(key.delegation_id.as_str(), "delegation_id")?;

        let new_sha = sha256_hex(content);
        if new_sha != metadata.sha256 {
            return Err(StoreError::Backend(format!(
                "metadata.sha256 ({}) does not match hashed content ({})",
                metadata.sha256, new_sha
            )));
        }

        let (dir, bin_path, meta_path) = self.paths_for(key);

        // ContentMismatch / idempotence check via metadata sidecar.
        if meta_path.exists() {
            let raw = fs::read(&meta_path).await?;
            let existing_meta: OutcomeMetadata = serde_json::from_slice(&raw)
                .map_err(|e| StoreError::Backend(format!("corrupt sidecar: {e}")))?;
            if existing_meta.sha256 == new_sha {
                return Ok(OutcomeRef {
                    key: key.clone(),
                    sha256: new_sha,
                    byte_size: existing_meta.stored_byte_size,
                    backend: BackendTag::Fs,
                });
            }
            return Err(StoreError::ContentMismatch {
                key: key.clone(),
                existing_sha: existing_meta.sha256,
                new_sha,
            });
        }

        fs::create_dir_all(&dir).await?;

        // Atomic content write via temp + rename.
        let tmp_bin = dir.join(format!("{}.bin.tmp.{}", key.attempt, std::process::id()));
        fs::write(&tmp_bin, content).await?;
        fs::rename(&tmp_bin, &bin_path).await?;

        // Atomic metadata write via temp + rename.
        let tmp_meta = dir.join(format!("{}.meta.tmp.{}", key.attempt, std::process::id()));
        let meta_bytes = serde_json::to_vec(metadata)
            .map_err(|e| StoreError::Backend(format!("metadata serialize: {e}")))?;
        fs::write(&tmp_meta, &meta_bytes).await?;
        fs::rename(&tmp_meta, &meta_path).await?;

        Ok(OutcomeRef {
            key: key.clone(),
            sha256: new_sha,
            byte_size: metadata.stored_byte_size,
            backend: BackendTag::Fs,
        })
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let (_, bin_path, meta_path) = self.paths_for(key);
        if !meta_path.exists() {
            return Err(StoreError::NotFound(key.clone()));
        }
        let raw_meta = fs::read(&meta_path).await?;
        let metadata: OutcomeMetadata = serde_json::from_slice(&raw_meta)
            .map_err(|e| StoreError::Backend(format!("corrupt sidecar: {e}")))?;
        let bytes = fs::read(&bin_path).await?;
        Ok(OutcomeContent { bytes, metadata })
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError> {
        Self::validate_uuid(brain_session_id.as_str(), "brain_session_id")?;
        let session_dir = self.root.join(brain_session_id.as_str());
        if !session_dir.exists() {
            return Ok(0);
        }
        let count = count_blobs(&session_dir).await?;
        fs::remove_dir_all(&session_dir).await?;
        Ok(count)
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let effective_ttl = ttl.max(Duration::from_secs(MIN_TTL_SECS));
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(effective_ttl).unwrap_or_else(|_| {
                chrono::Duration::seconds(MIN_TTL_SECS as i64)
            });

        let mut report = SweepReport {
            effective_ttl,
            ..Default::default()
        };
        if !self.root.exists() {
            return Ok(report);
        }

        let mut entries = fs::read_dir(&*self.root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let session_dir = entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let newest = newest_meta_in(&session_dir).await?;
            match newest {
                Some(ts) if ts < cutoff => {
                    let stats = collect_namespace_stats(&session_dir).await?;
                    report.namespaces_swept += 1;
                    report.blobs_swept += stats.blob_count;
                    report.bytes_freed += stats.bytes;
                    fs::remove_dir_all(&session_dir).await?;
                }
                _ => continue,
            }
        }
        Ok(report)
    }
}

#[derive(Default)]
struct NamespaceStats {
    blob_count: usize,
    bytes: u64,
}

async fn count_blobs(session_dir: &Path) -> Result<usize, StoreError> {
    let mut count = 0usize;
    let mut delegation_dirs = fs::read_dir(session_dir).await?;
    while let Some(d) = delegation_dirs.next_entry().await? {
        if !d.path().is_dir() {
            continue;
        }
        let mut files = fs::read_dir(d.path()).await?;
        while let Some(f) = files.next_entry().await? {
            if f.path().extension().and_then(|s| s.to_str()) == Some("meta") {
                count += 1;
            }
        }
    }
    Ok(count)
}

async fn newest_meta_in(session_dir: &Path) -> Result<Option<DateTime<chrono::Utc>>, StoreError> {
    let mut newest: Option<DateTime<chrono::Utc>> = None;
    let mut delegation_dirs = fs::read_dir(session_dir).await?;
    while let Some(d) = delegation_dirs.next_entry().await? {
        if !d.path().is_dir() {
            continue;
        }
        let mut files = fs::read_dir(d.path()).await?;
        while let Some(f) = files.next_entry().await? {
            if f.path().extension().and_then(|s| s.to_str()) != Some("meta") {
                continue;
            }
            let raw = fs::read(f.path()).await?;
            let meta: OutcomeMetadata = match serde_json::from_slice(&raw) {
                Ok(m) => m,
                Err(_) => continue,
            };
            newest = Some(match newest {
                Some(prev) if prev > meta.created_at => prev,
                _ => meta.created_at,
            });
        }
    }
    Ok(newest)
}

async fn collect_namespace_stats(session_dir: &Path) -> Result<NamespaceStats, StoreError> {
    let mut stats = NamespaceStats::default();
    let mut delegation_dirs = fs::read_dir(session_dir).await?;
    while let Some(d) = delegation_dirs.next_entry().await? {
        if !d.path().is_dir() {
            continue;
        }
        let mut files = fs::read_dir(d.path()).await?;
        while let Some(f) = files.next_entry().await? {
            let p = f.path();
            if p.extension().and_then(|s| s.to_str()) == Some("meta") {
                let raw = fs::read(&p).await?;
                if let Ok(meta) = serde_json::from_slice::<OutcomeMetadata>(&raw) {
                    stats.blob_count += 1;
                    stats.bytes += meta.stored_byte_size;
                }
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentType;
    use chrono::Utc;
    use spur_acp::SessionId;
    use tempfile::TempDir;

    fn key(session: &str, delegation: &str, attempt: u32) -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(session.into())),
            delegation_id: delegation.into(),
            attempt,
        }
    }

    fn metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    #[tokio::test]
    async fn fs_store_put_get_roundtrip() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"hello world".to_vec();

        let r = store.put(&k, &body, &metadata(&body)).await.expect("put");
        assert_eq!(r.byte_size, body.len() as u64);
        assert_eq!(r.backend, BackendTag::Fs);

        let got = store.get(&k, Some(Section::Full)).await.expect("get");
        assert_eq!(got.bytes, body);
    }

    #[tokio::test]
    async fn fs_store_idempotent_put() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"same".to_vec();
        let m = metadata(&body);

        let a = store.put(&k, &body, &m).await.unwrap();
        let b = store.put(&k, &body, &m).await.unwrap();
        assert_eq!(a.sha256, b.sha256);
    }

    #[tokio::test]
    async fn fs_store_content_mismatch() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );

        let body_a = b"first".to_vec();
        store.put(&k, &body_a, &metadata(&body_a)).await.unwrap();

        let body_b = b"second".to_vec();
        let err = store
            .put(&k, &body_b, &metadata(&body_b))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::ContentMismatch { .. }));
    }

    #[tokio::test]
    async fn fs_store_rejects_non_uuid_session() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let bad = OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId("../etc/passwd".into())),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        };
        let body = b"x".to_vec();
        let err = store.put(&bad, &body, &metadata(&body)).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(ref s) if s.contains("non-uuid")));
    }

    #[tokio::test]
    async fn fs_store_namespace_isolation() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let session_a = "550e8400-e29b-41d4-a716-446655440000";
        let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let k_a = key(session_a, "deadbeef-1111-2222-3333-444455556666", 1);
        let k_b = key(session_b, "deadbeef-1111-2222-3333-bbbbbbbbbbbb", 1);
        let body = b"body".to_vec();

        store.put(&k_a, &body, &metadata(&body)).await.unwrap();
        store.put(&k_b, &body, &metadata(&body)).await.unwrap();

        let removed = store
            .delete_namespace(&BrainSessionId::new(SessionId(session_a.into())))
            .await
            .unwrap();
        assert_eq!(removed, 1);

        assert!(store.get(&k_a, None).await.is_err());
        assert!(store.get(&k_b, None).await.is_ok());
    }

    #[tokio::test]
    async fn fs_store_sweep_clamps_to_one_day() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let report = store.sweep_older_than(Duration::from_secs(60)).await.unwrap();
        assert_eq!(report.effective_ttl, Duration::from_secs(MIN_TTL_SECS));
    }

    #[tokio::test]
    async fn fs_store_sweep_drops_old_namespace() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"old".to_vec();
        let mut m = metadata(&body);
        m.created_at = Utc::now() - chrono::Duration::days(2); // older than 1 day

        store.put(&k, &body, &m).await.unwrap();
        let report = store.sweep_older_than(Duration::from_secs(MIN_TTL_SECS)).await.unwrap();
        assert_eq!(report.namespaces_swept, 1);
        assert_eq!(report.blobs_swept, 1);
        assert!(store.get(&k, None).await.is_err());
    }
}
```

- [ ] **Step 2: Re-enable the re-export**

Edit `crates/spur-blob-store/src/lib.rs`. Uncomment:

```rust
pub use fs_store::FsOutcomeStore;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-blob-store --lib fs_store`
Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-blob-store/src/fs_store.rs crates/spur-blob-store/src/lib.rs
git commit -m "feat(spur-blob-store): FsOutcomeStore impl

Filesystem-backed OutcomeStore. Path layout:
  <root>/<session>/<delegation>/<attempt>.{bin,meta}

UUID validation guards put/delete_namespace (Round 9 P2-S2 — defense
against directory traversal). Idempotent put: reads existing .meta
sidecar to compare sha256 (Round 11 SF1 — no re-hash from disk).
ContentMismatch on conflicting content. Sweep clamps to 1-day floor
(Round 9 P2-S3).

Phase 2 of plan-5; spec §6.4."
```

---

## Task 6: Implement `MeasuredOutcomeStore<S>` decorator

**Files:**
- Modify: `crates/spur-blob-store/src/measured.rs` (replace placeholder)
- Modify: `crates/spur-blob-store/src/lib.rs` (uncomment re-export)

**What:** Generic decorator that wraps any `OutcomeStore` and emits `tracing::event!` calls for put/get/delete_namespace/sweep with size + latency. Target: `spur.metrics.blob_store.*`. Optional in production, mandatory in dev/CI.

- [ ] **Step 1: Write `crates/spur-blob-store/src/measured.rs`**

```rust
//! Decorator that emits `tracing` events for every `OutcomeStore`
//! operation. Wrap any inner store; preserves its behavior.
//!
//! Event target: `spur.metrics.blob_store.*` (matches Plan-4 §12.1).

use std::time::{Duration, Instant};

use async_trait::async_trait;
use spur_acp::BrainSessionId;
use tracing::event;

use crate::trait_def::OutcomeStore;
use crate::{
    OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError, SweepReport,
};

#[derive(Debug)]
pub struct MeasuredOutcomeStore<S: OutcomeStore> {
    inner: S,
}

impl<S: OutcomeStore> MeasuredOutcomeStore<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<S: OutcomeStore> OutcomeStore for MeasuredOutcomeStore<S> {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        let start = Instant::now();
        let result = self.inner.put(key, content, metadata).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        let bytes = content.len() as u64;

        match &result {
            Ok(r) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::DEBUG,
                op = "put",
                outcome = "ok",
                bytes,
                elapsed_us,
                backend = ?r.backend,
                sha256 = %r.sha256,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::WARN,
                op = "put",
                outcome = "err",
                bytes,
                elapsed_us,
                error = %e,
            ),
        }
        result
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let start = Instant::now();
        let result = self.inner.get(key, section).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        match &result {
            Ok(c) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::DEBUG,
                op = "get",
                outcome = "ok",
                bytes = c.bytes.len() as u64,
                elapsed_us,
                ?section,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::DEBUG,
                op = "get",
                outcome = "err",
                elapsed_us,
                error = %e,
                ?section,
            ),
        }
        result
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError> {
        let start = Instant::now();
        let result = self.inner.delete_namespace(brain_session_id).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        match &result {
            Ok(n) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::INFO,
                op = "delete_namespace",
                outcome = "ok",
                elapsed_us,
                blobs_removed = *n,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::WARN,
                op = "delete_namespace",
                outcome = "err",
                elapsed_us,
                error = %e,
            ),
        }
        result
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let start = Instant::now();
        let result = self.inner.sweep_older_than(ttl).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        match &result {
            Ok(r) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::INFO,
                op = "sweep",
                outcome = "ok",
                elapsed_us,
                namespaces_swept = r.namespaces_swept,
                blobs_swept = r.blobs_swept,
                bytes_freed = r.bytes_freed,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::WARN,
                op = "sweep",
                outcome = "err",
                elapsed_us,
                error = %e,
            ),
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentType, MemoryOutcomeStore};
    use chrono::Utc;
    use sha2::{Digest, Sha256};
    use spur_acp::SessionId;

    fn sha256_hex(content: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(content);
        let d = h.finalize();
        let mut s = String::with_capacity(64);
        for b in d {
            use std::fmt::Write;
            write!(&mut s, "{b:02x}").unwrap();
        }
        s
    }

    fn key() -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        }
    }

    fn metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    #[tokio::test]
    async fn measured_store_preserves_inner_behavior() {
        let inner = MemoryOutcomeStore::new();
        let store = MeasuredOutcomeStore::new(inner);
        let k = key();
        let body = b"trace me".to_vec();

        let r = store.put(&k, &body, &metadata(&body)).await.expect("put");
        assert_eq!(r.byte_size, body.len() as u64);

        let got = store.get(&k, None).await.expect("get");
        assert_eq!(got.bytes, body);
    }

    #[tokio::test]
    async fn measured_store_propagates_errors() {
        let inner = MemoryOutcomeStore::new();
        let store = MeasuredOutcomeStore::new(inner);
        let k = key();
        let err = store.get(&k, None).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }
}
```

- [ ] **Step 2: Re-enable the re-export**

Edit `crates/spur-blob-store/src/lib.rs`. Uncomment:

```rust
pub use measured::MeasuredOutcomeStore;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-blob-store --lib measured`
Expected: 2 tests pass.

- [ ] **Step 4: Run the full crate test suite**

Run: `cargo test -p spur-blob-store`
Expected: all green (types::tests + memory_store::tests + fs_store::tests + measured::tests + outcome::tests via the re-exported `pub use`).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-blob-store/src/measured.rs crates/spur-blob-store/src/lib.rs
git commit -m "feat(spur-blob-store): MeasuredOutcomeStore<S> decorator

Generic decorator that emits tracing events for every OutcomeStore op
on target spur.metrics.blob_store. Used in dev/CI for visibility,
optional in production. Behavior is pure passthrough.

Phase 2 of plan-5; spec §6.4."
```

---

## Task 7: `OutcomeRef` backcompat adapter (`as_worker_artifact`)

**Files:**
- Modify: `crates/spur-acp/src/domain/outcome.rs` (add adapter impl block)

**What:** Map a GitBlob-backed `OutcomeRef` into the legacy `WorkerArtifact` shape so Phase 2's wrapped persist path produces the same `DelegationResult.artifact` value the orchestrator currently expects. Round 11 fix: returns the **real** new-namespace ref (`refs/spur/outcomes/<session>/<delegation>-<attempt>.blob`), not the legacy hardcoded `refs/spur/artifacts/<session>` path.

- [ ] **Step 1: Append the adapter to `crates/spur-acp/src/domain/outcome.rs`**

Add at the end of the file (BEFORE the `#[cfg(test)] mod tests` block):

```rust
use crate::domain::artifact::{ArtifactKind as WorkerArtifactKind, WorkerArtifact};

impl OutcomeRef {
    /// Backcompat adapter: project a GitBlob-backed `OutcomeRef` into
    /// the legacy `WorkerArtifact` shape. Returns `None` for non-git
    /// backends. Phase 2 callers use this to preserve
    /// `DelegationResult.artifact` behavior during transition; Phase 3
    /// cleanup may remove or deprecate.
    ///
    /// Round 11 (MF2): returns the REAL per-(session, delegation, attempt)
    /// ref under `refs/spur/outcomes/`, NOT the legacy shared-per-session
    /// `refs/spur/artifacts/<session>` ref. The legacy ref is read-only
    /// during Phase 1 transition; new writes go to the new namespace.
    pub fn as_worker_artifact(&self, kind: WorkerArtifactKind) -> Option<WorkerArtifact> {
        match self.backend {
            BackendTag::GitBlob => Some(WorkerArtifact {
                object_ref: format!(
                    "refs/spur/outcomes/{}/{}-{}.blob",
                    self.key.brain_session_id.as_str(),
                    self.key.delegation_id.as_str(),
                    self.key.attempt,
                ),
                blob_sha: self.sha256.clone(),
                size_bytes: self.byte_size as usize,
                kind,
            }),
            _ => None,
        }
    }
}
```

- [ ] **Step 2: Add a test for the adapter**

Inside the existing `#[cfg(test)] mod tests` in `outcome.rs`, append:

```rust
    #[test]
    fn as_worker_artifact_maps_git_blob_backend_only() {
        use crate::domain::artifact::ArtifactKind as WorkerArtifactKind;

        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(40),
            byte_size: 99,
            backend: BackendTag::GitBlob,
        };
        let wa = r
            .as_worker_artifact(WorkerArtifactKind::Output)
            .expect("git_blob backend should map");
        assert_eq!(
            wa.object_ref,
            format!(
                "refs/spur/outcomes/{}/{}-{}.blob",
                r.key.brain_session_id.as_str(),
                r.key.delegation_id.as_str(),
                r.key.attempt,
            )
        );
        assert_eq!(wa.blob_sha, r.sha256);
        assert_eq!(wa.size_bytes, 99);
    }

    #[test]
    fn as_worker_artifact_returns_none_for_fs_backend() {
        use crate::domain::artifact::ArtifactKind as WorkerArtifactKind;

        let r = OutcomeRef {
            key: key(),
            sha256: "a".repeat(40),
            byte_size: 99,
            backend: BackendTag::Fs,
        };
        assert!(r.as_worker_artifact(WorkerArtifactKind::Output).is_none());
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-acp --lib outcome`
Expected: 6 tests pass (4 from Task 2 + 2 new).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/domain/outcome.rs
git commit -m "feat(spur-acp): OutcomeRef::as_worker_artifact backcompat adapter

Project a GitBlob-backed OutcomeRef into the legacy WorkerArtifact
shape so Phase 2's wrapped persist path produces the same
DelegationResult.artifact value the orchestrator expects.

Round 11 (MF2): returns the real per-(session, delegation, attempt)
ref under refs/spur/outcomes/, not the legacy hardcoded
refs/spur/artifacts/<session> path. Returns None for non-git backends.

Phase 2 of plan-5; spec §6.5."
```

---

## Task 8: `GitBlobOutcomeStore` in `spur-worktree`

**Files:**
- Modify: `crates/spur-worktree/Cargo.toml` (add `spur-blob-store` workspace dep)
- Modify: `crates/spur-worktree/src/lib.rs` (add `pub mod git_blob_store;`)
- Create: `crates/spur-worktree/src/git_blob_store.rs`
- Create: `crates/spur-worktree/tests/git_blob_store_impl.rs`

**What:** The git-backed `OutcomeStore` impl. Lives in `spur-worktree` (which already owns git ref operations) and depends on `spur-blob-store` for the trait. Writes to the **new** namespace `refs/spur/outcomes/<session>/<delegation>-<attempt>.{blob,meta}` (Round 11 MF1+MF2).

- [ ] **Step 1: Add the workspace dep to spur-worktree**

Run: `grep -n "^spur-acp\|^\\[dependencies\\]" crates/spur-worktree/Cargo.toml | head -5`

Find the `[dependencies]` block and add (alphabetical placement):

```toml
spur-blob-store = { workspace = true }
```

Also ensure `async-trait`, `chrono`, `serde_json`, and `sha2` are present (they should be, but check). If `async-trait` is missing, add `async-trait = { workspace = true }`.

- [ ] **Step 2: Read existing `crates/spur-worktree/src/lib.rs`**

Run: `cat crates/spur-worktree/src/lib.rs`

Note the `pub mod` declarations. Add a new line at the bottom:

```rust
pub mod git_blob_store;
```

- [ ] **Step 3: Write `crates/spur-worktree/src/git_blob_store.rs`**

```rust
//! `OutcomeStore` impl backed by git blobs in a SPUR worktree.
//!
//! Lives here (not in `spur-blob-store`) because this crate already owns
//! `git update-ref` / `git cat-file` plumbing.
//!
//! Ref namespace (Round 11 MF1+MF2):
//!   refs/spur/outcomes/<session-id>/<delegation-id>-<attempt>.blob   # content
//!   refs/spur/outcomes/<session-id>/<delegation-id>-<attempt>.meta   # OutcomeMetadata JSON
//!
//! Both refs are leaves under the namespace — no D/F conflict with the
//! legacy `refs/spur/artifacts/<session-id>` ref (which remains
//! read-only during transition).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use spur_acp::BrainSessionId;
use spur_blob_store::{
    BackendTag, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, OutcomeStore, Section,
    StoreError, SweepReport,
};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct GitBlobOutcomeStore {
    repo_root: Arc<PathBuf>,
}

impl GitBlobOutcomeStore {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root: Arc::new(repo_root),
        }
    }

    fn validate_uuid(value: &str, field: &str) -> Result<(), StoreError> {
        if value.len() != 36 {
            return Err(StoreError::Backend(format!(
                "non-uuid {field}: wrong length ({})",
                value.len()
            )));
        }
        for (i, c) in value.chars().enumerate() {
            let ok = match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            };
            if !ok {
                return Err(StoreError::Backend(format!(
                    "non-uuid {field}: bad char at position {i}"
                )));
            }
        }
        Ok(())
    }

    fn blob_ref(key: &OutcomeKey) -> String {
        format!(
            "refs/spur/outcomes/{}/{}-{}.blob",
            key.brain_session_id.as_str(),
            key.delegation_id.as_str(),
            key.attempt,
        )
    }

    fn meta_ref(key: &OutcomeKey) -> String {
        format!(
            "refs/spur/outcomes/{}/{}-{}.meta",
            key.brain_session_id.as_str(),
            key.delegation_id.as_str(),
            key.attempt,
        )
    }

    fn session_ref_glob(session: &BrainSessionId) -> String {
        format!("refs/spur/outcomes/{}/", session.as_str())
    }

    async fn run_git(&self, args: &[&str]) -> Result<Vec<u8>, StoreError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&*self.repo_root)
            .output()
            .await
            .map_err(|e| StoreError::Backend(format!("git spawn: {e}")))?;
        if !output.status.success() {
            return Err(StoreError::Backend(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }

    async fn run_git_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Result<Vec<u8>, StoreError> {
        use tokio::io::AsyncWriteExt;
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&*self.repo_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| StoreError::Backend(format!("git spawn: {e}")))?;
        if let Some(mut sin) = child.stdin.take() {
            sin.write_all(stdin)
                .await
                .map_err(|e| StoreError::Backend(format!("git stdin: {e}")))?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| StoreError::Backend(format!("git wait: {e}")))?;
        if !output.status.success() {
            return Err(StoreError::Backend(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }

    async fn read_meta(&self, key: &OutcomeKey) -> Result<Option<OutcomeMetadata>, StoreError> {
        let meta_ref = Self::meta_ref(key);
        let rev_parse = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &meta_ref])
            .current_dir(&*self.repo_root)
            .output()
            .await
            .map_err(|e| StoreError::Backend(format!("git spawn: {e}")))?;
        if !rev_parse.status.success() {
            return Ok(None);
        }
        let sha = String::from_utf8_lossy(&rev_parse.stdout).trim().to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        let raw = self.run_git(&["cat-file", "-p", &sha]).await?;
        let meta: OutcomeMetadata = serde_json::from_slice(&raw)
            .map_err(|e| StoreError::Backend(format!("corrupt meta sidecar: {e}")))?;
        Ok(Some(meta))
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

#[async_trait]
impl OutcomeStore for GitBlobOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        Self::validate_uuid(key.brain_session_id.as_str(), "brain_session_id")?;
        Self::validate_uuid(key.delegation_id.as_str(), "delegation_id")?;

        let new_sha = sha256_hex(content);
        if new_sha != metadata.sha256 {
            return Err(StoreError::Backend(format!(
                "metadata.sha256 ({}) does not match hashed content ({})",
                metadata.sha256, new_sha
            )));
        }

        if let Some(existing) = self.read_meta(key).await? {
            if existing.sha256 == new_sha {
                return Ok(OutcomeRef {
                    key: key.clone(),
                    sha256: new_sha,
                    byte_size: existing.stored_byte_size,
                    backend: BackendTag::GitBlob,
                });
            }
            return Err(StoreError::ContentMismatch {
                key: key.clone(),
                existing_sha: existing.sha256,
                new_sha,
            });
        }

        // Write the content blob.
        let blob_sha_bytes = self
            .run_git_with_stdin(&["hash-object", "-w", "--stdin"], content)
            .await?;
        let blob_sha = String::from_utf8_lossy(&blob_sha_bytes).trim().to_string();
        let blob_ref = Self::blob_ref(key);
        self.run_git(&["update-ref", &blob_ref, &blob_sha]).await?;

        // Write the meta blob.
        let meta_bytes = serde_json::to_vec(metadata)
            .map_err(|e| StoreError::Backend(format!("metadata serialize: {e}")))?;
        let meta_blob_sha_bytes = self
            .run_git_with_stdin(&["hash-object", "-w", "--stdin"], &meta_bytes)
            .await?;
        let meta_blob_sha = String::from_utf8_lossy(&meta_blob_sha_bytes)
            .trim()
            .to_string();
        let meta_ref = Self::meta_ref(key);
        self.run_git(&["update-ref", &meta_ref, &meta_blob_sha])
            .await?;

        Ok(OutcomeRef {
            key: key.clone(),
            sha256: new_sha,
            byte_size: metadata.stored_byte_size,
            backend: BackendTag::GitBlob,
        })
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let meta = match self.read_meta(key).await? {
            Some(m) => m,
            None => return Err(StoreError::NotFound(key.clone())),
        };
        let blob_ref = Self::blob_ref(key);
        let blob_sha_out = self
            .run_git(&["rev-parse", "--verify", &blob_ref])
            .await?;
        let blob_sha = String::from_utf8_lossy(&blob_sha_out).trim().to_string();
        let bytes = self.run_git(&["cat-file", "-p", &blob_sha]).await?;
        Ok(OutcomeContent {
            bytes,
            metadata: meta,
        })
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError> {
        Self::validate_uuid(brain_session_id.as_str(), "brain_session_id")?;
        let prefix = Self::session_ref_glob(brain_session_id);
        let pattern = prefix.trim_end_matches('/');

        // List all refs under the namespace.
        let listing = self
            .run_git(&["for-each-ref", "--format=%(refname)", pattern])
            .await?;
        let listing_str = String::from_utf8_lossy(&listing);
        let refs: Vec<&str> = listing_str.lines().filter(|l| !l.is_empty()).collect();

        // Each (blob,meta) pair is one logical blob.
        let mut count = 0usize;
        for r in &refs {
            // Run update-ref -d for each ref (rare batched op; loop is fine).
            self.run_git(&["update-ref", "-d", r]).await?;
            if r.ends_with(".meta") {
                count += 1;
            }
        }

        // Also clean up the legacy ref for this session if present
        // (Round 11 MF2: legacy is read-only during transition; deleting
        // on namespace teardown removes the pre-Plan-5 debt).
        let legacy = format!("refs/spur/artifacts/{}", brain_session_id.as_str());
        let _ = self.run_git(&["update-ref", "-d", &legacy]).await;

        Ok(count)
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(0));

        // For each session subnamespace, find the newest .meta sidecar.
        // git for-each-ref outputs everything under refs/spur/outcomes/.
        let listing = self
            .run_git(&["for-each-ref", "--format=%(refname)", "refs/spur/outcomes/"])
            .await?;
        let listing_str = String::from_utf8_lossy(&listing);

        // Group refs by session (refs/spur/outcomes/<session>/...).
        use std::collections::BTreeMap;
        let mut by_session: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in listing_str.lines().filter(|l| !l.is_empty()) {
            if let Some(rest) = line.strip_prefix("refs/spur/outcomes/") {
                if let Some((session, _)) = rest.split_once('/') {
                    by_session
                        .entry(session.to_string())
                        .or_default()
                        .push(line.to_string());
                }
            }
        }

        let mut report = SweepReport {
            effective_ttl: ttl,
            ..Default::default()
        };

        for (session, refs) in by_session {
            let mut newest: Option<DateTime<Utc>> = None;
            let mut total_bytes = 0u64;
            let mut blob_count = 0usize;
            for r in &refs {
                if !r.ends_with(".meta") {
                    continue;
                }
                let sha_out = match self.run_git(&["rev-parse", r]).await {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let sha = String::from_utf8_lossy(&sha_out).trim().to_string();
                let raw = match self.run_git(&["cat-file", "-p", &sha]).await {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let meta: OutcomeMetadata = match serde_json::from_slice(&raw) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                blob_count += 1;
                total_bytes += meta.stored_byte_size;
                newest = Some(match newest {
                    Some(prev) if prev > meta.created_at => prev,
                    _ => meta.created_at,
                });
            }
            if let Some(ts) = newest {
                if ts < cutoff {
                    for r in &refs {
                        let _ = self.run_git(&["update-ref", "-d", r]).await;
                    }
                    report.namespaces_swept += 1;
                    report.blobs_swept += blob_count;
                    report.bytes_freed += total_bytes;
                }
            }
        }
        Ok(report)
    }
}

#[allow(dead_code)]
fn _unused(_p: &Path) {}
```

- [ ] **Step 4: Write the cross-crate integration test `crates/spur-worktree/tests/git_blob_store_impl.rs`**

```rust
//! Integration test: GitBlobOutcomeStore against a real (tempfile) git repo.

use chrono::Utc;
use sha2::{Digest, Sha256};
use spur_acp::{BrainSessionId, SessionId};
use spur_blob_store::{
    BackendTag, ContentType, OutcomeKey, OutcomeMetadata, OutcomeStore, Section, StoreError,
};
use spur_worktree::git_blob_store::GitBlobOutcomeStore;
use std::process::Command;
use tempfile::TempDir;

fn sha256_hex(content: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(content);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

fn init_repo(p: &std::path::Path) {
    let r = Command::new("git").args(["init", "--quiet"]).current_dir(p).output().unwrap();
    assert!(r.status.success());
    Command::new("git").args(["config", "user.email", "t@e.com"]).current_dir(p).output().unwrap();
    Command::new("git").args(["config", "user.name", "t"]).current_dir(p).output().unwrap();
}

fn key(s: &str, d: &str, a: u32) -> OutcomeKey {
    OutcomeKey {
        brain_session_id: BrainSessionId::new(SessionId(s.into())),
        delegation_id: d.into(),
        attempt: a,
    }
}

fn metadata(content: &[u8]) -> OutcomeMetadata {
    OutcomeMetadata {
        created_at: Utc::now(),
        content_type: ContentType::Stdout,
        original_byte_size: content.len() as u64,
        stored_byte_size: content.len() as u64,
        sha256: sha256_hex(content),
    }
}

#[tokio::test]
async fn git_blob_store_put_get_roundtrip() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    let body = b"hello git\n".to_vec();
    let r = store.put(&k, &body, &metadata(&body)).await.unwrap();
    assert_eq!(r.backend, BackendTag::GitBlob);
    assert_eq!(r.byte_size, body.len() as u64);

    let got = store.get(&k, Some(Section::Full)).await.unwrap();
    assert_eq!(got.bytes, body);
}

#[tokio::test]
async fn git_blob_store_idempotent_put() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    let body = b"same".to_vec();
    let m = metadata(&body);
    let a = store.put(&k, &body, &m).await.unwrap();
    let b = store.put(&k, &body, &m).await.unwrap();
    assert_eq!(a.sha256, b.sha256);
}

#[tokio::test]
async fn git_blob_store_content_mismatch() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    store.put(&k, b"first", &metadata(b"first")).await.unwrap();
    let err = store
        .put(&k, b"second", &metadata(b"second"))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::ContentMismatch { .. }));
}

#[tokio::test]
async fn git_blob_store_namespace_isolation() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let session_a = "550e8400-e29b-41d4-a716-446655440000";
    let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
    let k_a = key(session_a, "deadbeef-1111-2222-3333-444455556666", 1);
    let k_b = key(session_b, "deadbeef-1111-2222-3333-bbbbbbbbbbbb", 1);
    store.put(&k_a, b"A", &metadata(b"A")).await.unwrap();
    store.put(&k_b, b"B", &metadata(b"B")).await.unwrap();

    let removed = store
        .delete_namespace(&BrainSessionId::new(SessionId(session_a.into())))
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(store.get(&k_a, None).await.is_err());
    assert!(store.get(&k_b, None).await.is_ok());
}

#[tokio::test]
async fn git_blob_store_rejects_non_uuid() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let bad = OutcomeKey {
        brain_session_id: BrainSessionId::new(SessionId("../etc/passwd".into())),
        delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
        attempt: 1,
    };
    let err = store.put(&bad, b"x", &metadata(b"x")).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend(ref s) if s.contains("non-uuid")));
}

#[tokio::test]
async fn git_blob_store_per_attempt_granularity() {
    // Verifies Round 11 MF1 fix: distinct attempts under same delegation
    // get distinct refs (legacy bug overwrote the shared session ref).
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let session = "550e8400-e29b-41d4-a716-446655440000";
    let delegation = "deadbeef-1111-2222-3333-444455556666";
    let k1 = key(session, delegation, 1);
    let k2 = key(session, delegation, 2);

    store.put(&k1, b"first attempt", &metadata(b"first attempt")).await.unwrap();
    store.put(&k2, b"second attempt", &metadata(b"second attempt")).await.unwrap();

    let g1 = store.get(&k1, None).await.unwrap();
    let g2 = store.get(&k2, None).await.unwrap();
    assert_eq!(g1.bytes, b"first attempt");
    assert_eq!(g2.bytes, b"second attempt");
}
```

- [ ] **Step 5: Run tests**

Run: `cargo check -p spur-worktree`
Expected: clean.

Run: `cargo test -p spur-worktree --test git_blob_store_impl`
Expected: 6 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-worktree/Cargo.toml crates/spur-worktree/src/lib.rs crates/spur-worktree/src/git_blob_store.rs crates/spur-worktree/tests/git_blob_store_impl.rs
git commit -m "feat(spur-worktree): GitBlobOutcomeStore impl

Git-backed OutcomeStore. Lives in spur-worktree (which already owns
git ref ops) and depends on spur-blob-store for the trait.

Ref namespace (Round 11 MF1+MF2):
  refs/spur/outcomes/<session>/<delegation>-<attempt>.{blob,meta}

Per-(session, delegation, attempt) granularity — the legacy
refs/spur/artifacts/<session> ref overwrites shared per session. Both
new refs are leaves; no D/F conflict with the legacy ref. Legacy ref
is also deleted on namespace teardown (cleanup of pre-Plan-5 debt).

UUID validation guards put + delete_namespace (Round 11 SF3 — defense
against shell-meta in git update-ref).

Phase 2 of plan-5; spec §6.4."
```

---

## Task 9: Wire orchestrator's persist callsite through `OutcomeStore::put`

**Files:**
- Modify: `crates/spur-core/Cargo.toml` (add `spur-blob-store` workspace dep)
- Modify: `crates/spur-core/src/orchestrator.rs:4755-4778` (callsite migration)

**What:** Switch the orchestrator's `worktrees.persist_artifact` call to `OutcomeStore::put` via `GitBlobOutcomeStore` + the `as_worker_artifact` adapter. Preserves the observable `DelegationResult.artifact: Option<WorkerArtifact>` shape (MF4 backcompat). The `worktrees.persist_artifact` method retains its public signature for callers we haven't migrated yet.

- [ ] **Step 1: Add the workspace dep**

Run: `grep -n "^spur-acp\|^spur-worktree\|^\\[dependencies\\]" crates/spur-core/Cargo.toml | head -5`

Add (alphabetical):
```toml
spur-blob-store = { workspace = true }
```

- [ ] **Step 2: Locate the existing call site and the orchestrator's `worktrees` handle**

Run: `sed -n '4740,4780p' crates/spur-core/src/orchestrator.rs`

Note the surrounding code: `worktrees: Arc<WorktreeManager>`, `worker_session: SessionId`, `output_text: String`, `worker_success: bool`, `summary_cap_bytes()`. The brain session id is in scope as `brain_session_id` (or similar — verify by grepping).

Run: `grep -n "brain_session_id\|delegation_id\|attempt" crates/spur-core/src/orchestrator.rs | head -20`

Identify the variable names actually in scope at line 4755. They will be needed for the `OutcomeKey` construction.

- [ ] **Step 3: Replace the call site**

Edit `crates/spur-core/src/orchestrator.rs` — find lines `4755-4778` (the `let persist_result = if output_text.len() > summary_cap_bytes() { ... } else { None };` block).

Replace with:

```rust
let persist_result: Option<Result<spur_acp::WorkerArtifact, String>> =
    if output_text.len() > summary_cap_bytes() {
        let kind = if worker_success {
            spur_acp::ArtifactKind::Output
        } else {
            spur_acp::ArtifactKind::Diagnostic
        };

        // Phase 2 of plan-5: route through OutcomeStore::put. The
        // GitBlobOutcomeStore writes to refs/spur/outcomes/<session>/
        // <delegation>-<attempt>.{blob,meta} (Round 11 MF1+MF2) and
        // returns an OutcomeRef which we project into the legacy
        // WorkerArtifact shape via as_worker_artifact (MF4 backcompat).
        let store = spur_worktree::git_blob_store::GitBlobOutcomeStore::new(
            worktrees.repo_root.clone(),
        );
        let key = spur_acp::domain::outcome::OutcomeKey {
            brain_session_id: brain_session_id.clone(),
            delegation_id: delegation_id.clone(),
            attempt: current_attempt,
        };
        let content = output_text.as_bytes();
        let sha = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(content);
            let d = h.finalize();
            let mut s = String::with_capacity(64);
            for b in d {
                use std::fmt::Write;
                write!(&mut s, "{b:02x}").expect("hex write infallible");
            }
            s
        };
        let metadata = spur_blob_store::OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: spur_blob_store::ContentType::Stdout,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha,
        };

        match <spur_worktree::git_blob_store::GitBlobOutcomeStore as spur_blob_store::OutcomeStore>::put(
            &store, &key, content, &metadata,
        )
        .await
        {
            Ok(outcome_ref) => match outcome_ref.as_worker_artifact(kind) {
                Some(wa) => Some(Ok(wa)),
                None => {
                    tracing::warn!(
                        session = %worker_session,
                        "OutcomeStore::put returned non-git backend; falling back to legacy persist_artifact"
                    );
                    match worktrees
                        .persist_artifact(&worker_session, &output_text, kind)
                        .await
                    {
                        Ok(a) => Some(Ok(a)),
                        Err(e) => Some(Err(e.to_string())),
                    }
                }
            },
            Err(e) => {
                tracing::warn!(
                    session = %worker_session,
                    error = %e,
                    "OutcomeStore::put failed; falling back to legacy persist_artifact"
                );
                match worktrees
                    .persist_artifact(&worker_session, &output_text, kind)
                    .await
                {
                    Ok(a) => Some(Ok(a)),
                    Err(e) => Some(Err(e.to_string())),
                }
            }
        }
    } else {
        None
    };
```

**Note about variable names:** the snippet uses `brain_session_id`, `delegation_id`, `current_attempt`. Inspect the actual surrounding code via Step 2 — these names are likely accurate but the orchestrator's local variable names may differ slightly (e.g., `attempt` instead of `current_attempt`). Adjust to match the in-scope variable names. Do NOT silently rename callers' variables; just use whichever symbol IS in scope.

- [ ] **Step 4: Verify `worktrees.repo_root` is accessible**

Run: `grep -n "pub.*repo_root\|repo_root:" crates/spur-worktree/src/manager.rs | head -5`

If `repo_root` is private, add a `pub fn repo_root(&self) -> &std::path::Path { &self.repo_root }` accessor in the same impl block. Update the snippet in Step 3 to use `worktrees.repo_root().to_path_buf()`.

- [ ] **Step 5: Run cargo check**

Run: `cargo check -p spur-core`
Expected: clean. Compile errors most likely indicate variable-name or visibility differences — adjust the snippet, do NOT silence the compiler.

- [ ] **Step 6: Run the orchestrator's existing test suite**

Run: `cargo test -p spur-core`
Expected: green. The orchestrator tests primarily exercise the high-level pipeline; the wrapped persist path produces byte-equivalent `DelegationResult.artifact` so existing assertions should hold.

If a test fails because of a behavioral difference (e.g., different error message format), inspect carefully: does the test assert on Phase 1 invariants we've now changed? If yes, the test needs updating; if no, the change introduced a regression.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/Cargo.toml crates/spur-core/src/orchestrator.rs crates/spur-worktree/src/manager.rs
git commit -m "feat(spur-core): route persist through OutcomeStore::put (Phase 2)

Switch the orchestrator's oversized-stdout persist path from a direct
worktrees.persist_artifact call to OutcomeStore::put via
GitBlobOutcomeStore. The OutcomeRef is projected back into the legacy
WorkerArtifact shape via as_worker_artifact (MF4 backcompat) so
DelegationResult.artifact remains byte-equivalent.

worktrees.persist_artifact retains its public signature for callers we
haven't migrated yet; it also serves as a fallback path on the
unlikely OutcomeStore::put failure (defense in depth).

Phase 2 of plan-5; spec §6.5."
```

---

## Task 10: Phase 2 verification — workspace test pass + property tests

**Files:**
- Create: `crates/spur-blob-store/tests/proptest_invariants.rs`

**What:** Run the full workspace test suite; confirm clippy is clean; add property-based tests for the in-process stores (256 cases) verifying invariants the unit tests don't reach.

- [ ] **Step 1: Write `crates/spur-blob-store/tests/proptest_invariants.rs`**

```rust
//! Property-based invariants for in-process OutcomeStore impls.
//! 256 cases per proptest; binary content + control chars in scope.

use chrono::Utc;
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use spur_acp::{BrainSessionId, SessionId};
use spur_blob_store::{
    ContentType, MemoryOutcomeStore, OutcomeKey, OutcomeMetadata, OutcomeStore,
};

fn sha256_hex(content: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(content);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

fn arb_uuid() -> impl Strategy<Value = String> {
    // UUIDs from a small pool so collisions are likely (exercise idempotence).
    prop_oneof![
        Just("550e8400-e29b-41d4-a716-446655440000".to_string()),
        Just("550e8400-e29b-41d4-a716-aaaaaaaaaaaa".to_string()),
        Just("550e8400-e29b-41d4-a716-bbbbbbbbbbbb".to_string()),
    ]
}

fn arb_content() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..2048)
}

fn arb_attempt() -> impl Strategy<Value = u32> {
    1u32..5
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

    #[test]
    fn put_then_get_round_trips(
        session in arb_uuid(),
        delegation in arb_uuid(),
        attempt in arb_attempt(),
        content in arb_content(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = MemoryOutcomeStore::new();
            let key = OutcomeKey {
                brain_session_id: BrainSessionId::new(SessionId(session)),
                delegation_id: delegation.into(),
                attempt,
            };
            let metadata = OutcomeMetadata {
                created_at: Utc::now(),
                content_type: ContentType::Stdout,
                original_byte_size: content.len() as u64,
                stored_byte_size: content.len() as u64,
                sha256: sha256_hex(&content),
            };
            let r = store.put(&key, &content, &metadata).await.expect("put");
            let got = store.get(&key, None).await.expect("get");
            prop_assert_eq!(got.bytes, content);
            prop_assert_eq!(got.metadata.sha256, r.sha256);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn idempotent_double_put_returns_equivalent_ref(
        session in arb_uuid(),
        delegation in arb_uuid(),
        attempt in arb_attempt(),
        content in arb_content(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = MemoryOutcomeStore::new();
            let key = OutcomeKey {
                brain_session_id: BrainSessionId::new(SessionId(session)),
                delegation_id: delegation.into(),
                attempt,
            };
            let metadata = OutcomeMetadata {
                created_at: Utc::now(),
                content_type: ContentType::Stdout,
                original_byte_size: content.len() as u64,
                stored_byte_size: content.len() as u64,
                sha256: sha256_hex(&content),
            };
            let a = store.put(&key, &content, &metadata).await.expect("first put");
            let b = store.put(&key, &content, &metadata).await.expect("second put");
            prop_assert_eq!(a.sha256, b.sha256);
            prop_assert_eq!(a.byte_size, b.byte_size);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn delete_namespace_only_removes_that_session(
        session_a in arb_uuid(),
        session_b in arb_uuid(),
        delegation in arb_uuid(),
        content in arb_content(),
    ) {
        prop_assume!(session_a != session_b);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = MemoryOutcomeStore::new();
            let metadata = OutcomeMetadata {
                created_at: Utc::now(),
                content_type: ContentType::Stdout,
                original_byte_size: content.len() as u64,
                stored_byte_size: content.len() as u64,
                sha256: sha256_hex(&content),
            };
            let k_a = OutcomeKey {
                brain_session_id: BrainSessionId::new(SessionId(session_a.clone())),
                delegation_id: delegation.clone().into(),
                attempt: 1,
            };
            let k_b = OutcomeKey {
                brain_session_id: BrainSessionId::new(SessionId(session_b.clone())),
                delegation_id: delegation.into(),
                attempt: 1,
            };
            store.put(&k_a, &content, &metadata).await.unwrap();
            store.put(&k_b, &content, &metadata).await.unwrap();
            store.delete_namespace(&BrainSessionId::new(SessionId(session_a))).await.unwrap();
            prop_assert!(store.get(&k_a, None).await.is_err());
            prop_assert!(store.get(&k_b, None).await.is_ok());
            Ok(())
        }).unwrap();
    }
}
```

- [ ] **Step 2: Run the property tests**

Run: `cargo test -p spur-blob-store --test proptest_invariants`
Expected: 3 proptests pass (256 cases each).

- [ ] **Step 3: Run cargo check across the workspace**

Run: `cargo check --workspace`
Expected: clean.

- [ ] **Step 4: Run cargo clippy with -D warnings on affected crates**

Run: `cargo clippy -p spur-acp -p spur-blob-store -p spur-worktree -p spur-core -- -D warnings`
Expected: no warnings.

If warnings surface in code Phase 2 modified, fix them at the source. Do NOT add `#[allow(...)]` to silence them.

- [ ] **Step 5: Run the workspace test suite**

Run: `cargo test --workspace`
Expected: all green.

If specific tests fail because of disk space or sandbox limits, narrow the run to affected crates: `cargo test -p spur-acp -p spur-blob-store -p spur-worktree -p spur-core`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-blob-store/tests/proptest_invariants.rs
git commit -m "test(spur-blob-store): proptest invariants for in-process stores

256 cases per property:
- put_then_get_round_trips: content survives serialize+restore.
- idempotent_double_put_returns_equivalent_ref: same key+content =>
  same OutcomeRef (no rewrite, same sha).
- delete_namespace_only_removes_that_session: cross-session isolation.

Random keys (small UUID pool to exercise collisions) + random binary
content (incl. control chars). No panics, no leaked state.

Phase 2 of plan-5; spec §6.6."
```

- [ ] **Step 7: Document Phase 2 completion**

Append a status line to this plan file:

```bash
echo "" >> docs/superpowers/plans/2026-04-25-brain-continuation-phase-2-blob-store.md
echo "**Status:** Implementation complete on $(git log -1 --format=%cd --date=short HEAD). Final commit: $(git rev-parse --short HEAD)." >> docs/superpowers/plans/2026-04-25-brain-continuation-phase-2-blob-store.md
git add docs/superpowers/plans/2026-04-25-brain-continuation-phase-2-blob-store.md
git commit -m "docs(plan): mark brain-continuation phase 2 implementation complete"
```

---

## Verification Checklist

Use this list to confirm Phase 2 meets spec §6:

- [ ] New crate `spur-blob-store` exists; `cargo check -p spur-blob-store` is clean.
- [ ] Wire-shape types (`OutcomeKey`, `OutcomeRef`, `BackendTag`) live in `spur-acp::domain::outcome` (MF1 — avoids cycle).
- [ ] `BackendTag` is **not** `Copy` (Round 9 P2-S1).
- [ ] `OutcomeStore` trait has `put`, `get`, `delete_namespace`, `sweep_older_than`.
- [ ] `OutcomeMetadata.sha256` is the single source of truth (Round 11 SF1).
- [ ] `MemoryOutcomeStore`, `FsOutcomeStore`, `MeasuredOutcomeStore` impls exist with full unit tests.
- [ ] `GitBlobOutcomeStore` lives in `spur-worktree`; depends on `spur-blob-store`.
- [ ] Git ref namespace is `refs/spur/outcomes/<session>/<delegation>-<attempt>.{blob,meta}` (Round 11 MF1+MF2 — leaf refs, no D/F conflict).
- [ ] UUID validation guards `put` and `delete_namespace` on both `FsOutcomeStore` and `GitBlobOutcomeStore`.
- [ ] `ContentMismatch` semantics: differing content under same key returns `StoreError::ContentMismatch` without overwrite; matching content is idempotent.
- [ ] `OutcomeRef::as_worker_artifact` returns `Some(...)` for `BackendTag::GitBlob`, `None` for other backends; uses the new namespace path.
- [ ] Orchestrator's `persist_artifact` callsite at `orchestrator.rs:4755-4778` routes through `OutcomeStore::put`.
- [ ] `DelegationResult.artifact: Option<WorkerArtifact>` shape unchanged on the wire.
- [ ] Property tests cover idempotence + namespace isolation + roundtrip (256 cases).
- [ ] Workspace `cargo test --workspace` is green.
- [ ] Workspace clippy is clean with `-D warnings`.

---

## Out of Scope for Phase 2

The following are explicitly NOT in Phase 2 — they belong to Phase 3 (next plan):

- `ContinuationPayload` schema bump to v3 (`artifact_id: Option<OutcomeKey>`, `estimated_cost_micros`, `fetch_hint`).
- `OutcomeMaterializer` (the single-producer that clips success path + persists full result).
- `clip_status_strings` / `clip_diff_files` / `clip_artifact_ref_strings` helpers.
- Section pagination beyond `Section::Full` (StatusOnly / Summary / DiffOnly).
- `attempt: Option<u32>` argument widening on the `fetch_outcome_artifact` MCP tool.
- Beads audit-comment artifact_uri field.
- INV-D8 (no oversized inline drops) materializer-boundary enforcement test.
- GC/TTL background sweeper task wiring (Phase 2 just provides `sweep_older_than`).
- Truncation-ladder fallback at the materializer.
- Removal of `worktrees.persist_artifact` (Phase 3 cleanup).

---

## Self-Review

**Spec coverage:**
- §6.1 (new crate skeleton) → Task 1
- §6.2 (Cargo.toml) → Task 1 Step 2
- §6.3 (public API + type ownership split) → Tasks 2, 3
- §6.4 (Fs / Memory / Git / Measured impls + new ref namespace + UUID validation + ContentMismatch) → Tasks 4, 5, 6, 8
- §6.5 (orchestrator migration + backcompat adapter) → Tasks 7, 9
- §6.6 (tests: unit + integration + cross-crate + proptest) → Tasks 4–8 (unit + integration), Task 10 (proptest)
- §6.7 (migration / rollback) → Task 9 (additive; one-commit revert)

**Type consistency:** `OutcomeKey`, `OutcomeRef`, `BackendTag`, `OutcomeMetadata`, `OutcomeContent`, `Section`, `StoreError`, `SweepReport`, `OutcomeStore`, `ContentType` — all referenced consistently across Tasks 1–10. The `as_worker_artifact` adapter signature `(self, kind: WorkerArtifactKind) -> Option<WorkerArtifact>` is identical in Tasks 7 and 9. The git ref format `refs/spur/outcomes/<session>/<delegation>-<attempt>.blob` appears verbatim in Tasks 7 (adapter), 8 (impl), and the test fixtures.

**Placeholder scan:** No `TBD`, `TODO`, `add appropriate error handling`, or "similar to Task N" patterns.
