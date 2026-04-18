# DuckDB Analytics Architecture

**Status:** Initial technical design  
**Last updated:** 2026-04-18  
**Owner:** Kevin  
**Proposed crate:** `spur-analytics`  
**Related crates:** `spur-core`, `spur-acp`, `spur-cost`, `spur-cli`

---

## Executive Summary

SPUR should use DuckDB as an embedded analytics warehouse over:

- SPUR NDJSON event logs
- SQLite cost history
- vendor session archives such as `~/.claude/projects/**` and `~/.kiro/sessions/cli/**`

This design does **not** replace `spur-cost`'s SQLite database. SQLite remains
the hot-path operational ledger. DuckDB becomes the read-heavy warehouse for:

- cost and token analytics
- burn-rate and forecast reporting
- routing and delegation quality analysis
- user and agent behavior analysis

The key architectural choice is to normalize vendor-specific JSON/JSONL in Rust
before building analytical marts in DuckDB. Raw SQL alone should not own vendor
schema drift.

---

## Problem

SPUR already emits and stores useful operational signals, but they are split
across multiple systems:

- `.spur/events/*.ndjson` contains replayable runtime events
- `.spur/cost.db` contains session and delegation cost history
- vendor archives contain richer token, tool, and message data than SPUR
  currently persists

This leaves four product questions only partially answerable today:

1. Which projects, sessions, agents, and models consume the most money?
2. What is the current token/context burn rate, and what will a session likely
   cost if it continues?
3. Which delegation and routing patterns correlate with success, conflict,
   timeout, or review churn?
4. What user interaction patterns appear over time across sessions and agents?

---

## Grounded Current State

This design is grounded in the current code and sampled local data, not an
imagined future pipeline.

### Current sources

| Source | Current location | What it contains | Current issue |
|---|---|---|---|
| SPUR event logs | `.spur/events/*.ndjson` | ordered `SpurEvent` stream with `seq`, `occurred_at`, lifecycle, delegation, worker activity, `AgentNotification` payloads | code writes repo-local path, while some docs still describe `~/.spur/events` |
| Cost DB | `~/.spur/cost.db` | sessions, delegation log, summary-friendly ledger | good operational store, weak for analytics joins |
| Claude archives | `~/.claude/projects/**.jsonl` | session metadata, messages, tool calls, token usage, cwd, branch, timestamps | vendor-specific schema, not normalized |
| Kiro archives | `~/.kiro/sessions/cli/*.jsonl` | prompt/assistant/tool/result history | different schema, sampled lines did not expose token counts |

### What SPUR already emits

SPUR's event contract already includes:

- `BrainSpawned`
- `AgentSessionReady`
- `DelegationRequested`
- `DelegationCompleted`
- `WorkerProgress`
- `WorkerFileTouched`
- `AgentNotification`
- `SessionHistory`
- `CostUpdate`

However, the current implementation has important gaps:

1. `CostUpdate` exists in the event contract but is not currently emitted by
   the runtime.
2. `SessionUpdate::UsageUpdate` is visible in the TUI and event logs, but is
   not persisted into a canonical analytics store.
3. `spur-cost` stores heuristic end-of-session dollars, but not the richer
   vendor usage detail that already exists in some agent archives.

### Evidence from current code

- Event sink: `crates/spur-core/src/event_sink.rs`
- Event contract: `crates/spur-acp/src/domain/events.rs`
- Cost tracker: `crates/spur-cost/src/db.rs`, `crates/spur-cost/src/tracker.rs`
- Kiro disk reader: `crates/spur-core/src/orchestrator.rs`
- Claude stream-json adapter: `crates/spur-acp/src/connection/stream_json_adapter.rs`
- Claude result schema: `crates/spur-acp/src/protocol/claude_events.rs`

---

## Design Goals

1. Provide a single embedded analytics engine for SPUR and vendor session data.
2. Preserve raw data for audit and reprocessing.
3. Normalize vendor-specific schemas into stable canonical facts.
4. Support batch refresh with deterministic replay and no double-counting.
5. Keep operational writes simple and low-risk.
6. Make cost, usage, and routing reports queryable from the CLI.

## Non-Goals

1. Replacing SQLite as the hot operational store for `spur-cost`.
2. Real-time multi-process writes directly into one shared DuckDB database file.
3. Solving all semantic memory and graph-recall use cases in phase 1.
4. Building a hosted dashboard in the initial rollout.

---

## First Principles

This design follows four first-principles constraints:

1. **Analytics is downstream of execution.**
   SPUR's job is to orchestrate work first, then explain and optimize it.

2. **Source schemas are unstable.**
   Vendor JSON formats will change. Rust adapters should absorb that drift.

3. **Warehouse writes are not hot-path writes.**
   The analytics system should refresh from durable sources, not sit in the
   critical path of every running session.

4. **Canonical facts matter more than raw logs.**
   The useful unit is not "a JSON line" but a session, turn, tool call, usage
   sample, delegation, artifact, and outcome.

---

## Proposed Architecture

### High-level split

- `spur-cost` stays SQLite-backed and remains the operational ledger.
- `spur-analytics` is a new crate that builds a DuckDB warehouse over raw and
  normalized data.
- Rust source adapters normalize vendor and SPUR logs into canonical facts.
- DuckDB stores landing tables, core facts, and derived marts.

### Write model

DuckDB is used by **one analytics writer process** at a time, not by every
running SPUR process.

Initial model:

- `spur analytics refresh` scans source files and refreshes warehouse tables
- `spur analytics report ...` reads the warehouse
- later, a background daemon can be added if needed

This keeps the runtime simple and avoids coupling analytics writes to
orchestrator latency.

### Proposed crate structure

```text
crates/spur-analytics/
  src/
    lib.rs
    db.rs
    schema.rs
    refresh.rs
    discover.rs
    identity.rs
    sources/
      spur_events.rs
      spur_cost.rs
      claude_jsonl.rs
      kiro_jsonl.rs
    normalize/
      sessions.rs
      turns.rs
      usage.rs
      tools.rs
      delegations.rs
      artifacts.rs
    reports/
      burn_rate.rs
      forecast.rs
      routing.rs
      user_patterns.rs
```

---

## Data Flow

```text
source files / databases
  -> discovery
  -> raw landing tables
  -> Rust normalization
  -> core fact tables
  -> mart tables
  -> CLI reports / ad hoc SQL
```

### Source discovery

The first version should discover:

- `.spur/events/**/*.ndjson`
- configured cost DB path (default `~/.spur/cost.db`)
- `~/.claude/projects/**/*.jsonl`
- `~/.kiro/sessions/cli/*.{json,jsonl}`

Discovery should record:

- absolute path
- source kind
- file size
- mtime
- content hash or stable checksum
- ingest run id

### Raw landing

Raw landing tables preserve replayability and forensic access.

Suggested raw tables:

- `raw.files`
- `raw.spur_events`
- `raw.claude_jsonl`
- `raw.kiro_jsonl`
- `raw.cost_sessions`
- `raw.cost_delegations`

### Rust normalization layer

Rust adapters convert source-specific records into canonical records.

This is the critical abstraction boundary. SQL should not be the only place
that understands vendor schema details such as:

- Claude assistant `usage` objects
- Kiro `kind` and `content` layouts
- SPUR `AgentNotification` payload variants

Suggested canonical types:

- `SessionFact`
- `TurnFact`
- `UsageSampleFact`
- `ToolCallFact`
- `ToolResultFact`
- `DelegationFact`
- `FileTouchFact`
- `ArtifactFact`
- `CostFact`

### Warehouse layers

#### `raw`

Exact source preservation and low-level traceability.

#### `core`

Canonical facts used by reports and downstream joins.

#### `mart`

Opinionated derived datasets for CLI reports and future dashboards.

---

## Canonical Fact Model

### `core.sessions`

One row per canonical session.

Core fields:

- `canonical_session_id`
- `spur_session_id`
- `acp_session_id`
- `vendor_session_id`
- `vendor`
- `agent`
- `model`
- `role`
- `project_root`
- `worktree_path`
- `git_branch`
- `started_at`
- `ended_at`
- `status`

### `core.turns`

One row per user/assistant turn or replayable turn boundary.

Core fields:

- `canonical_turn_id`
- `canonical_session_id`
- `turn_index`
- `started_at`
- `ended_at`
- `user_text`
- `assistant_text`
- `source_kind`

### `core.usage_samples`

Supports both token-based and context-window-based usage.

Core fields:

- `canonical_session_id`
- `occurred_at`
- `vendor`
- `agent`
- `model`
- `context_used`
- `context_size`
- `input_tokens`
- `output_tokens`
- `cache_creation_input_tokens`
- `cache_read_input_tokens`
- `estimated_cost_usd`
- `source_kind`

### `core.tool_calls`

Core fields:

- `canonical_tool_call_id`
- `canonical_session_id`
- `canonical_turn_id`
- `occurred_at`
- `tool_name`
- `tool_family`
- `raw_input_json`
- `raw_output_json`
- `status`
- `cwd`

### `core.delegations`

Core fields:

- `delegation_id`
- `brain_session_id`
- `worker_session_id`
- `request_id`
- `to_agent`
- `task`
- `issue_id`
- `requested_at`
- `completed_at`
- `status`
- `review_outcome`
- `cost_usd`

### `core.file_touches`

Core fields:

- `canonical_session_id`
- `executor_id`
- `occurred_at`
- `path`
- `kind`
- `source_kind`

### `core.cost_facts`

Core fields:

- `canonical_session_id`
- `occurred_at`
- `cost_usd`
- `cost_origin` (`heuristic_session_end`, `vendor_reported`, `derived_from_tokens`)
- `vendor`
- `agent`

---

## Session Identity Strategy

Identity stitching is required because one logical session can appear under
different ids in different systems.

The warehouse should maintain a mapping strategy across:

- SPUR session id
- ACP session id
- vendor session id
- cwd / project root
- agent name
- close timestamp proximity

Initial rule:

1. Prefer explicit IDs when SPUR already knows both sides.
2. Fall back to deterministic heuristics only when explicit linkage is absent.
3. Store confidence and origin of the match.

Suggested table:

- `core.session_identity_edges`

Fields:

- `left_id`
- `right_id`
- `edge_kind`
- `confidence`
- `match_reason`

---

## Derived Marts

### `mart.burn_rate`

Answers:

- current context burn per minute
- current token burn per minute
- dollar burn per minute

Dimensions:

- project
- agent
- model
- vendor
- hour/day

### `mart.cost_forecast`

Answers:

- predicted session-end cost
- predicted token total
- projected context saturation time

Method:

- rolling slope from `core.usage_samples`
- optional fallback to duration-based heuristic if token data is absent

### `mart.routing_effectiveness`

Answers:

- success/failure/timeout/conflict rates by agent and task type
- review rejection and retry rates
- cost per successful delegation
- average time to completion

### `mart.user_patterns`

Answers:

- active hours
- interruption frequency
- context-reset frequency
- average prompt size
- tool mix per session
- worker vs brain workload split

---

## CLI Surface

Add to `spur-cli`:

```text
spur analytics refresh
spur analytics query "<sql>"
spur analytics report burn-rate
spur analytics report forecast
spur analytics report routing
spur analytics report user-patterns
```

Phase 1 should optimize for:

- human-readable terminal reports
- CSV export
- direct SQL for power users

---

## Required Runtime Instrumentation Changes

The warehouse can start from current sources, but these runtime changes will
materially improve report accuracy.

1. Emit `SpurEventBody::CostUpdate` from the runtime.
2. Persist canonical usage facts from `SessionUpdate::UsageUpdate`.
3. Standardize the event-log root path. Pick one path and make code/docs match.
4. Thread stable source identity between SPUR session ids, ACP session ids, and
   vendor session ids wherever available.
5. Optionally emit compact normalized ingest files later, but only after the
   file-scanning path is proven.

---

## Validation Strategy

Analytics bugs fail silently. Validation must be explicit.

### Grounding tests

1. Replay the same SPUR event files twice and prove `core.*` row counts do not
   double.
2. Parse a real Claude fixture and assert normalized token totals match fixture
   totals.
3. Parse a real Kiro fixture and assert prompt/tool/result counts match the
   source file.
4. Join a known session from the SQLite cost DB and the event log into one
   canonical session row.
5. Assert monotonic `seq` ordering within each SPUR event file.
6. Assert `usage_update.used` is non-decreasing within a sampled session unless
   the source explicitly resets.

### Warehouse invariants

1. Ingest must be idempotent for unchanged files.
2. Every mart row must be explainable from `core.*`.
3. Every `core.*` record must be traceable back to a `raw.*` record.
4. Session identity matches must record confidence and reason.

---

## Rollout Plan

### Phase 1

- create `spur-analytics`
- ingest SPUR event logs
- attach SQLite cost DB
- ingest Claude and Kiro archives
- build `raw` and `core`
- ship burn-rate and routing reports

### Phase 2

- add forecasting marts
- add better identity stitching
- emit runtime `CostUpdate`
- improve usage normalization across transports

### Phase 3

- add Parquet export / snapshotting
- optional FTS over prompts and tool output
- optional always-on refresh daemon
- feed future `spur-context` memory and router features from the same warehouse

---

## Open Questions

1. Should `spur-analytics` be a separate crate, or should it land under the
   planned `spur-context` crate with a dedicated schema namespace?
2. What should be the single canonical event path: repo-local `.spur/events` or
   user-global `~/.spur/events`?
3. Should warehouse refresh prefer direct JSON scans every run, or materialize
   immutable Parquet snapshots after normalization?
4. How aggressively should identity stitching infer links when explicit ids are
   missing?
5. Should vendor-reported costs override heuristic session-end costs, or should
   both be stored side by side and selected at query time?

---

## Recommendation

Proceed with a new `spur-analytics` crate that uses DuckDB as the embedded
warehouse, while keeping `spur-cost` on SQLite.

The minimum viable architecture is:

- SQLite for operational writes
- DuckDB for analytical reads
- Rust adapters for normalization
- CLI-driven refresh and report generation

This yields immediate leverage on real SPUR and vendor data without introducing
high-risk runtime coupling or forcing an early migration of the operational
store.
