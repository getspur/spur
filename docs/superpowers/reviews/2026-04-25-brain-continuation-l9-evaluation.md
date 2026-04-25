# L9 Staff Engineering Evaluation — Brain Continuation Artifact Store Design

**Date:** 2026-04-25
**Evaluator:** L9 Rust Language Staff Engineer (simulated)
**Spec Under Review:** `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md`
**Grounding:** Current codebase at `0718592`+ (post-hotfix), `spur-context` shipped as DuckDB analytics crate

---

## Executive Summary

The spec is **architecturally sound** and represents a correct pivot from Plan-4's lossy truncation ladder to a bounded-handle + on-demand-fetch model. After eight review rounds, the design has converged on a coherent three-phase delivery that minimizes risk and preserves backward compatibility.

**Verdict: APPROVE with 3 MUST-FIX items for implementation, 5 SHOULD-FIX refinements, and 2 NITs.** The MUST-FIXes are all in the "implementation landmine" category — the spec text is correct but the code-change surface has subtle traps that will cause production regressions if not handled precisely.

---

## 1. First-Principles Decomposition

### 1.1 What problem are we actually solving?

The fundamental tension is:
- **Constraint A:** The merger has a bounded envelope budget (`MERGE_BUDGET_DEFAULT_BYTES`).
- **Constraint B:** Worker output is unbounded (stdout, diff text, error messages).
- **Constraint C:** The brain needs *some* signal from every worker to make forward progress.
- **Constraint D:** Brain context windows are finite and expensive.

Plan-4 (truncation ladder) solves A+C by violating B's fidelity. Plan-5 solves A+B+C+D by introducing an indirection: bound the envelope *by construction* (always small) and move unbounded content behind a fetchable reference.

**First-principles assessment:** Indirection is the correct primitive here. Every scalable system that handles variable-sized payloads against fixed-size channels uses some form of handle + store (page tables, inode tables, S3 pre-signed URLs, etc.). The spec correctly identifies this as the long-term architectural primitive.

### 1.2 Why the orchestrator level?

The user explicitly mandated "handle at orchestrator level." Let's verify this is correct rather than just compliant:

- **Producer-level (Plan-4):** Worker clips its own output before sending. Problem: worker doesn't know the envelope budget (budget is a merger property). Worker also doesn't know what else is in the batch (merger packs multiple continuations). Clipping at producer is inherently conservative and lossy.
- **Merger-level (v3.1):** Merger drops oversized items. Problem: brain never learns the worker completed. Violates Constraint C.
- **Orchestrator-level (Plan-5):** Orchestrator knows both the full worker output AND the envelope budget. It can persist full fidelity and construct a handle that fits. This is the **correct locus of control**.

**Verdict:** Orchestrator-level is the unique correct answer. The user's intuition is grounded.

---

## 2. Part-by-Part Evaluation

### 2.1 Phase 1 — `ArtifactRef` Metadata Repair (§5.1)

**The change:** Add `git_object_ref: Option<String>` and `git_blob_sha: Option<String>` to `ArtifactRef`.

**First-principles check:**
- `ArtifactRef` is a wire type. Adding `Option<String>` fields with `#[serde(default)]` is the textbook backward-compatible evolution pattern.
- The current `map_worker_artifact_ref` drops `blob_sha` and `object_ref`. This is a genuine bug: the brain gets a URI it cannot resolve.

**MCTS branch — alternatives considered:**
1. **New struct `GitArtifactRef` extending `ArtifactRef`**: Rejected — unnecessary type proliferation. `Option` fields are cleaner.
2. **Embed ref info in URI query string**: Rejected — parses are fragile, URI semantics get overloaded.
3. **Store metadata in a side table**: Rejected — introduces stateful lookup where none exists today.

**Rust-specific concern — `#[serde(flatten)]` on `ArtifactKind`:**
The current `ArtifactRef` has `#[serde(flatten)] pub kind: ArtifactKind`. Adding new non-flattened fields after a flattened field is **generally safe** in serde_json, but we should verify the enum variants don't have clashing field names. `ArtifactKind` variants: `Patch`, `TestOutput`, `Log`, `Other(String)`. None clash with `git_object_ref` or `git_blob_sha`. Safe.

**MUST-FIX (P1-M1):** The spec shows `ArtifactRef` with `kind: ArtifactKind` as a non-flattened field, but the **current code** has `#[serde(flatten)] pub kind: ArtifactKind`. The spec's code block does not show `flatten`. Implementation must preserve the `flatten` attribute or the wire format changes. This is a subtle but real compatibility hazard.

**SHOULD-FIX (P1-S1):** `git_blob_sha` is documented as "40-char hex SHA-1" but typed as `Option<String>`. Consider `Option<[u8; 20]>` with a custom hex serialize/deserialize. However, `WorkerArtifact.blob_sha` is already `String`, so consistency with existing code argues for `String`. Accept `String` but add a `debug_assert!` in the mapping function that the string is 40 hex chars.

### 2.2 Phase 1 — `fetch_outcome_artifact` MCP Tool (§5.2)

**The change:** New JSON-RPC tool that reads artifact content by delegation_id, scoped to the server's `brain_session_id`.

**First-principles check:**
- Authorization scoping via `self.brain_session_id` (not user-supplied) is correct. This prevents cross-session data exfiltration.
- Phase 1 scope is "read-only access to existing git-blob-backed WorkerArtifact payloads." This is the right scope: minimal, independently shippable, brain-visible value.

**MCTS branch — alternatives:**
1. **User-supplied `brain_session_id` argument**: Rejected in spec (codex SF3). Correct rejection — would allow session hopping.
2. **Return structured JSON instead of text**: Phase 1 returns raw text content. Phase 3 adds section pagination. This is a clean evolution path.

**MUST-FIX (P1-M2):** The tool reads from `completed_delegations` to look up the `ArtifactRef`. We need to verify that `completed_delegations` is the correct authority. In `server.rs`, `build_detached_continuation` produces the continuation but the `DelegationResult` (including `artifact`) may or may not be durably stored in `completed_delegations` at the time of fetch. If the fetch races the completion, we need well-defined behavior. The spec should explicitly state: "If the delegation has not yet completed or the artifact is not found, return `StoreError::NotFound`."

**SHOULD-FIX (P1-S2):** The Phase 1 tool signature has `section: Option<"full">` with default "full". The spec says "Phase 3 widens the supported sections." This is forward-compatible because serde will ignore unknown variants if the enum is externally tagged... but actually, if the client sends `"diff_only"` to a Phase 1 server, and the server parses into a `Section` enum that doesn't have `DiffOnly`, it will fail. The spec should clarify: Phase 1 servers reject unknown section strings with a clean error, not a deserialization panic.

### 2.3 Phase 2 — `spur-blob-store` Crate Architecture (§6)

**The change:** New crate with `OutcomeStore` async trait, FS impl, memory impl, measured decorator. Git-blob impl stays in `spur-worktree`.

**First-principles check:**
- Trait abstraction with ≥2 backends (FS, GitBlob) and ≥2 consumers (orchestrator, fetch tool) justifies the crate boundary. Correct.
- Type ownership split (§6.3) is the most architecturally subtle part of the entire spec. Let's evaluate it carefully.

**The cycle avoidance problem (MF1):**
- `ContinuationPayload` (in `spur-acp`) needs `artifact_id: Option<OutcomeKey>`.
- `OutcomeStore` trait (in `spur-blob-store`) needs `OutcomeKey` in its method signatures.
- If `OutcomeKey` lives in `spur-blob-store`, then `spur-acp → spur-blob-store`.
- But `spur-blob-store` already needs `spur-acp` for `BrainSessionId`, `DelegationId`.
- **Cycle:** `spur-acp ↔ spur-blob-store`.

**Resolution:** Move `OutcomeKey`, `OutcomeRef`, `BackendTag` to `spur-acp/src/domain/outcome.rs`. This breaks the cycle: `spur-blob-store` depends on `spur-acp` (one-way). `spur-acp` knows about keys but not about the store trait.

**MCTS branch — alternatives for cycle breaking:**
1. **Move identifiers to a separate `spur-ident` crate**: Overkill for three types. Adds crate overhead.
2. **Use generic type parameters on the trait**: `OutcomeStore<Key, Ref>`. This pushes complexity to all call sites. Rejected — the spec's approach is cleaner.
3. **Duplicate the key shape in both crates**: Maintenance nightmare. Rejected.
4. **Use type-erased `String` keys in the trait**: Loses type safety. Rejected.

**Verdict:** The type ownership split is correct. `spur-acp` as the leaf-domain crate owning identifiers and wire shapes is the right dependency graph.

**SHOULD-FIX (P2-S1):** `BackendTag` is `Copy`. If we add S3/GCS later, those variants may need associated config (region, bucket). A `Copy` enum can't carry `String` data. The spec should either:
- Make `BackendTag` non-`Copy` now (breaking later if we make it `Copy`), or
- Accept that future cloud variants will be `BackendTag::Cloud(String)` and thus non-`Copy`, which is a backward-compatible change (removing `Copy` is a breaking change; adding non-`Copy` variants to a `Copy` enum is a breaking change).

**Recommendation:** Remove `Copy` from `BackendTag` now. It's not needed (we pass by reference or clone), and it prevents future pain.

**MUST-FIX (P2-M1):** `FsOutcomeStore` path layout: `<root>/<brain_session_id>/<delegation_id>/<attempt>.json`. If `brain_session_id` or `delegation_id` contain path separators or other filesystem-unfriendly characters, this creates a directory traversal vulnerability or simply fails. `DelegationId` and `BrainSessionId` are likely UUIDs, but we should verify and document. If they're arbitrary strings, we need sanitization or a content-addressed flat layout.

**SHOULD-FIX (P2-S2):** `FsOutcomeStore::put` writes atomically via tempfile + rename. On macOS/APFS, `rename` is atomic. On Linux with ext4, `rename` over an existing file is atomic. But what about creating the parent directories? `create_dir_all` is not atomic with the rename. A crash between `create_dir_all` and `rename` could leave an empty directory tree. Acceptable for this use case, but document it.

**SHOULD-FIX (P2-S3):** `sweep_older_than` walks namespaces and checks "newest mtime." On `FsOutcomeStore`, `std::fs::metadata` mtime resolution is filesystem-dependent. On macOS APFS, it's nanosecond. On some network filesystems, it's 1-second or worse. TTL sweeps with very short TTLs (sub-second) could be unreliable. Document: `SPUR_OUTCOME_TTL_DAYS` minimum is 1 day; sub-day TTLs are not supported on all backends.

### 2.4 Phase 3 — Lean Schema v3 + `OutcomeMaterializer` (§7)

**The change:** `ContinuationPayload` gains `artifact_id: Option<OutcomeKey>`, `estimated_cost_micros: Option<u64>`, `fetch_hint: Option<String>`. `OutcomeMaterializer` in `spur-mcp` is the single producer of `BrainContinuation` on the success path.

**First-principles check — the `artifact_ref` vs `artifact_id` coexistence (MF5):**
The spec correctly identifies that `artifact_ref` (legacy, narrow scope = oversized stdout) and `artifact_id` (new, broad scope = full delegation outcome) must coexist during transition. This is the right call — abrupt removal of `artifact_ref` would break existing brain prompts that reference it.

**However,** there's a subtlety in the `ContinuationResourceBody` renderer (`continuation_bridge.rs`). The current renderer maps `ContinuationPayload` fields to the resource body at schema_version 2. When we bump to 3, we need to decide: does the resource body include `artifact_id`? If old brains expect the resource body shape to match schema_version 2, adding new fields is fine (they'll ignore them). But if the TUI or other consumers parse the resource body strictly, we need care.

**Verdict:** `artifact_id` in `ContinuationPayload` with `#[serde(default, skip_serializing_if = "Option::is_none")]` is safe. Old serde consumers ignore unknown fields by default.

**The `estimated_cost_micros: Option<u64>` change (round 8 / MF3):**
Excellent catch in round 7. `f64` does not implement `Eq`, which would break `#[derive(Eq)]` on `ContinuationPayload`. The `DelegationKey` type likely depends on `ContinuationPayload: Eq + Hash`. Using `u64` micro-USD preserves `Eq` and is semantically correct (LLM pricing is rational, not irrational).

**SHOULD-FIX (P3-S1):** The spec says `estimated_cost_micros` is "cost in micro-USD (1e-6 USD)." We should document the conversion formula precisely and add a helper function:
```rust
impl ContinuationPayload {
    pub fn estimated_cost_usd(&self) -> Option<f64> {
        self.estimated_cost_micros.map(|m| m as f64 / 1_000_000.0)
    }
}
```
This prevents every consumer from doing its own (potentially incorrect) conversion.

**The `OutcomeMaterializer` location (round 8 / MF1):**
Round 6 placed it in `spur-core`, which would create `spur-mcp → spur-core` cycle when `server.rs` and `plan/mod.rs` call it. Round 8 relocates to `spur-mcp`.

**MCTS branch — alternative cycle resolutions:**
1. **Keep materializer in `spur-core`, introduce a callback trait in `spur-mcp`**: `spur-mcp` defines `MaterializerClient` trait, `spur-core` implements. This inverts control but adds indirection.
2. **Move materializer to a new `spur-materializer` crate**: Yet another crate. Overkill.
3. **Put materializer in `spur-mcp`**: Natural home, both callsites are there, no cycle.

**Verdict:** Round 8's relocation to `spur-mcp` is correct. The dependency graph in §15.2 is now acyclic.

**MUST-FIX (P3-M1):** The spec says "the materializer is the **single producer** of `BrainContinuation` on the success path." This is a critical invariant (INV-D8). We need to verify that NO OTHER code path constructs `BrainContinuation` and pushes it to the merger. Let's trace:
- `build_detached_continuation` in `server.rs`: Will be updated to call `OutcomeMaterializer::materialize`.
- `persist_completion_result_and_notify` in `plan/mod.rs`: Will be updated to call `OutcomeMaterializer::materialize`.
- Any test code that constructs `BrainContinuation` directly and feeds it to the merger.

The spec mentions test callsites in §7.1 (SF6) but there's a broader concern: grep for `BrainContinuation {` across all crates. Any direct construction that bypasses the materializer is a violation of INV-D8. The spec should mandate a pre-implementation audit: `grep -rn 'BrainContinuation {' crates/` and either convert all sites to use the materializer or document why they don't need to (e.g., unit tests that don't touch the merger).

**The clipping strategy (round 8 / MF2):**
Round 7 correctly identified that the success path was NOT clipping unbounded fields. Round 8 fixes this: clipping is mandatory on every materializer entrypoint, success or fallback.

**First-principles evaluation:** The spec frames INV-D8 as "enforced-by-clip" rather than "by construction." This is honest and correct. The alternative (introducing a `LeanStatus` enum) was considered and rejected for good reasons (variant duplication, brain UX regression).

**However,** "enforced-by-clip" is procedurally fragile. It depends on:
1. Every developer remembering to call `clip_status_strings` when adding a new materializer entrypoint.
2. The exhaustive-match proptest (INV-D9) catching new variants.
3. The `debug_assert!` firing in test/debug builds.

**MCTS branch — could we make it "by construction" without `LeanStatus`?**
One approach: make `DelegationStatus`'s string fields use a `BoundedString<const N: usize>` newtype that enforces the cap at construction. Then `clip_status_strings` returns `DelegationStatus<BoundedString<512>>` instead of `DelegationStatus<String>`. This is a significant refactor but would make INV-D8 true by construction.

**Verdict:** For this spec, "enforced-by-clip" is acceptable because:
- The materializer is the single producer.
- CI proptest (INV-D9) catches new variants.
- The cost of `BoundedString` refactor is high and would touch every `DelegationStatus` construction site.

**But** add a `// INVARIANT: INV-D8` comment at the materializer boundary and in the clip helper module. Social enforcement complements mechanical enforcement.

**SHOULD-FIX (P3-S2):** The `clip_status_strings` and `clip_diff_files` helpers are moved to `spur-acp::domain::clip`. This module should be `pub(crate)` or `pub` with a doc comment: "Internal clipping helpers for continuation materialization. Do not use directly outside of `spur-mcp::OutcomeMaterializer` and `spur-core::continuation_bridge`." Prevent accidental proliferation.

### 2.5 Truncation-Ladder Fallback (§7.7)

**The change:** Plan-4's truncation ladder survives as the `OutcomeStore::put` failure fallback.

**First-principles check:** This is correct engineering. The artifact store is a new system; it will have failures (disk full, permissions, git corruption). The fallback ensures INV-α holds even when the store is down.

**Critical concern:** The fallback path is rarely exercised in production. Bit-rot is real. The spec's `MockFailingOutcomeStore` for CI is the right mitigation.

**SHOULD-FIX (P3-S3):** The `MockFailingOutcomeStore` should be parameterized by failure mode:
- `put` fails with `Io`
- `put` fails with `TooLarge`
- `put` fails with `Backend`
- `put` panics (to test the materializer's panic catching)

Each mode should trigger the fallback and produce a valid continuation. This is more thorough than a single "always fails" mock.

### 2.6 GC and Lifecycle (§8)

**The change:** Session-scoped namespace deletion + TTL sweep on startup + manual CLI.

**First-principles check:**
- Session-scoped primary cleanup is correct. When a brain session ends, its artifacts are no longer needed by that brain.
- TTL backup for crash recovery is correct. No reference counting needed (accepted in §9.1).
- Manual operator escape is correct operational practice.

**MCTS branch — alternatives for cleanup:**
1. **Reference counting**: Rejected in spec. Would require tracking every `fetch_outcome_artifact` call and decrementing on brain context eviction. Complex, error-prone, not justified for session-scoped data.
2. **Immediate deletion on brain read**: Rejected — brain may re-fetch.
3. **Archive to cold storage before delete**: Deferred/non-goal. Correct.

**MUST-FIX (GC-M1):** §8.4 says "GitBlobOutcomeStore::sweep_older_than checks refs/spur/artifacts/<session> mtimes." Git refs don't have mtimes in the traditional sense. `git for-each-ref --format='%(authordate)'` gives the commit date, not the ref touch time. The spec needs to clarify how "mtime" is determined for git refs. Options:
- Use the commit timestamp of the blob (which is the commit that created it, not the ref update time).
- Use filesystem mtime of `.git/refs/spur/artifacts/<session>` (only works for loose refs, not packed refs).
- Use a separate metadata file or reflog.

**Recommendation:** For git-blob backend, track artifact creation time in `OutcomeMetadata` (which is stored alongside the artifact or in a sidecar), not filesystem mtime. The `FsOutcomeStore` can use filesystem mtime. Document this backend difference.

**SHOULD-FIX (GC-S1):** The startup TTL sweep blocks orchestrator startup. If there are many old namespaces (e.g., after a long downtime), the sweep could take seconds. Consider spawning the sweep in a background task so the orchestrator can accept work immediately. The spec mentions "on orchestrator startup" but doesn't specify blocking vs. background.

### 2.7 Failure Modes (§9)

**§9.1 Atomicity gap (persist+enqueue):**
The spec correctly identifies and accepts this gap. Option (b) "accept the loss" is consistent with v3.1's in-memory scheduler state. This is the right call — adding a transaction log for this would be massive over-engineering.

**§9.2 Persist failure → truncation fallback:**
Correct. The fallback ensures the brain still gets a signal.

**§9.3 Authorization mismatch on fetch:**
Correct. Cross-session rejection is a security boundary.

**§9.4 LLM cache invalidation cost:**
Excellent that this is documented. The trade-off is real: lean continuations save context tokens on the "skim" path but may cost cache hits on the "fetch then act" path. No action needed, but this should be in operator docs.

**§9.5 Bounded artifact size:**
Correct. 512 KiB is honest. The spec should be explicit: "Artifact store is not a general-purpose blob store. For outputs >512 KiB, truncate with sentinel."

**§9.6 Brain-session crash auth wall:**
Deliberate and correct trade-off. Security > crash recovery for session-scoped data. The operator escape (`spur outcomes copy`) is deferred appropriately.

**§9.7 Concurrent fetch-during-GC race:**
The spec's analysis of POSIX semantics and git blob survival is correct. The behavior is benign.

**NIT (FM-N1):** §9.7 says "clients should retry on transient error." The MCP tool is called by the brain (LLM), not a human client. The brain doesn't have retry logic unless we build it into the tool handler. The spec should clarify: "The `fetch_outcome_artifact` tool handler returns `NotFound` immediately; the brain decides whether to retry. If the race is likely (session terminating), the brain may see `NotFound` and should treat it as a signal that the session is ending."

### 2.8 Observability (§10)

**Structured tracing events:**
The table is comprehensive. Target namespacing (`spur.metrics.*`) is consistent.

**SHOULD-FIX (OBS-S1):** Add `spur.metrics.outcome_fetch_not_found` at WARN level. When the brain calls `fetch_outcome_artifact` for a non-existent key, this is a sign of either:
- A bug in the materializer (key mismatch)
- A race with GC
- A brain hallucinating a delegation_id

This event is distinct from `outcome_fetch_unauthorized` (security) and should be tracked separately.

**TUI surfacing (§10.2):**
The "PRODUCER-BUG" prefix for `OversizedSingleItem` post-Plan-5 is a good operational signal. If this fires after Plan-5 ships, it means INV-D8 has been violated.

### 2.9 spur-context Integration (§13)

**The cross-check:** The spec correctly notes that `spur-context` as-shipped is DuckDB analytics, NOT the 2026-04-13 ContextEngine spec. The integration hook is forward-looking.

**The `attempt` column coordination:**
The spec's observation that `observations` may need an `attempt` column is correct. Option (a) (add column) vs Option (b) (embed in JSON). The spec defers this decision, which is appropriate.

**SHOULD-FIX (CTX-S1):** Add a tracking issue or TODO comment in the spec: "When ContextEngine Phase 1 implements `observations`, verify `attempt` column exists or is embedded in `artifacts_json`."

---

## 3. Rust-Specific Engineering Concerns

### 3.1 Async Trait Overhead

The spec uses `#[async_trait::async_trait]` for `OutcomeStore`. In Rust 2021 with `async-trait`, this allocates a `Box<dyn Future>` per call. For a store that may see hundreds of put/get ops per session, this is acceptable but not free.

**MCTS branch — alternatives:**
1. ** RPITIT (Return Position Impl Trait in Trait)**: Rust 1.75+ supports `async fn` in traits natively. The workspace `rust-version` should be checked. If it's ≥1.75, `async-trait` can be avoided.
2. **Keep `async-trait`**: Stable, well-known, minor overhead.

**Verdict:** Check `rust-version` in workspace `Cargo.toml`. If ≥1.75, prefer native async trait. If <1.75, `async-trait` is fine.

### 3.2 `Eq` Derive Chain

The round-8 change from `f64` to `u64` for `estimated_cost_micros` restores `Eq`. This is correct. We should verify the full derive chain:
- `ContinuationPayload` derives `Eq` → requires all fields to be `Eq`.
- `DelegationStatus` must derive `Eq` → requires all variants' fields to be `Eq`.
- `DiffSummary` must derive `Eq`.
- `ArtifactRef` must derive `Eq`.
- `OutcomeKey` must derive `Eq`.

If any of these currently lack `Eq`, adding it is a breaking change for consumers that manually implemented `PartialEq` but not `Eq`. Check before implementing.

### 3.3 Serialization Backward Compatibility

All new fields are `Option<T>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. This is the correct pattern. Old payloads deserialize with `None`. New payloads are minimal when fields are absent.

**One subtlety:** `ContinuationPayload` is part of `BrainContinuation` which is part of `DelegationKey` (used in hash maps). The hash must be stable across schema versions. Since `Option::None` serializes to absent field, and absent field deserializes to `None`, the hash is stable. Good.

### 3.4 Proptest Strategy

INV-D9 requires an exhaustive-match proptest. The spec says "proptest with `arb_delegation_status` (every variant with adversarial-large strings) → call `OutcomeMaterializer::materialize` → assert `continuation_cost_bytes(cont) ≤ MERGE_BUDGET_DEFAULT_BYTES` for 1024 cases."

**SHOULD-FIX (TST-S1):** 1024 cases is a good default for CI, but the proptest should also run with `PROPTEST_CASES=100_000` in a nightly/weekly job. The budget bound is critical; we want high confidence.

**SHOULD-FIX (TST-S2):** The proptest should also generate adversarial `DiffSummary` with many files and long paths, and adversarial `summary`/`worker_branch` strings. The spec mentions `arb_delegation_status` but `ContinuationPayload` has multiple unbounded fields.

### 3.5 Error Handling

`StoreError` uses `thiserror`. Good. The variants are well-chosen.

**NIT (ERR-N1):** `StoreError::Unauthorized` has no associated context field. Consider `Unauthorized { requested: OutcomeKey, actual_session: BrainSessionId }` for better error messages and observability.

---

## 4. Dependency Graph Verification (§15.2)

Let's trace the post-round-8 graph:

```
spur-core ──► spur-mcp ──► spur-worktree
                │              │
                └──────► spur-blob-store
                            │
                            ▼
                        spur-acp
                            │
                            ▼
                    agent-client-protocol
```

**Check for cycles:**
- `spur-core → spur-mcp` (existing)
- `spur-mcp → spur-blob-store` (new)
- `spur-mcp → spur-worktree` (existing)
- `spur-worktree → spur-blob-store` (new, for GitBlobOutcomeStore)
- `spur-blob-store → spur-acp` (new)
- `spur-acp → agent-client-protocol` (existing)

No cycles. The graph is a DAG. `spur-acp` is correctly the leaf-domain crate.

**One concern:** `spur-worktree → spur-blob-store` means `spur-worktree` gains a new dependency. Let's verify `spur-worktree`'s current dependencies are lightweight. If `spur-worktree` is meant to be git-only, adding `spur-blob-store` (which brings in `tokio`, `serde`, etc.) is fine since `spur-worktree` already depends on `spur-acp` which depends on `serde`.

**Verdict:** Dependency graph is sound.

---

## 5. Implementation Risk Assessment

| Phase | Risk Level | Primary Risk | Mitigation |
|-------|-----------|--------------|------------|
| 1 | Low | `ArtifactRef` serde flatten attribute mishandling | Add round-trip test old→new→old wire format |
| 2 | Medium | `spur-blob-store` crate bootstrap + trait stabilization | Keep trait minimal; don't over-abstract |
| 3 | High | `OutcomeMaterializer` integration into two callsites; `BrainContinuation` construction site audit | Pre-implementation grep audit; feature-flag rollout |
| GC | Low | Startup sweep blocking | Spawn in background task |
| Tests | Medium | Proptest runtime in CI | Gate at 1024 cases, nightly at 100k |

**Overall risk:** Medium. The spec is well-converged after eight rounds. The highest risk is Phase 3's integration surface — touching `server.rs`, `plan/mod.rs`, `plan/reconciler.rs`, `continuation_bridge.rs`, and multiple test files simultaneously.

**Recommendation:** Implement behind a feature flag `artifact-store-v3` or an env var `SPUR_ARTIFACT_STORE=1` so Phase 3 can be rolled out gradually. The materializer can branch: if disabled, build continuations the old way (full payload, subject to merger drop). If enabled, use the new path. This allows canarying.

---

## 6. Summary of Findings

### MUST-FIX (Implementation blockers)

| ID | Section | Issue | Fix |
|----|---------|-------|-----|
| P1-M1 | §5.1 | Spec's `ArtifactRef` code block omits `#[serde(flatten)]` on `kind` that exists in current code | Preserve `flatten` in implementation; add old→new wire round-trip test |
| P1-M2 | §5.2 | `fetch_outcome_artifact` lookup authority (`completed_delegations`) may race completion | Document: return `NotFound` if artifact not yet available |
| P3-M1 | §7.2 | "Single producer" claim needs verification — grep for all `BrainContinuation {` construction sites | Pre-implementation audit; convert all production sites to materializer; document test exemptions |
| GC-M1 | §8.4 | Git ref "mtime" is not well-defined; filesystem mtime doesn't work for packed refs | Use `OutcomeMetadata.created_at` for git-blob TTL; document backend difference |

### SHOULD-FIX (Quality / future-proofing)

| ID | Section | Issue | Fix |
|----|---------|-------|-----|
| P1-S1 | §5.1 | `git_blob_sha` typed as `String` without validation | Add `debug_assert!(is_40_hex(&sha))` in mapping function |
| P1-S2 | §5.2 | Phase 1 server behavior on unknown `section` string | Return clean error, not deserialization panic |
| P2-S1 | §6.3 | `BackendTag` derives `Copy` — prevents future cloud variants with config | Remove `Copy` now |
| P2-S2 | §6.4 | `FsOutcomeStore` path layout uses raw IDs without sanitization | Verify IDs are UUIDs; if not, sanitize or hash |
| P2-S3 | §6.4 | `sweep_older_than` relies on filesystem mtime resolution | Document minimum TTL = 1 day |
| P3-S1 | §7.1 | No helper for `micros → USD` conversion | Add `estimated_cost_usd()` helper on `ContinuationPayload` |
| P3-S2 | §7.2 | `clip` module accessibility | Mark `pub` with doc comment restricting usage |
| P3-S3 | §7.7 | `MockFailingOutcomeStore` only tests one failure mode | Parameterize by failure mode (Io, TooLarge, Backend, Panic) |
| GC-S1 | §8.2 | Startup sweep may block orchestrator startup | Spawn sweep in background task |
| OBS-S1 | §10.1 | Missing `outcome_fetch_not_found` event | Add WARN-level event for missing keys |
| CTX-S1 | §13 | No tracking for ContextEngine `attempt` column decision | Add TODO/issue reference |
| TST-S1 | §4 | Proptest case count may be insufficient for critical invariant | Run 100k cases in nightly CI |
| TST-S2 | §4 | Proptest strategy may not cover all unbounded fields | Ensure `arb_diff_summary`, `arb_summary`, `arb_worker_branch` are adversarial |

### NITs

| ID | Section | Issue |
|----|---------|-------|
| ERR-N1 | §6.3 | `StoreError::Unauthorized` lacks context fields |
| FM-N1 | §9.7 | "Clients should retry" — brain is not a retrying client; clarify semantics |

---

## 7. Final Verdict

**The design is APPROVED for implementation.**

After exhaustive first-principles analysis and MCTS-style branching across alternatives, the spec converged on the correct architecture:
- Bounded handles by construction (the unique solution to the envelope problem)
- Orchestrator-level control (the correct locus)
- Three-phase delivery (correct risk sequencing)
- Backward-compatible schema evolution (correct deployment strategy)
- Trait abstraction with cycle-free dependency graph (correct Rust engineering)

The MUST-FIXes are all implementation-landmine category — the spec text is correct but the code changes need defensive execution. The SHOULD-FIXes improve robustness and future-proofing.

**Recommended next step:** Proceed to plan-writing (`superpowers:writing-plans`) with Phase 1 as the first beads epic. Include the pre-implementation `grep` audit (P3-M1) as the first task in the Phase 3 epic.

---

## 8. Grounding Pass (round 9 — sequential-thinking MCTS over §1–§7 above)

**Author:** L9 Rust staff engineer (simulated, second sweep)
**Grounded against:** Plan-5 spec at commit `0049729` + uncommitted round-8 amendments (`OutcomeMaterializer` in `spur-mcp`, `estimated_cost_micros: u64`, INV-D8-via-clip, single materializer entrypoint, `attempt`-aware fetch tool).

This pass cross-checks the §1–§7 findings against the **actual** round-8 spec state and the **actual** codebase, then MCTS-branches the deepest concerns with first-principles arithmetic. It also surfaces four findings the original L9 review did not catch.

### 8.1 Items already resolved by round-8 spec

| Original finding | Round-8 status | Evidence |
|---|---|---|
| `estimated_cost_usd: f64` breaks `Eq` (called out in §3.2 above) | RESOLVED | §7.1 uses `estimated_cost_micros: Option<u64>` |
| `OutcomeMaterializer` placement → cycle (§2.4) | RESOLVED | §7.2 + §15.2 place it in `spur-mcp`, not `spur-core` |
| INV-D8 success-path unbounded inline status (round 7 finding § implicit) | RESOLVED | §4 + §7.2 enforce-by-clip on every materializer entrypoint |
| Dual reconciler entrypoints needed (§2.4 P3-M1 implicit) | OBSOLETE | §7.3 collapses to single `materialize` entrypoint; reconciler has full `DelegationResult` from `rx.await` (verified at `spur-mcp/src/plan/reconciler.rs:409`) |

### 8.2 §1–§7 items confirmed as real round-9 gaps

| ID | Severity | Round-9 action |
|---|---|---|
| **P1-M1** (serde flatten drift) | **MUST-FIX** | §5.1 code block must show `#[serde(flatten)] pub kind: ArtifactKind` matching `crates/spur-acp/src/domain/continuation.rs:46`. Without it, naive impl breaks the wire envelope shape (verified by reading the file). |
| **P1-M2** (fetch race) | **MUST-FIX** (semantic) | §5.2 + §7.2 must state the **persist-before-publish ordering invariant**: brain only learns `delegation_id` *after* the `BrainContinuation` is enqueued, which happens *after* `store.put` returns. Therefore any brain-initiated fetch for a *known* id cannot race the persist. NotFound is reserved for hallucinated ids and post-GC reads. |
| **GC-M1** (git ref mtime) | **MUST-FIX** | §8.4 must specify per-backend semantics: `FsOutcomeStore` uses filesystem mtime; `GitBlobOutcomeStore` stores `OutcomeMetadata` (incl. `created_at`) in a sidecar git blob (e.g., `refs/spur/artifacts/<session>/.meta`) and reads `created_at` from there. Filesystem mtime of `.git/refs/...` is unreliable for packed refs. |
| P2-S1 (`BackendTag: Copy`) | SHOULD-FIX | §6.3 should drop `Copy` derive. Future cloud variants (`Cloud { region: String }`) will need it removed; doing it now avoids a future breaking change. |
| P2-S2 (FS path sanitization) | SHOULD-FIX | §6.4 should assert that `BrainSessionId` and `DelegationId` are UUID-shaped (verify via `uuid::Uuid::parse_str` in `put`); reject otherwise to prevent path traversal. |
| P3-S1 (`micros → USD` helper) | SHOULD-FIX | §7.1 should add `impl ContinuationPayload { pub fn estimated_cost_usd(&self) -> Option<f64> }` to centralize the conversion. |
| P3-S3 (MockFailingOutcomeStore parameterization) | SHOULD-FIX | §7.7 enumerate `Failure::{Io, TooLarge, Backend, Panic}` so each fallback path is exercised. |
| GC-S1 (startup sweep blocking) | SHOULD-FIX | §8.2 should specify `tokio::spawn` for the sweep; orchestrator startup must not block on it. |
| OBS-S1 (`outcome_fetch_not_found` event) | SHOULD-FIX | §10.1 add WARN-level event distinguishing "race/hallucination/GC'd" from `unauthorized`. |

### 8.3 NEW findings surfaced by first-principles pass (not in §1–§7)

These were not covered by the original L9 review.

#### N1. INV-D8 proof gap: `ArtifactRef.uri` and `ArtifactRef.git_object_ref` are unbounded

**MUST-FIX (proof completeness).** §4's INV-D8 envelope arithmetic depends on every inline string being bounded. Walking the lean payload:

| Field | Bound | Source |
|---|---|---|
| `status` (after `clip_status_strings`) | ≤ N × 512 B per variant | INV-D9 exhaustive match |
| `summary` | ≤ 512 B | materializer cap |
| `diff_summary` (after `clip_diff_files`) | ≤ 16 × 128 B | materializer cap |
| `worker_branch` | ≤ 256 B | materializer cap |
| `fetch_hint` | ≤ 256 B | materializer cap |
| `estimated_cost_micros` | 8 B | u64 |
| `artifact_id` (`OutcomeKey`) | ~80 B | UUID + UUID + u32 |
| **`artifact_ref.uri`** | **UNBOUNDED `String`** | type system |
| **`artifact_ref.git_object_ref`** | **UNBOUNDED `Option<String>`** | type system |

The current single construction site (`map_worker_artifact_ref`) produces structurally bounded URIs (`spur://artifact/{uuid}` ≈ 40 B) and refs (`refs/spur/artifacts/{uuid}` ≈ 50 B). But INV-D8 must hold *as a property of `ContinuationPayload`*, not as a property of the constructor. A future refactor that constructs `ArtifactRef` with non-bounded URIs (e.g., S3 presigned URLs with query parameters) would silently violate INV-D8 without any compile-time signal.

**Fix:** add `clip_artifact_ref_strings(artifact_ref, 256)` to the materializer's clip pass. Cap `uri` at 256 B and `git_object_ref` at 256 B. This is cheap, defensive, and closes the proof.

**Detection in CI:** the INV-D8 proptest (§4 round 8) should generate adversarial `ArtifactRef.uri` strings as part of its strategy. Currently the spec says "arb_delegation_status with adversarial-large strings" — needs to also cover `arb_artifact_ref`.

#### N2. Concurrent `put` semantics for same key + different content are undefined

**SHOULD-FIX.** §6.3 says `OutcomeStore::put` is "Idempotent: same key + same content → same OutcomeRef, no rewrite." What about same key + **different** content? Practical scenario: a worker re-runs on the same `(brain_session, delegation_id, attempt)` due to a retry path bug, with different stdout.

Three semantics options:
- (a) Last-write-wins (silent overwrite). Risky — silently loses earlier output.
- (b) First-write-wins (subsequent puts return existing `OutcomeRef`, ignore new content). Safe but masks logic bugs.
- (c) Error on content mismatch (`StoreError::ContentMismatch { existing_sha, new_sha }`). Loud, forces caller to handle.

**Recommendation:** (c). attempts are supposed to be unique per worker run; if the same key sees two distinct contents, that's an upstream invariant violation worth surfacing.

#### N3. Release-build `debug_assert!` cannot enforce INV-α in production

**SHOULD-FIX.** §7.2 step 6 uses `debug_assert!(continuation_cost_bytes(&cont) <= MERGE_BUDGET_DEFAULT_BYTES)`. In release builds this is a no-op. If a future variant slips past the clip helpers (or the materializer is bypassed in some path), production silently produces an oversized continuation and the merger drops it — the exact bug Plan-5 was designed to eliminate.

**Fix:** in addition to `debug_assert!`, emit a release-build `tracing::error!(target: "spur.metrics.materializer_oversized_post_clip", ...)` if the post-clip envelope exceeds budget, AND fall through to the truncation-ladder fallback as a recovery. Belt + suspenders. Operators see the metric; brain still gets a continuation.

#### N4. Schema-version forward-compat: new brains must accept old schemas

**NIT.** §7.1 bumps `schema_version: 2 → 3`. Spec says "old brains see new fields as unknown-but-ignored" (forward-compatible from old → new). The reverse direction is not stated: do new brains accept `schema_version: 2` payloads (which lack `artifact_id`, `estimated_cost_micros`, `fetch_hint`)? With `#[serde(default)]` on all new fields, yes. Worth one sentence in §11 (Migration / rollback) to make explicit: "New brains accept all `schema_version ∈ {2, 3}`; missing optional fields default to `None`."

### 8.4 MCTS branch summary on contested decisions

| Decision | Alternatives MCTS-explored | Winner | Reason |
|---|---|---|---|
| Materializer crate location | `spur-core` (round 6) / `spur-mcp` (round 8) / new `spur-materializer` crate / callback-trait inversion | `spur-mcp` | Both callsites and `McpEventSink` already there; no new crate; no cycle |
| INV-D8 enforcement model | "by construction" via `LeanStatus` enum / "by clip" at materializer / `BoundedString<N>` newtype | "by clip" | LeanStatus duplicates variants; BoundedString is a workspace-wide refactor; clip + INV-D9 proptest is sufficient |
| Cost type | `f64` (lossy) / `u64` micros / `Decimal`-style fixed-point | `u64` micros | Eq-derivable; LLM pricing is rational; no float comparison hazards |
| Fetch race semantics | block-until-ready / pending status / NotFound | NotFound + ordering invariant | Persist-before-publish makes known-id race impossible; NotFound is correct for hallucinated/GC'd ids |
| GC mtime source | filesystem mtime / git reflog / sidecar `OutcomeMetadata` / git-gc-only | sidecar metadata + per-backend split | Reliable across loose/packed refs; aligns with type-erased `OutcomeStore` trait |
| `BackendTag` derive | `Copy` / non-`Copy` | non-`Copy` | Future cloud variants need `String` config; one-time annoyance vs future breaking change |

### 8.5 Recommendation

The round-8 spec is **architecturally complete**. The findings above are **implementation hardening** — necessary for production correctness but mechanical to fold.

Three options for the user:

1. **Round-9 amendments** (~30 min): fold the 3 confirmed MUST-FIX (P1-M1, P1-M2, GC-M1) and N1 (ArtifactRef-uri unboundedness) into the spec text. SHOULD-FIX items become beads tasks during plan-writing.
2. **Defer to writing-plans**: spec stays at round 8; plan-writer ingests this grounding report alongside the spec, allocates each finding to a Phase-1/2/3 task. Lower spec churn.
3. **Hybrid**: fold only the 4 MUST-FIX items into the spec (they affect correctness arguments); allocate the rest to plan tasks.

**L9 recommendation: option 3.** The MUST-FIX items affect spec text correctness (a future reader cannot reconstruct INV-D8's proof without N1; a future implementer will silently break wire format without P1-M1). The SHOULD-FIX items are perfectly fine as plan tasks since each is a discrete code change with an obvious test.

After option-3 fold, the spec is ready for `superpowers:writing-plans`.
