# Brain Continuation Phase 1 — Brain-Visible Fetch Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair the latent metadata-loss bug in `map_worker_artifact_ref` and ship the `fetch_outcome_artifact` MCP tool so the brain can read existing oversized-stdout artifacts. No new crate; additive wire schema; brain-visible value end-to-end.

**Architecture:** Phase 1 of three. Extends `ArtifactRef` with two `Option<String>` git-blob fields (preserving `#[serde(flatten)]` on `kind` to avoid wire breakage), updates the producer to populate them, and adds an MCP tool that resolves the new fields via `git cat-file -p <blob_sha>` against the brain session's repo root. Persist-before-publish ordering already holds in current code; documented and tested. Phase 2 (`spur-blob-store` crate) and Phase 3 (lean schema v3 + `OutcomeMaterializer`) are separate plans.

**Tech Stack:** Rust workspace (`spur-acp`, `spur-mcp`), serde, tokio, JSON-RPC 2.0 (existing MCP transport), git CLI (already wired via `run_git_capture`).

**Spec:** `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §5 (Phase 1).

---

## File Structure

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/domain/continuation.rs` | Modify (lines 43-52) | Extend `ArtifactRef` with `git_object_ref` + `git_blob_sha` |
| `crates/spur-acp/tests/data/artifact_ref_v0.json` | Create | Golden fixture: pre-Phase-1 wire shape |
| `crates/spur-acp/tests/artifact_ref_wire_compat.rs` | Create | Round-trip test locking `flatten` attribute (Patch + Other variants) |
| `crates/spur-mcp/src/server.rs` | Modify (~lines 237-249, ~2177) | Update `map_worker_artifact_ref`; add `fetch_outcome_artifact` handler + dispatch |
| `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs` | Create | End-to-end test against a tempfile git repo |

Phase 1's surface is small by design — three modifications, two new files. The fetch tool reads the brain server's `repo_root` and invokes existing `run_git_capture` helpers.

---

## Task 1: Extend `ArtifactRef` with git-blob metadata fields

**Files:**
- Modify: `crates/spur-acp/src/domain/continuation.rs:43-52`

**What:** Add `git_object_ref: Option<String>` and `git_blob_sha: Option<String>`. The existing `#[serde(flatten)] pub kind: ArtifactKind` MUST be preserved — without it, the wire shape changes from `{"kind":"patch","uri":...}` to `{"kind":{"kind":"patch"},...}`.

- [ ] **Step 1: Read the current ArtifactRef definition**

Run: `sed -n '43,52p' crates/spur-acp/src/domain/continuation.rs`

Expected output:
```
/// Reference to a persisted continuation artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    #[serde(flatten)]
    pub kind: ArtifactKind,
    pub uri: String,
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}
```

- [ ] **Step 2: Replace ArtifactRef definition with the extended shape**

Edit `crates/spur-acp/src/domain/continuation.rs`, replacing lines 43-52:

```rust
/// Reference to a persisted continuation artifact.
///
/// **INVARIANT:** the `#[serde(flatten)]` attribute on `kind` is mandatory.
/// Removing it changes the wire shape from `{"kind":"patch","uri":...}` to
/// `{"kind":{"kind":"patch"},...}`. The golden round-trip test in
/// `crates/spur-acp/tests/artifact_ref_wire_compat.rs` enforces this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    #[serde(flatten)]
    pub kind: ArtifactKind,
    pub uri: String,
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Git ref path (e.g., `"refs/spur/artifacts/<session>"`) when stored as a git blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_object_ref: Option<String>,
    /// 40-char hex SHA-1 of the git blob; survives ref deletion until git GC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_blob_sha: Option<String>,
}
```

- [ ] **Step 3: Run cargo check on spur-acp**

Run: `cargo check -p spur-acp`
Expected: clean exit (no errors). Existing tests using `ArtifactRef { kind, uri, byte_size, sha256 }` literals will compile because new fields default via `#[serde(default)]` and the struct fields have full visibility — but the literals will fail at the `..` rest-pattern step. **There is no rest pattern; literals must include all fields.** Tests touching `ArtifactRef` literals now need `git_object_ref: None, git_blob_sha: None` appended. cargo check will surface them.

- [ ] **Step 4: Update existing test literals to include the new fields**

Run: `grep -rn "ArtifactRef {" crates/spur-acp/src/`

Expected matches inside the existing test module at `continuation.rs:159` and `continuation.rs:196`. For each, add `git_object_ref: None, git_blob_sha: None,` after `sha256: ...`. Example for `continuation.rs:159` (inside `continuation_payload_builds_from_parts`):

```rust
artifact_ref: Some(ArtifactRef {
    kind: ArtifactKind::Patch,
    uri: "spur://artifact/abc".into(),
    byte_size: 42,
    sha256: Some("a".repeat(64)),
    git_object_ref: None,
    git_blob_sha: None,
}),
```

Apply the same change at `continuation.rs:196` (inside `brain_continuation_round_trips_wire_fields_and_refreshes_created_at_mono`).

- [ ] **Step 5: Run all spur-acp tests**

Run: `cargo test -p spur-acp`
Expected: all existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/continuation.rs
git commit -m "feat(spur-acp): extend ArtifactRef with git-blob metadata fields

Add git_object_ref and git_blob_sha as additive Option<String> fields
with #[serde(default, skip_serializing_if)] so existing wire payloads
deserialize unchanged. Doc comment marks the existing #[serde(flatten)]
attribute on \`kind\` as a wire-shape invariant — round-trip CI test
will follow in the next task.

Phase 1 of plan-5 brain-continuation artifact store
(docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md)."
```

---

## Task 2: Golden round-trip test locking `#[serde(flatten)]`

**Files:**
- Create: `crates/spur-acp/tests/data/artifact_ref_v0.json`
- Create: `crates/spur-acp/tests/artifact_ref_wire_compat.rs`

**What:** Write a fixture file with the pre-Phase-1 wire shape (no `git_object_ref`/`git_blob_sha`) plus a test that deserializes, re-serializes, and asserts the `kind` flat shape survives. Per spec §5.1 round-9 NIT — fixture must include BOTH a unit variant (`Patch`) AND `Other(String)` to lock both the top-level discriminator and the `name` projection.

- [ ] **Step 1: Create the golden fixture directory**

Run: `mkdir -p crates/spur-acp/tests/data`
Expected: directory exists silently.

- [ ] **Step 2: Write the golden fixture file**

Create `crates/spur-acp/tests/data/artifact_ref_v0.json`:

```json
{
  "patch_unit": {
    "kind": "patch",
    "uri": "spur://artifact/uuid-1",
    "byte_size": 1024,
    "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
  },
  "other_named": {
    "kind": "other",
    "name": "worker_artifact",
    "uri": "spur://artifact/uuid-2",
    "byte_size": 2048
  }
}
```

The `patch_unit` entry locks the unit variant's flat discriminator (`"kind":"patch"`, no nested object). The `other_named` entry locks the `Other(String)` variant's `kind` + `name` shape.

- [ ] **Step 3: Write the round-trip test**

Create `crates/spur-acp/tests/artifact_ref_wire_compat.rs`:

```rust
//! Golden round-trip test: `ArtifactRef` wire format must remain stable
//! across releases. The `#[serde(flatten)]` attribute on `kind` is a
//! wire-shape invariant — removing it changes the on-the-wire JSON from
//! `{"kind":"patch",...}` to `{"kind":{"kind":"patch"},...}`.
//!
//! This test reads a frozen golden fixture, deserializes into the current
//! `ArtifactRef` shape, re-serializes, and asserts that the structural
//! `kind` projection still produces the flat shape.

use serde_json::{json, Value};
use spur_acp::domain::ArtifactRef;

fn load_fixture() -> Value {
    let raw = include_str!("data/artifact_ref_v0.json");
    serde_json::from_str(raw).expect("fixture must parse")
}

#[test]
fn patch_unit_variant_round_trips_with_flat_kind() {
    let fixture = load_fixture();
    let payload = fixture
        .get("patch_unit")
        .cloned()
        .expect("patch_unit entry");

    let parsed: ArtifactRef = serde_json::from_value(payload.clone())
        .expect("patch_unit must deserialize into current ArtifactRef shape");

    let reserialized = serde_json::to_value(&parsed).expect("re-serialize");

    assert_eq!(
        reserialized.get("kind"),
        Some(&Value::String("patch".into())),
        "kind must remain a flat string on the wire (#[serde(flatten)] preserved)"
    );
    assert_eq!(reserialized.get("uri"), payload.get("uri"));
    assert_eq!(reserialized.get("byte_size"), payload.get("byte_size"));
    assert_eq!(reserialized.get("sha256"), payload.get("sha256"));
}

#[test]
fn other_named_variant_round_trips_with_flat_name_projection() {
    let fixture = load_fixture();
    let payload = fixture
        .get("other_named")
        .cloned()
        .expect("other_named entry");

    let parsed: ArtifactRef = serde_json::from_value(payload.clone())
        .expect("other_named must deserialize into current ArtifactRef shape");

    let reserialized = serde_json::to_value(&parsed).expect("re-serialize");

    assert_eq!(
        reserialized.get("kind"),
        Some(&Value::String("other".into())),
        "kind discriminator stays flat for data-carrying variants"
    );
    assert_eq!(
        reserialized.get("name"),
        Some(&Value::String("worker_artifact".into())),
        "Other variant's String payload surfaces as a sibling \"name\" field"
    );
}

#[test]
fn old_payloads_without_new_fields_deserialize_with_none() {
    // Phase 1 added git_object_ref + git_blob_sha as additive Option fields.
    // Pre-Phase-1 payloads (the golden fixture) must yield None for them.
    let fixture = load_fixture();
    let payload = fixture.get("patch_unit").cloned().unwrap();

    let parsed: ArtifactRef = serde_json::from_value(payload).expect("deserialize");

    assert!(parsed.git_object_ref.is_none());
    assert!(parsed.git_blob_sha.is_none());
}

#[test]
fn new_payloads_with_optional_fields_omit_them_when_none() {
    let fresh = ArtifactRef {
        kind: spur_acp::domain::continuation::ArtifactKind::Patch,
        uri: "spur://artifact/x".into(),
        byte_size: 0,
        sha256: None,
        git_object_ref: None,
        git_blob_sha: None,
    };

    let serialized = serde_json::to_value(&fresh).unwrap();
    assert!(serialized.get("git_object_ref").is_none(), "None field must be omitted");
    assert!(serialized.get("git_blob_sha").is_none());
    assert!(serialized.get("sha256").is_none());

    // But the new shape with values populated produces both new fields.
    let with_meta = ArtifactRef {
        git_object_ref: Some("refs/spur/artifacts/sess-1".into()),
        git_blob_sha: Some("a".repeat(40)),
        ..fresh
    };
    let serialized2 = serde_json::to_value(&with_meta).unwrap();
    assert_eq!(
        serialized2.get("git_object_ref"),
        Some(&json!("refs/spur/artifacts/sess-1"))
    );
    assert_eq!(serialized2.get("git_blob_sha"), Some(&json!("a".repeat(40))));
}
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test -p spur-acp --test artifact_ref_wire_compat`
Expected: 4 passed; 0 failed.

- [ ] **Step 5: Verify the test catches `#[serde(flatten)]` removal (manual sanity)**

Temporarily edit `crates/spur-acp/src/domain/continuation.rs` to remove `#[serde(flatten)]` (delete line 46). Run: `cargo test -p spur-acp --test artifact_ref_wire_compat`
Expected: `patch_unit_variant_round_trips_with_flat_kind` FAILS with "kind must remain a flat string on the wire".

Restore the `#[serde(flatten)]` line. Re-run the test. Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/tests/data/artifact_ref_v0.json crates/spur-acp/tests/artifact_ref_wire_compat.rs
git commit -m "test(spur-acp): golden round-trip test for ArtifactRef wire shape

Locks the #[serde(flatten)] attribute on ArtifactRef::kind. Fixture
covers both the unit variant (Patch -> flat \"kind\":\"patch\") and the
data-carrying variant (Other(String) -> flat \"kind\":\"other\" plus
sibling \"name\":\"<value>\"). Tests also verify pre-Phase-1 payloads
deserialize cleanly (additive new fields default to None) and that
None values are omitted on re-serialize.

Phase 1 of plan-5; spec §5.1 round-9 NIT (golden fixture coverage)."
```

---

## Task 3: Update `map_worker_artifact_ref` to populate git-blob metadata

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:237-249`

**What:** Replace the hard-coded `sha256: None` and missing-fields shape with values pulled from the source `WorkerArtifact { object_ref, blob_sha, ... }`. The existing `WorkerArtifact` carries both fields; the bug is purely in the mapping function dropping them.

- [ ] **Step 1: Read the current mapping function**

Run: `sed -n '237,249p' crates/spur-mcp/src/server.rs`

Expected output:
```rust
fn map_worker_artifact_ref(
    delegation_id: &DelegationId,
    artifact: Option<&spur_acp::domain::artifact::WorkerArtifact>,
) -> Option<spur_acp::domain::ArtifactRef> {
    use spur_acp::domain::continuation::ArtifactKind as ContinuationArtifactKind;

    artifact.map(|artifact| spur_acp::domain::ArtifactRef {
        kind: ContinuationArtifactKind::Other("worker_artifact".into()),
        uri: format!("spur://artifact/{}", delegation_id.as_str()),
        byte_size: artifact.size_bytes as u64,
        sha256: None,
    })
}
```

- [ ] **Step 2: Replace the function body**

Edit `crates/spur-mcp/src/server.rs:237-249`:

```rust
fn map_worker_artifact_ref(
    delegation_id: &DelegationId,
    artifact: Option<&spur_acp::domain::artifact::WorkerArtifact>,
) -> Option<spur_acp::domain::ArtifactRef> {
    use spur_acp::domain::continuation::ArtifactKind as ContinuationArtifactKind;

    artifact.map(|artifact| spur_acp::domain::ArtifactRef {
        kind: ContinuationArtifactKind::Other("worker_artifact".into()),
        uri: format!("spur://artifact/{}", delegation_id.as_str()),
        byte_size: artifact.size_bytes as u64,
        // Phase 1 fix: WorkerArtifact carries blob_sha (40-char hex) which
        // serves as the SHA-1 git blob identifier. Populate sha256 with it
        // for now; in Phase 3 OutcomeMetadata.sha256 will carry SHA-256
        // separately. Until then, the field name is historical.
        sha256: Some(artifact.blob_sha.clone()),
        git_object_ref: Some(artifact.object_ref.clone()),
        git_blob_sha: Some(artifact.blob_sha.clone()),
    })
}
```

- [ ] **Step 3: Update existing test literals in the same file**

Run: `grep -n "ArtifactRef {" crates/spur-mcp/src/server.rs`

For each match, add `git_object_ref: None, git_blob_sha: None,` after the `sha256` field. The `#[cfg(test)]` test helpers around line 4500-4700 likely contain these.

- [ ] **Step 4: Add a unit test for the mapping**

Append to the existing `#[cfg(test)] mod continuation_producer_tests` (currently around line 4400-4900) of `crates/spur-mcp/src/server.rs`. If the module isn't easily found, use:

Run: `grep -n "mod continuation_producer_tests" crates/spur-mcp/src/server.rs`

Add this test inside that module (after existing tests):

```rust
#[test]
fn map_worker_artifact_ref_preserves_git_metadata() {
    use spur_acp::domain::artifact::{ArtifactKind as WorkerArtifactKind, WorkerArtifact};

    let delegation_id: DelegationId = "uuid-test".into();
    let worker = WorkerArtifact {
        object_ref: "refs/spur/artifacts/sess-abc".into(),
        blob_sha: "a".repeat(40),
        size_bytes: 12345,
        kind: WorkerArtifactKind::Output,
    };

    let mapped = super::map_worker_artifact_ref(&delegation_id, Some(&worker)).unwrap();

    assert_eq!(
        mapped.git_object_ref.as_deref(),
        Some("refs/spur/artifacts/sess-abc"),
        "git_object_ref must survive the mapping (Phase 1 bug fix)"
    );
    assert_eq!(
        mapped.git_blob_sha.as_deref(),
        Some(&*"a".repeat(40)),
        "git_blob_sha must survive the mapping"
    );
    assert_eq!(mapped.byte_size, 12345);
    assert!(mapped.uri.starts_with("spur://artifact/"));
}
```

- [ ] **Step 5: Run cargo check + tests**

Run: `cargo check -p spur-mcp`
Expected: clean.

Run: `cargo test -p spur-mcp --lib map_worker_artifact_ref_preserves_git_metadata`
Expected: 1 passed.

Run: `cargo test -p spur-mcp` (full suite — surfaces test-helper literal updates)
Expected: all passing.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "fix(spur-mcp): preserve git_object_ref + git_blob_sha in ArtifactRef mapping

map_worker_artifact_ref previously dropped the WorkerArtifact's
object_ref and blob_sha, leaving the brain with a spur://artifact/<id>
URI it could not resolve. Phase 1 fix populates both new fields plus
sha256 (using blob_sha as the SHA-1 identifier until Phase 3 introduces
OutcomeMetadata with SHA-256).

Phase 1 of plan-5; spec §5.1."
```

---

## Task 4: Add `fetch_outcome_artifact` MCP tool — handler + dispatch

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:2177` (dispatcher) and add new handler method on `McpCallbackServer`

**What:** Add a JSON-RPC tool that accepts `{ delegation_id, section? }` and returns the artifact text via `git cat-file -p <blob_sha>`. Authorization is implicit: the tool reads `self.brain_session_id` to validate the artifact belongs to this brain session. Phase 1's `section` parameter accepts `Some("full")` only; Phase 3 widens it.

- [ ] **Step 1: Locate the dispatch site**

Run: `grep -n 'match tool_name.as_str()' crates/spur-mcp/src/server.rs`
Expected: line ~2177.

- [ ] **Step 2: Add the dispatch arm**

Edit `crates/spur-mcp/src/server.rs` — find the line `"check_delegation_status" => self.handle_check_delegation_status(id, arguments).await,` (around line 2180). Add immediately AFTER it:

```rust
            "fetch_outcome_artifact" => self.handle_fetch_outcome_artifact(id, arguments).await,
```

- [ ] **Step 3: Add the handler method to `impl McpCallbackServer`**

Add this method to the `impl McpCallbackServer` block — placement-wise, immediately AFTER `handle_check_delegation_status` (around line 2715, before `handle_list_available_workers`). Use `grep -n "async fn handle_check_delegation_status" crates/spur-mcp/src/server.rs` to find the exact end of that method.

```rust
    /// Phase 1 of plan-5 brain-continuation artifact store.
    /// Reads an existing oversized-stdout artifact via the git-blob path
    /// previously stored by `worktrees.persist_artifact`. Authorization is
    /// scoped to `self.brain_session_id` — the caller does NOT supply
    /// `brain_session_id` as an argument (codex SF3, spec §5.2).
    ///
    /// Phase 1 args:
    ///   { "delegation_id": String, "section": Option<"full"> }  // default "full"
    ///
    /// Returns: { "content": [{ "type": "text", "text": <full text> }] }
    ///
    /// Future-compat: Phase 3 widens `section` to status_only|summary|diff_only|full
    /// and adds an `attempt: Option<u32>` arg. Phase 1 servers reject unknown
    /// section values with a clean error rather than a deserialization panic.
    async fn handle_fetch_outcome_artifact(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return JsonRpcResponse::invalid_params(id, "Missing or empty 'delegation_id'");
            }
        };

        // Phase 1: only "full" is supported. Anything else is a clean
        // InvalidParams response, NOT a serde deserialization error.
        match args.get("section").and_then(|v| v.as_str()) {
            None | Some("full") => {}
            Some(other) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("Phase 1 only supports section='full' (got '{other}'). Phase 3 will add: status_only, summary, diff_only."),
                );
            }
        }

        // Look up the artifact from completed_delegations. Persist-before-publish
        // ordering (current orchestrator: persist_artifact runs synchronously
        // before build_detached_continuation) guarantees that any delegation_id
        // the brain knows has already had its artifact persisted. NotFound is
        // reserved for hallucinated/wrong ids.
        let completed = self.completed_delegations.lock().await;
        let entry = match completed.get(&DelegationId::from(delegation_id.clone())) {
            Some((result, _ts)) => result.clone(),
            None => {
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("Outcome artifact not found for delegation_id={delegation_id}"),
                );
            }
        };
        drop(completed);

        let artifact = match entry.artifact.as_ref() {
            Some(a) => a,
            None => {
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("Delegation {delegation_id} has no side-channel artifact"),
                );
            }
        };

        // Resolve via git cat-file -p <blob_sha>. The brain server's
        // repo_root is the authoritative location; if missing, we cannot
        // resolve the blob.
        let repo_root = match &self.repo_root {
            Some(r) => r.clone(),
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "fetch_outcome_artifact requires repo_root to be configured",
                );
            }
        };

        let text = match run_git_capture(
            &repo_root,
            None,
            &["cat-file", "-p", artifact.blob_sha.as_str()],
        )
        .await
        {
            Ok(output) => output,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("git cat-file failed for blob {}: {error}", artifact.blob_sha),
                );
            }
        };

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{ "type": "text", "text": text }]
            }),
        )
    }
```

- [ ] **Step 4: Run cargo check**

Run: `cargo check -p spur-mcp`
Expected: clean compile. If `JsonRpcResponse::error` does not exist with that signature, fall back to `JsonRpcResponse::internal_error(id, format!(...))` and adjust. (Verify: `grep -n "impl JsonRpcResponse" crates/spur-mcp/src/server.rs` and inspect available constructors.)

- [ ] **Step 5: Run the spur-mcp test suite**

Run: `cargo test -p spur-mcp`
Expected: existing tests still pass. (No new tests yet — Task 5 covers them.)

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): add fetch_outcome_artifact MCP tool (Phase 1)

Read-only access to existing git-blob-backed WorkerArtifact payloads.
Authorization is scoped to self.brain_session_id (no caller-supplied
session id). Phase 1 supports section='full' only; unknown sections
return a clean InvalidParams error.

Resolves blobs via git cat-file -p <blob_sha> against the brain
server's repo_root. NotFound is reserved for hallucinated delegation
ids — persist-before-publish ordering guarantees known ids resolve.

Phase 1 of plan-5; spec §5.2."
```

---

## Task 5: End-to-end test for `fetch_outcome_artifact`

**Files:**
- Create: `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs`

**What:** Test the JSON-RPC tool against a tempfile git repo. Two tests: (a) successful round-trip — produce an oversized-stdout artifact, fetch via the tool, assert the returned text matches the original; (b) clean error on missing delegation_id (NotFound).

- [ ] **Step 1: Inspect existing integration-test patterns**

Run: `ls crates/spur-mcp/tests/`
Expected: existing tests like `epic_completion.rs`, `submit_plan_persist.rs`, `reconciler_tick.rs`.

Run: `head -40 crates/spur-mcp/tests/epic_completion.rs`
Note the imports and the McpCallbackServer test-construction pattern. Reuse it.

- [ ] **Step 2: Write the integration test file**

Create `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs`:

```rust
//! End-to-end test for `fetch_outcome_artifact` (Phase 1 of plan-5).
//!
//! Persists an oversized-stdout artifact via `worktrees.persist_artifact`,
//! injects a fake completed delegation into the MCP server's state, then
//! invokes the JSON-RPC tool and asserts the round-trip.

use serde_json::{json, Value};
use spur_acp::domain::artifact::{ArtifactKind, WorkerArtifact};
use spur_acp::domain::{DelegationResult, DelegationStatus};
use spur_acp::{BrainSessionId, DelegationId, SessionId};
use spur_mcp::server::McpCallbackServer;
use std::process::Command;
use tempfile::TempDir;
use tokio::time::Instant;

/// Initialize a bare git repo in `path` for hosting artifact refs.
fn init_repo(path: &std::path::Path) {
    let out = Command::new("git")
        .args(["init", "--quiet", "."])
        .current_dir(path)
        .output()
        .expect("git init");
    assert!(out.status.success(), "git init failed: {:?}", out);

    // Some hosts have init.defaultBranch=main; either way we just need
    // refs/spur/artifacts/* to be writable, which any initialized repo allows.
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(path)
        .output()
        .unwrap();
}

#[tokio::test]
async fn fetch_outcome_artifact_returns_persisted_blob_text() {
    // Setup: create a git repo and persist a sample artifact.
    let td = TempDir::new().unwrap();
    init_repo(td.path());

    let body = "line one\nline two\n".repeat(100);
    let session_id = "550e8400-e29b-41d4-a716-446655440000";
    let artifact = spur_worktree::artifact::persist(
        td.path(),
        session_id,
        &body,
        ArtifactKind::Output,
    )
    .await
    .expect("persist artifact");

    // Construct an MCP server with the temp-repo as repo_root.
    let mut server = McpCallbackServer::test_only_minimal(
        BrainSessionId::new(SessionId(session_id.into())),
        td.path().to_path_buf(),
    );

    // Inject a completed delegation pointing at the artifact.
    let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
    let result = DelegationResult {
        status: DelegationStatus::Success,
        summary: Some("ok".into()),
        diff_summary: None,
        worker_branch: None,
        artifact: Some(artifact.clone()),
    };
    server
        .test_only_inject_completed(delegation_id.clone(), result, Instant::now())
        .await;

    // Invoke the tool via the public dispatcher.
    let response = server
        .test_only_dispatch_tool(
            "fetch_outcome_artifact",
            json!({ "delegation_id": delegation_id.as_str() }),
        )
        .await;

    // Decode the response.
    let payload = response.success_payload().expect("expected success");
    let text = payload["content"][0]["text"].as_str().expect("text content");
    assert_eq!(text, body, "round-trip text must match the persisted body");
}

#[tokio::test]
async fn fetch_outcome_artifact_returns_clean_error_for_unknown_delegation() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());

    let mut server = McpCallbackServer::test_only_minimal(
        BrainSessionId::new(SessionId("any-session".into())),
        td.path().to_path_buf(),
    );

    let response = server
        .test_only_dispatch_tool(
            "fetch_outcome_artifact",
            json!({ "delegation_id": "nonexistent-delegation-id" }),
        )
        .await;

    let error = response.error_payload().expect("expected error");
    assert!(
        error.message.contains("not found"),
        "error message must mention not-found: {:?}",
        error
    );
}

#[tokio::test]
async fn fetch_outcome_artifact_rejects_unknown_section_cleanly() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());

    let mut server = McpCallbackServer::test_only_minimal(
        BrainSessionId::new(SessionId("any-session".into())),
        td.path().to_path_buf(),
    );

    let response = server
        .test_only_dispatch_tool(
            "fetch_outcome_artifact",
            json!({
                "delegation_id": "any-id",
                "section": "diff_only"  // Phase 3 value, not yet supported
            }),
        )
        .await;

    let error = response.error_payload().expect("expected InvalidParams error");
    assert!(
        error.message.contains("Phase 1 only supports section='full'"),
        "Phase 1 must reject unknown sections cleanly: {:?}",
        error
    );
}
```

- [ ] **Step 3: Add the test-only constructor and helpers to `McpCallbackServer`**

The integration test depends on three test-only helpers:
- `McpCallbackServer::test_only_minimal(brain_session_id, repo_root) -> Self`
- `test_only_inject_completed(delegation_id, result, ts)` (already exists; verify with `grep -n "test_only" crates/spur-mcp/src/server.rs`)
- `test_only_dispatch_tool(name, args) -> JsonRpcResponse` (or use `handle_tool_call` directly if public)

Verify what already exists. If `test_only_minimal` does not exist, add it inside `impl McpCallbackServer` gated by `#[cfg(any(test, feature = "test-helpers"))]`:

```rust
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn test_only_minimal(
        brain_session_id: spur_acp::BrainSessionId,
        repo_root: std::path::PathBuf,
    ) -> Self {
        // Construct the same minimal shape McpCallbackServer::new produces,
        // but without binding to a real listener. Mirrors the existing
        // test scaffolding pattern in continuation_producer_tests.
        let (delegation_tx, _delegation_rx) =
            tokio::sync::mpsc::channel::<crate::DelegationRequest>(8);
        Self {
            delegation_tx,
            workers: Vec::new(),
            brain_session_id,
            active_delegations: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            completed_delegations: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            task_tracker: tokio_util::task::TaskTracker::new(),
            pm_service: None,
            event_sink: None,
            // ... fill remaining fields with their Default::default() or None.
            // Use `grep -n "pub struct McpCallbackServer" crates/spur-mcp/src/server.rs`
            // and copy the field list; defaults are mostly None / empty.
            repo_root: Some(repo_root),
            ..Default::default()  // if Default is impled; otherwise enumerate
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn test_only_dispatch_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> crate::server::JsonRpcResponse {
        self.handle_tool_call(
            serde_json::Value::Number(1.into()),
            serde_json::json!({ "name": tool_name, "arguments": arguments }),
        )
        .await
    }
```

If `McpCallbackServer` does not derive or impl `Default`, enumerate every field explicitly (use `grep -n "pub struct McpCallbackServer" crates/spur-mcp/src/server.rs` then read 30 lines below to see the full struct).

Also add success/error payload helpers to `JsonRpcResponse` if missing:

```rust
    #[cfg(any(test, feature = "test-helpers"))]
    impl JsonRpcResponse {
        pub fn success_payload(&self) -> Option<&serde_json::Value> {
            // Inspect the `result` field on the response. Implementation
            // depends on JsonRpcResponse's actual shape; adjust accordingly.
            self.result.as_ref()
        }

        pub fn error_payload(&self) -> Option<&JsonRpcError> {
            self.error.as_ref()
        }
    }
```

(If `JsonRpcResponse` is an enum or has different shape, adapt the helpers to match. Inspect via `grep -n "pub struct JsonRpcResponse\|pub enum JsonRpcResponse" crates/spur-mcp/src/server.rs`.)

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p spur-mcp --test fetch_outcome_artifact_e2e`
Expected: 3 passed; 0 failed.

If a test fails because `test_only_minimal` doesn't compile (e.g., missing fields), iterate by adding each missing field with its sensible default. The rust-idioms skill applies: `cargo check` after every change; never wrap in `Arc<Mutex<T>>` to silence the borrow checker; understand the data flow.

- [ ] **Step 5: Run the full spur-mcp test suite to confirm no regressions**

Run: `cargo test -p spur-mcp`
Expected: all passing.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs crates/spur-mcp/src/server.rs
git commit -m "test(spur-mcp): end-to-end fetch_outcome_artifact integration tests

Three scenarios:
- successful round-trip of a persisted oversized-stdout blob via
  git cat-file -p
- clean NotFound error for unknown delegation_id
- clean InvalidParams error for Phase-3-only section values

Adds test_only_minimal and test_only_dispatch_tool helpers to
McpCallbackServer (gated by #[cfg(test)] / test-helpers feature).

Phase 1 of plan-5; spec §5.3."
```

---

## Task 6: Authorization scoping test (cross-session reads must be impossible)

**Files:**
- Modify: `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs`

**What:** Per spec §5.3, add an authorization test: even though the tool reads `self.brain_session_id` (not from args), verify that injecting a `DelegationResult.artifact` whose `object_ref` belongs to a DIFFERENT session does not expose its content. The brain server's `repo_root` is shared across sessions in tests, so we exercise the boundary by checking that `completed_delegations` is per-server (not global).

- [ ] **Step 1: Append the authorization test**

Append this test to `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs`:

```rust
#[tokio::test]
async fn fetch_outcome_artifact_completed_delegations_are_per_session() {
    // Two MCP servers in the same temp-repo, but each binds to its own
    // brain_session_id and its own completed_delegations map.
    // Server A persists an artifact for delegation_a; Server B sees no entry.
    let td = TempDir::new().unwrap();
    init_repo(td.path());

    let body = "secret stdout for session A".to_string();
    let session_a_id = "550e8400-e29b-41d4-a716-446655440000";
    let session_b_id = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";

    let artifact_a = spur_worktree::artifact::persist(
        td.path(),
        session_a_id,
        &body,
        ArtifactKind::Output,
    )
    .await
    .unwrap();

    let mut server_a = McpCallbackServer::test_only_minimal(
        BrainSessionId::new(SessionId(session_a_id.into())),
        td.path().to_path_buf(),
    );
    let mut server_b = McpCallbackServer::test_only_minimal(
        BrainSessionId::new(SessionId(session_b_id.into())),
        td.path().to_path_buf(),
    );

    let delegation_a: DelegationId = "delegation-belonging-to-a".into();
    server_a
        .test_only_inject_completed(
            delegation_a.clone(),
            DelegationResult {
                status: DelegationStatus::Success,
                summary: None,
                diff_summary: None,
                worker_branch: None,
                artifact: Some(artifact_a),
            },
            Instant::now(),
        )
        .await;

    // Server A can fetch its own delegation.
    let resp_a = server_a
        .test_only_dispatch_tool(
            "fetch_outcome_artifact",
            json!({ "delegation_id": delegation_a.as_str() }),
        )
        .await;
    let text = resp_a.success_payload().unwrap()["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(text, body);

    // Server B has no completed_delegations entry → NotFound, even
    // though the underlying git blob is reachable from this same repo.
    // The boundary is the per-server completed_delegations map, scoped
    // to brain_session_id at construction.
    let resp_b = server_b
        .test_only_dispatch_tool(
            "fetch_outcome_artifact",
            json!({ "delegation_id": delegation_a.as_str() }),
        )
        .await;
    let err = resp_b.error_payload().unwrap();
    assert!(
        err.message.contains("not found"),
        "Server B must not expose delegations from Server A"
    );
}
```

- [ ] **Step 2: Run the new test**

Run: `cargo test -p spur-mcp --test fetch_outcome_artifact_e2e fetch_outcome_artifact_completed_delegations_are_per_session`
Expected: 1 passed.

- [ ] **Step 3: Run the full spur-mcp test suite**

Run: `cargo test -p spur-mcp`
Expected: all passing.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs
git commit -m "test(spur-mcp): authorization scoping test for fetch_outcome_artifact

Two MCP servers in the same temp-repo with distinct brain_session_ids:
Server A persists+fetches successfully; Server B sees NotFound for the
same delegation_id (its completed_delegations map is empty). The
authorization boundary is per-server completed_delegations, scoped to
brain_session_id at construction (codex SF3, spec §5.3 round-9).

Phase 1 of plan-5; spec §5.3."
```

---

## Task 7: Document persist-before-publish ordering invariant in code

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:251-289` (around `build_detached_continuation`)

**What:** Per spec §5.2 round-9 P1-M2, the spec asserts that `worktrees.persist_artifact(...).await` runs synchronously before `build_detached_continuation` produces the `BrainContinuation` carrying the `delegation_id`. Add a code comment that locks this invariant for future maintainers; without it, a future refactor could move persist into a spawned task and silently break Phase 1's correctness argument.

- [ ] **Step 1: Read current build_detached_continuation context**

Run: `sed -n '251,290p' crates/spur-mcp/src/server.rs`

- [ ] **Step 2: Add the invariant comment ABOVE the function**

Edit `crates/spur-mcp/src/server.rs:251` — prepend the following doc comment immediately before `fn build_detached_continuation(`:

```rust
/// **INVARIANT (persist-before-publish):** the upstream caller MUST persist
/// any side-channel artifact (`worktrees.persist_artifact(...).await`) BEFORE
/// invoking this function. This ensures that `result.artifact.blob_sha`, which
/// gets propagated into the `BrainContinuation` and thus exposed to the brain,
/// resolves successfully under `git cat-file -p <blob_sha>` at the time the
/// brain calls `fetch_outcome_artifact`. Without this ordering, the brain
/// could observe a `delegation_id` whose backing blob has not yet been
/// written, racing the persist with the fetch.
///
/// Spec §5.2 / §7.2 (post-round-9 P1-M2). Phase 3 will preserve this
/// ordering inside `OutcomeMaterializer::materialize`.
```

- [ ] **Step 3: Verify cargo check still passes**

Run: `cargo check -p spur-mcp`
Expected: clean (comment-only change).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "docs(spur-mcp): lock persist-before-publish invariant on build_detached_continuation

Adds a code-level invariant comment to prevent future refactors from
spawning the artifact persist in a background task, which would break
Phase 1's NotFound semantics (brain observing a delegation_id whose
blob hasn't been written yet).

Phase 1 of plan-5; spec §5.2 round-9 P1-M2."
```

---

## Task 8: Update MCP tool schema (advertise the new tool to the brain)

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (the `tools/list` response, search by tool name)

**What:** The MCP `tools/list` JSON-RPC method advertises which tools the server exposes. Phase 1 must add `fetch_outcome_artifact` to that list so the brain (LLM) discovers and uses it. Without this, the dispatch arm exists but the LLM never calls the tool.

- [ ] **Step 1: Locate the tools/list response**

Run: `grep -n '"name": "delegate_to_worker"' crates/spur-mcp/src/server.rs`
Expected: a single match in the `handle_list_tools` (or similar) response payload.

Run: `grep -n "fn handle_list_tools\|tools/list\|list_tools" crates/spur-mcp/src/server.rs | head -5`

- [ ] **Step 2: Read the full tool-list handler**

Use the line number from Step 1 to read 60-100 lines around the existing tool definitions. Each tool has a JSON object like:

```rust
json!({
    "name": "check_delegation_status",
    "description": "...",
    "inputSchema": { ... }
})
```

- [ ] **Step 3: Append the new tool entry**

Add this JSON object to the array of tool definitions, immediately after `check_delegation_status` (or wherever read-only tools are grouped):

```rust
json!({
    "name": "fetch_outcome_artifact",
    "description": "Fetch the side-channel artifact (oversized stdout) associated with a completed delegation. Use this when a delegation result references an artifact (via continuation payload's artifact_ref) and you need the full content. Phase 1 supports section='full' only; Phase 3 will add status_only/summary/diff_only.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "delegation_id": {
                "type": "string",
                "description": "The delegation_id whose artifact you want to fetch. The artifact's brain session is implicit (this server's session); cross-session reads are not supported."
            },
            "section": {
                "type": "string",
                "enum": ["full"],
                "default": "full",
                "description": "Phase 1 supports 'full' only."
            }
        },
        "required": ["delegation_id"]
    }
})
```

- [ ] **Step 4: Verify cargo check + test**

Run: `cargo check -p spur-mcp`
Expected: clean.

Run: `cargo test -p spur-mcp` (full suite)
Expected: all passing.

- [ ] **Step 5: Manual verification (smoke test)**

Run a brief manual smoke test by invoking the new tool via the existing `tools/list` JSON-RPC. If there is an existing test that exercises `tools/list`, add an assertion:

Run: `grep -n 'tools/list\|"fetch_outcome_artifact"' crates/spur-mcp/tests/*.rs`

If a list-tools integration test exists (likely in `crates/spur-mcp/tests/`), append an assertion that `fetch_outcome_artifact` appears in the list. If no such test exists, write a small one in `crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs`:

```rust
#[tokio::test]
async fn fetch_outcome_artifact_appears_in_tools_list() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let server = McpCallbackServer::test_only_minimal(
        BrainSessionId::new(SessionId("sess".into())),
        td.path().to_path_buf(),
    );

    // Invoke tools/list via the JSON-RPC method dispatcher.
    let response = server
        .test_only_dispatch_method("tools/list", serde_json::Value::Null)
        .await;

    let payload = response.success_payload().expect("success");
    let tools = payload["tools"].as_array().expect("array");
    assert!(
        tools.iter().any(|t| t["name"] == "fetch_outcome_artifact"),
        "fetch_outcome_artifact must appear in tools/list, got: {tools:?}"
    );
}
```

If `test_only_dispatch_method` doesn't exist (we only added `test_only_dispatch_tool` in Task 5), add it analogously — it routes through `handle_request` instead of `handle_tool_call`. Inspect the entry point: `grep -n "fn handle_request\|fn dispatch" crates/spur-mcp/src/server.rs`.

Run: `cargo test -p spur-mcp --test fetch_outcome_artifact_e2e fetch_outcome_artifact_appears_in_tools_list`
Expected: 1 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/fetch_outcome_artifact_e2e.rs
git commit -m "feat(spur-mcp): advertise fetch_outcome_artifact in tools/list

Without the inputSchema entry the brain (LLM) cannot discover the new
tool. Adds a test asserting fetch_outcome_artifact appears in
tools/list responses.

Phase 1 of plan-5; spec §5.2."
```

---

## Task 9: Phase 1 verification — full workspace test pass

**Files:** none (verification only)

**What:** Run cargo's full test suite + clippy across the workspace to confirm Phase 1 ships green.

- [ ] **Step 1: Run cargo check across the workspace**

Run: `cargo check --workspace`
Expected: clean exit.

- [ ] **Step 2: Run cargo clippy with -D warnings**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings.

If warnings surface in code Phase 1 modified, fix them. Do NOT add `#[allow(...)]` to silence them — investigate and address per the rust-idioms skill (the compiler is your partner, not an obstacle).

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --workspace`
Expected: all passing.

- [ ] **Step 4: Confirm Phase 1 is shippable on its own**

Phase 1 ships independently — no Phase 2 or Phase 3 required for it to provide value. Verify by reviewing the diff:

Run: `git log --oneline 'origin/main..HEAD' -- crates/`
Expected: a clean sequence of small, reviewable commits (one per task).

- [ ] **Step 5: Document phase-1 completion**

Append a one-line note to the plan file (this file):

```bash
echo "" >> docs/superpowers/plans/2026-04-25-brain-continuation-phase-1-fetch-fix.md
echo "**Status:** Implementation complete on $(git log -1 --format=%cd --date=short HEAD). Final commit: $(git rev-parse --short HEAD)." >> docs/superpowers/plans/2026-04-25-brain-continuation-phase-1-fetch-fix.md
git add docs/superpowers/plans/2026-04-25-brain-continuation-phase-1-fetch-fix.md
git commit -m "docs(plan): mark brain-continuation phase 1 implementation complete"
```

---

## Verification Checklist

Use this list to confirm Phase 1 meets spec §5:

- [ ] `ArtifactRef` has `git_object_ref` and `git_blob_sha` fields with `#[serde(default, skip_serializing_if)]`.
- [ ] `#[serde(flatten)]` on `ArtifactRef::kind` is preserved (golden fixture test enforces this).
- [ ] Round-trip test covers BOTH unit variant (`Patch`) AND data-carrying variant (`Other(String)`).
- [ ] `map_worker_artifact_ref` populates all three fields (`sha256`, `git_object_ref`, `git_blob_sha`) from `WorkerArtifact`.
- [ ] `fetch_outcome_artifact` MCP tool is registered, dispatched, and returns artifact text via `git cat-file -p <blob_sha>`.
- [ ] Tool authorization is server-scoped (uses `self.brain_session_id`, not args).
- [ ] Phase 1 supports `section="full"` only; unknown sections return clean `InvalidParams`.
- [ ] Persist-before-publish ordering is documented as a code-level invariant on `build_detached_continuation`.
- [ ] `tools/list` advertises the new tool.
- [ ] Workspace tests pass; clippy clean with `-D warnings`.

---

## Out of Scope for Phase 1

The following are explicitly NOT in Phase 1 — they belong to Phase 2 or Phase 3:

- New crate `spur-blob-store` (Phase 2).
- `OutcomeStore` trait or any of its impls (Phase 2).
- `OutcomeKey` / `OutcomeRef` / `BackendTag` types (Phase 2).
- `OutcomeMaterializer` (Phase 3).
- `ContinuationPayload` schema bump to v3 (`artifact_id`, `estimated_cost_micros`, `fetch_hint`) (Phase 3).
- Section pagination beyond `"full"` (Phase 3).
- `attempt: Option<u32>` argument to the fetch tool (Phase 3).
- Truncation-ladder fallback at the materializer (Phase 3 — fallback role only).
- GC sweeper / TTL handling (Phase 2 / Phase 3).

This separation keeps Phase 1 small enough to ship independently and provide brain-visible value without coupling to the bigger architectural changes.
