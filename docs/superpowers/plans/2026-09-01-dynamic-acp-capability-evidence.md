# Dynamic ACP Capability Evidence Implementation Plan

**Approved spec:** `docs/superpowers/specs/2026-09-01-dynamic-acp-capability-evidence-design.ipynb`  
**Design epic:** `bd-j5kb` (closed)  
**Implementation source epic:** `bd-fh8n`

**Execution epic:** `bd-2tck`

**Execution plan:** `8a0b5d1c-97e8-45c2-bb38-05f3876fd663`
**DAG label:** `spur:impl-dynamic-acp-v1`  
**Worker routing:** user-selected `codex`, model `gpt-5.6-sol`, effort `xhigh`

## Objective

Replace provider-shaped capability guesses with an isolated hybrid evidence pipeline:

`raw ACP frames -> evidence ledger -> semantic capabilities -> policy reducer -> exactly one dispatch`

The migration remains staged. Legacy behavior stays available as a bounded shadow fallback until Grok/Kiro replay parity and live post-probe agreement are demonstrated. Claude Code authentication failure remains an evidence gap.

## Formal contract

- Router cell `bd350001-0000-4000-8000-000000000003`: `relational_lia@1`, 7/7 matched, report `63631bebb75ffa69cdeecc8b3ae686d5a91396dfab030beb7d536842389c338d`.
- Lifecycle cell `bd350001-0000-4000-8000-000000000005`: `state_invariant_lia@1`, 8/8 matched, report `e81e519f56896645475eae4af1344e437575e7b9941fbc29100a89d06361d923`.
- Every task runs `solve_rule_spec` before RED when a catalog family may own the constraint and records either the selected rule/profile or why it is not applicable.
- Every implementation task follows: pre-evaluation -> failing test -> minimal implementation -> same post-evaluation -> affected regressions.
- Rust commands use `scripts/spur-cargo`; never bare `cargo`.
- Live probes must not send billed prompts or expose secrets.

## Dependency graph

```text
bd-1tc4 probe contract ------------------------------+
                                                       +--> bd-30s6 isolated cache --+
bd-b61n evidence kernel --> bd-3ah7 raw capture ------+                              |
                                |                                                     +--> bd-24no TUI routing --> bd-2f4b live replay
                                +-----------------------------------------------------+
```

The submitted plan materializes separate execution beads while retaining the
stable task IDs used below:

| Task ID | Execution bead |
|---|---|
| `bd-1tc4` | `bd-2ntt` |
| `bd-b61n` | `bd-3196` |
| `bd-3ah7` | `bd-2fb2` |
| `bd-30s6` | `bd-2ixh` |
| `bd-24no` | `bd-q9eg` |
| `bd-2f4b` | `bd-331w` |

## Task 1 — `bd-1tc4`: Freeze provider-neutral probe and fixture contract

**Owned writes**

- `scripts/probe_acp_capabilities.py`
- `scripts/test_probe_acp_capabilities.py`
- `scripts/fixtures/acp_capabilities/README.md`

**Pre/RED**

1. Preserve the current Grok/Kiro report shape from the existing non-billed probe invocation.
2. Add failing tests for a versioned identity/raw/evidence/fixture contract.
3. Add failing cases proving recipes do not advertise support, dynamic choices come from payloads, authentication/timeouts are inconclusive, and secrets are redacted.

**Green/post**

1. Add the smallest additive schema while keeping existing report fields compatible.
2. Emit provenance-tagged claims and stable raw digests suitable for fixture replay.
3. Re-run `python3 -m unittest scripts/test_probe_acp_capabilities.py` and the same non-billed probe scenarios.

## Task 2 — `bd-b61n`: Add evidence kernel and deterministic reducer

**Owned writes**

- `crates/spur-acp/src/capability_evidence.rs` (new)
- `crates/spur-acp/src/lib.rs` (module export only)

**Pre/RED**

1. Bind unit tests to the approved Hidden/PromptOnly/NativePreferred lifecycle and router partition.
2. Cover every formal witness, coverage, exclusivity, determinism, initiation, and preservation branch.
3. Prove unknown/rejected/inconclusive/recipe-only inputs cannot native-enable a capability.

**Green/post**

1. Implement provider-neutral keys, records, provenance, identity, immutable epochs, confidence, route, and pure reducer types.
2. Return exactly one route per reduced capability.
3. Run `scripts/spur-cargo test -p spur-acp capability_evidence`.

## Task 3 — `bd-3ah7`: Capture raw ACP evidence and adapt the compatibility facade

**Depends on:** `bd-b61n`

**Owned writes**

- `crates/spur-acp/src/connection/native.rs`
- `crates/spur-acp/src/spur_agent_caps.rs`
- `crates/spur-acp/tests/executor_events_roundtrip.rs`

**Pre/RED**

1. Add a round-trip failure showing top-level/vendor fields lost by typed projection.
2. Add lifecycle cases for initialize, new/load session, notifications, rejection, timeout, and authentication failure.
3. Add a shadow-parity test for current Grok/Kiro routes and a no-same-action-fallback test.

**Green/post**

1. Capture raw envelopes before deserialization and normalize them into evidence records.
2. Expose the reducer through `SpurAgentCaps` as a compatibility facade while retaining bounded legacy shadow behavior.
3. Preserve ACP sequencing and notification bounds; run `scripts/spur-cargo test -p spur-acp`.

## Task 4 — `bd-30s6`: Evolve the model catalog into an isolated evidence cache

**Depends on:** `bd-1tc4`, `bd-b61n`, `bd-3ah7`

**Owned writes**

- `crates/spur-acp/src/agent_model_catalog.rs`
- `crates/spur-acp/tests/agent_model_catalog.rs`

**Pre/RED**

1. Fail tests for identity drift, incompatible schema, atomic commit, TTL, coalescing, and inconclusive failures.
2. Assert that a partial/failed probe cannot promote NativePreferred.
3. Assert no active user session or billed prompt is used.

**Green/post**

1. Version the cache around CLI identity, evidence epoch, provenance, and reduced snapshot.
2. Coalesce concurrent misses into one isolated ephemeral probe and atomically publish only complete epochs.
3. Preserve existing TTL semantics; run `scripts/spur-cargo test -p spur-acp --test agent_model_catalog`.

## Task 5 — `bd-24no`: Route TUI commands from reduced evidence exactly once

**Depends on:** `bd-3ah7`, `bd-30s6`

**Owned writes**

- `crates/spur-tui/src/commands/advertised.rs`
- `crates/spur-tui/src/commands/registry.rs`
- `crates/spur-tui/src/commands/submit_router.rs`

**Pre/RED**

1. Reproduce dynamic/synthetic `/model` collision and duplicate-dispatch risk.
2. Assert Hidden is absent, PromptOnly is prompt-routed, and NativePreferred is native-routed.
3. Assert one pinned evidence epoch per action and no prompt resend after native failure.

**Green/post**

1. Reduce/deduplicate before registry insertion.
2. Bind each entry to one route and one epoch at dispatch start.
3. Run `scripts/spur-cargo test -p spur-tui commands::` plus existing picker/model/effort/mode tests.

## Task 6 — `bd-2f4b`: Replay fixtures and run live post-probes

**Depends on:** `bd-24no`

**Owned writes**

- New sanitized files under `scripts/fixtures/acp_capabilities/`
- `docs/superpowers/evals/2026-09-01-dynamic-acp-capability-evidence.md` (new)

**Evaluation**

1. Replay sanitized Grok 1.0.13 and Kiro 2.20.2 fixtures and compare exact reduced routes.
2. Run the same live non-billed probes used for the pre-evaluation.
3. Confirm Grok dynamic models/efforts (including `xhigh`) and `model_changed` evidence.
4. Confirm Kiro 9 models, 3 modes, 25 commands, accepted direct set calls, and `-32601` options-method evidence.
5. Record Claude authentication as inconclusive. If live/replay behavior disagrees with the reducer, emit a scope/blocker signal instead of editing production files.
6. Run targeted `spur-acp` and `spur-tui` regressions through `scripts/spur-cargo`.

## Review gate

For every worker task, the brain reviews the issue state, signal labels/comments, worker branch diff, RED evidence, GREEN/post-evaluation evidence, and affected regression output before approval. The final merge is allowed only when the implementation epic has no unresolved blocker/scope-change signals and Task 6 reports either agreement or an explicit, reviewed evidence gap.
