   VERDICT: APPROVE-WITH-CHANGES

   MUST-FIX:

   - §7 envelope "proof" is invalid — it does not account for JSON escape expansion. Empirically, a BrainContinuation with emergency-clipped strings (128 B reason + 64 B worker_branch) but filled with \x00
control chars serializes to 1091 B for the body alone (tested via replicated ContinuationResourceBody struct), and 1475 B without any artifact_ref. With artifact_ref present (all nulls), the body is 3854 B.
Step 6 dropping artifact_ref still leaves 1091 B > MIN_MERGE_BUDGET_BYTES = 1024. The claim "step 6 is provably unreachable" is false; it is load-bearing for adversarial inputs. The spec must either (a)
make step 6 more aggressive (also clear worker_branch and replace status with a minimal variant like Success), (b) reduce emergency caps to survive 6× expansion, or (c) increase MIN_MERGE_BUDGET_BYTES. §7
   - continuation_cost_bytes is defined in spur-acp (§5.2) but the canonical serialization logic (ContinuationResourceBody, continuation_resource_block) is private to spur-core. The two measurements will
diverge unless the spec mandates moving ContinuationResourceBody and continuation_resource_block to spur-acp (or at least exposing a public stable serialization contract). §5.1, §5.2
   - PRODUCER_MAX_FIELD_BYTES is also dead at crates/spur-core/src/continuation_bridge.rs:111; the spec only mentions removal from spur-mcp. Both copies should be removed. §10.2
   - arb_delegation_status in §14.2 has a comment /* TimedOut — bounded, no strings */ but no actual strategy for TimedOut. The variant is completely absent from prop_oneof!, so the proptest cannot reach
it. This breaks the INV-D9 "exhaustive match" coverage claim. §14.2

   SHOULD-FIX:

   - ArtifactKind::Other with #[serde(flatten)] and #[serde(tag = "kind", content = "name")] serializes as "kind":"other","name":"<64B>", not "Other":"<64B>". The ~75 B sub-estimate is ~13 B low even for
plain ASCII (actual keys+value = ~86 B). The overall artifact_ref total (~385 B) is close enough to the measured ~381 B for plain ASCII, but the sub-breakdown notation is misleading. §7
   - fit_floor_debug_assert_unreachable in §14.1 claims to "confirm step 5 prevents step 6 firing" — this test will fail for control-char inputs unless the spec fixes the envelope arithmetic. The test plan
needs a step-6 release-mode fallback test that exercises adversarial (JSON-hostile) inputs and asserts the fallback successfully restores fit. §14.1
   - TimeoutFallback::Reject { reason: String } contains a String field, but INV-D9 only mentions clipping for DelegationStatus variants. The invariant text says "Every DelegationStatus and TimeoutFallback
variant containing String..." but the clip_status_strings match (implied by INV-D9) must also cover TimeoutFallback::Reject.reason. §4

   NITS:

   - §6 JSON-escape note claims "The step-5 emergency pass provides margin for MIN_MERGE_BUDGET_BYTES = 1024 against adversarial worst-case expansion." This is empirically false for control-char expansion
(6×). Emergency-capped nulls exceed 1024 without artifact_ref. §6

   CROSS-SECTION CONSISTENCY:

   - §5.2 ↔ §6: match — all constants and variants appear in both.
   - §6 ↔ §7: match in structure, but arithmetic is invalid (see MUST-FIX #1).
   - §6 ↔ §10.3: match — all TruncationEvent variants map to a field string.

   ENVELOPE ARITHMETIC CHECK:

   - ArtifactKind::Other with serde(flatten): serializes as "kind":"other","name":"<64B>". Plain ASCII keys+value = ~86 B (spec says ~75 B). With all backslashes = ~150 B. With all nulls = ~398 B.
   - Total worst case plain ASCII: ~970 B stands approximately (measured ~974 B for equivalent struct). Total with all backslashes: ~1550 B. Total with all nulls: ~3854 B. Step 6 (drop artifact_ref) still
yields ~1091 B for all-null emergency caps, exceeding MIN = 1024.
## Round 7 — gemini

**Verdict: REJECT-WITH-MUST-FIX**

**MUST-FIX:**
1. **Unbounded `DelegationStatus` on the success path violates INV-D8:** In §7.2, the persist-then-build sequence caps `summary` and `worker_branch`, but makes no mention of clipping the unbounded fields inside `DelegationStatus` (e.g., `Failed.error` which can be arbitrary stderr, `Conflict.files`) or clearing `diff_summary.files`. If a worker fails with a 10MB stderr, the "lean" continuation will inline the 10MB `DelegationStatus::Failed` variant, instantly violating the INV-D8 envelope bound and causing the orchestrator to terminally drop it.
   *Fix:* `OutcomeMaterializer` must explicitly apply the Plan-4 `clip_status_strings()` and `clip_diff_files()` logic to the lean payload on the success path, OR the wire schema must use structurally bounded types.

**SHOULD-FIX:**
1. **Tool return shape discontinuity (Phase 1 vs 3):** Phase 1's `fetch_outcome_artifact` reads legacy `WorkerArtifact` git blobs which are raw `text/plain` stdout. Phase 3 stores and retrieves full `DelegationResult` JSON. The spec should explicitly state that the Phase 3 fetch tool must use `OutcomeMetadata.content_type` to format the returned text appropriately, so the LLM UX handles both legacy and new artifacts seamlessly.

**NIT:**
1. **`as_worker_artifact` legacy kind:** In §6.5, the adapter `OutcomeRef::as_worker_artifact(legacy_kind)` requires the caller to provide the `WorkerArtifactKind`. The spec could clarify that the orchestrator caller at `crates/spur-core/src/orchestrator.rs:4755` will simply provide the existing hardcoded kind (e.g., `Stdout`) when invoking this adapter during the Phase 2 transition window.

## Round 7 — codex

**Verdict: REJECT-WITH-MUST-FIX**

**MUST-FIX:**
1. **MF1 only moved the key types; the materializer placement still creates a crate cycle.** `OutcomeKey`/`OutcomeRef` in `spur-acp` is fine (`spur-acp/Cargo.toml` has no `spur-blob-store`). But §7.2 puts `OutcomeMaterializer` in `spur-core`, while §7.3 wires it into `spur-mcp/src/server.rs::build_detached_continuation` and `spur-mcp/src/plan/mod.rs::persist_completion_result_and_notify`. Current deps are `spur-core -> spur-mcp` (`crates/spur-core/Cargo.toml:16-17`) and `spur-mcp` does not depend on `spur-core` (`crates/spur-mcp/Cargo.toml:9-26`). Adding `&OutcomeMaterializer` to a `spur-mcp` signature forces `spur-mcp -> spur-core`, creating a cycle. Move materializer to `spur-mcp`, `spur-blob-store`, or a lower shared crate, or make `spur-core` own the call path without exposing its type to `spur-mcp`.
2. **INV-D8 is still false on the materializer success path.** §7.1 keeps `status: DelegationStatus` and `diff_summary: Option<DiffSummary>` inline; current `DelegationStatus` has unbounded `String`/`Vec<PathBuf>` fields and `DiffSummary` includes `files: Vec<PathBuf>`. §7.2 only caps `summary` and `worker_branch`; §4 says status clipping is only in the fallback path. A successful store write can still emit an oversized lean continuation via `Failed.error`, `Rejected.reason`, `TimedOut.fallback.Reject.reason`, or `diff_summary.files`. The materializer must build structurally bounded inline status/diff summary, or apply the Plan-4 clipping ladder before returning the lean continuation even when persistence succeeds.
3. **The `ContinuationPayload` snippet will not compile as written.** §7.1 keeps `#[derive(... Eq ...)]` while adding `estimated_cost_usd: Option<f64>`; `f64` does not implement `Eq`. Either remove `Eq` from the payload derive or store cost as an integer/newtype that implements `Eq`.

**SHOULD-FIX:**
1. **MF2 state plumbing is adequate, but the call-site description is stale.** `brain_session_id` exists on `ReconcilerDispatchCtx` and `task.attempt` is in scope at dispatch; both can be captured into the spawned completion closure. No audit JSON schema change is needed for those values if they only form `OutcomeKey`/`artifact_uri`. However, current `reconciler.rs` also has the full `DelegationResult` from `rx.await` before calling `persist_completion_result_and_notify`; the spec's claim that callsite 2 only has beads-polling metadata is not true for the runtime path.
2. **MF3 parser regression is resolved, but implementation handoff must list construction sites.** Adding `artifact_uri: Option<String>` inside the JSON `Completion` variant preserves parser compatibility: `parse_comment` feeds only the JSON body to `serde_json::from_str`, old comments deserialize via `#[serde(default)]`, and old readers ignore extra fields. But Rust literals for `AuditSentinelKind::Completion` in `plan/mod.rs`, `server.rs`, `audit_sentinel.rs` tests, projector tests, and `spur-mcp/tests/*` must add `artifact_uri: None` or compilation fails.
3. **SF6 was not actually folded as claimed.** The prompt says session-scoped reference counting + 24h TTL + manual `spur artifact gc`; the spec says no explicit refcounting, `SPUR_OUTCOME_TTL_DAYS` default 7, and manual `spur gc outcomes`. Either update the spec to the claimed policy or correct the round-6 claim.
4. **Fetch key shape should match `OutcomeKey`.** Phase 3 `fetch_outcome_artifact` still accepts only `delegation_id` + `section`, while `OutcomeKey` includes `attempt`. Since continuation payloads now carry `artifact_id`, the fetch tool should accept `artifact_id` or an explicit `attempt` to avoid ambiguous retry artifacts.

**NIT:**
1. `fetch_hint: Option<String>` in §7.1 lacks `#[serde(default, skip_serializing_if = "Option::is_none")]`, unlike the surrounding additive optional fields.
