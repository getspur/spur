# SPUR Graph Engine — Design Spec

| | |
|---|---|
| Status | Draft |
| Date | 2026-05-05 |
| Author | brain (synthesized from codex feasibility review + multi-round MCTS evaluation) |
| Replaces | external `bv` (beads_viewer) Go binary subprocess |
| Companion spec | `2026-05-05-beads_rust-direct-crate-dep-design.md` (BeadsCrateAdapter — strict prerequisite) |

---

## TL;DR

Replace SPUR's external `bv` (beads_viewer) subprocess with a native Rust **graph engine** living inside `crates/spur-pm/src/graph_engine/`. The engine reads `.beads/` issues + dependencies through `BeadsCrateAdapter::read` (a single `&SqliteStorage` closure), builds an in-memory `GraphSnapshot` value, and runs five pure-function analyzers (triage, plan, insights, alerts, subgraph) that produce reports byte-compatible with the existing wire types in `crates/spur-pm/src/graph.rs`. `BvAdapter`'s public method signatures are preserved; only its internals swap. SPUR adopts **owned, documented scoring semantics** rather than attempting bv parity (which is impossible without reading bv's source — license forbids it for Anthropic-driven work). Sequenced after the in-flight `BeadsCrateAdapter` cutover; developed in parallel using `TestBeadsWorkspace` fixtures.

---

## Background

### What `bv` is

`bv` is an external Go binary (`brew install dicklesworthstone/tap/bv`, ~438 .go files) that performs graph analysis over a `.beads/` issue store: PageRank, betweenness centrality, HITS, k-core, articulation points, cycles, plus higher-level reports (project triage, parallel execution plan, alerts). SPUR consumes only its **robot protocol** — five JSON-output commands — through `BvAdapter` at `crates/spur-pm/src/bv.rs`.

### Why replace it

1. **Install dependency**: every SPUR install must `brew install` a separate binary on each machine.
2. **Subprocess overhead**: each call is fork + exec + JSON parse, repeated by reconciler tick, MCP tool calls, orchestrator orientation, and TUI status bar refresh.
3. **Type drift surface**: bv's JSON schema can change between versions; we discover via parse failures.
4. **Operational opacity**: subprocess output is opaque to SPUR's tracing/metrics layer.
5. **Alignment**: SPUR is mid-cutover from `Command::new("br")` shellouts to direct `beads_rust` crate linkage (companion spec). Replacing `bv` subprocess with native graph code completes that alignment — every `.beads/` operation becomes an in-process Rust call.

### License constraint (HARD)

The upstream `bv` repository (https://github.com/Dicklesworthstone/beads_viewer) is licensed **MIT with an OpenAI/Anthropic Rider** that explicitly forbids derivative works by, for, or under the direction of Anthropic or OpenAI, where "use" is expanded to include "analyzing… or incorporating the Software… into any… pipeline for machine learning or other automated systems."

**Implication:** SPUR cannot read bv's source code with the goal of re-implementation when that work is driven through Claude (Anthropic). This rules out exact behavioral parity — we cannot know bv's scoring formulas, track-grouping rules, alert thresholds, or category definitions without reading source.

**Resolution:** SPUR adopts **owned semantics**. The analyzers compute well-known graph algorithms and produce wire-compatible JSON, but the formulas, weights, thresholds, and category definitions are SPUR's own — documented in this spec and the code, configurable in `spur.config`, tunable as the brain feedback loop surfaces issues.

### `BeadsCrateAdapter` is a strict prerequisite

The companion spec replaces SPUR's `Command::new("br")` shellouts with `BeadsCrateAdapter` — direct linkage to `beads_rust` 0.2.1, exposing `read`/`write`/`batch` primitives over typed `SqliteStorage` connections. After that cutover, `crates/spur-pm/src/beads.rs` is deleted.

The graph engine **builds on `BeadsCrateAdapter::read`** for data access. It does not open `.beads/beads.db` directly, does not invent its own SQLite reader, does not couple to schema internals. Loader runs inside one `read` closure (honoring "no DB call across `.await`" discipline), returns a `GraphSnapshot` value, and analysis runs on the value with zero further IO.

### Current scale

`.beads/issues.jsonl` = 406 lines; `.beads/beads.db` = 7.7 MB. At this scale, full graph analysis is sub-millisecond. Performance is not a design driver; correctness and maintainability are.

---

## Goals

1. **Eliminate the `bv` install dependency.** After cutover, `BvAdapter` runs entirely in-process; the external binary is no longer required, no longer installed by any setup script, no longer probed at startup.
2. **Wire-compatible JSON output.** All five existing typed reports (`TriageReport`, `ExecutionPlan`, `GraphInsights`, `AlertReport`, `DependencyGraph` in `crates/spur-pm/src/graph.rs`) deserialize unchanged. The `raw: serde_json::Value` field that MCP passes to brain agents is generated from the typed reports.
3. **Owned, documented semantics.** Every scoring formula, threshold, and category definition is named, justified, configurable, and tested.
4. **Stable `data_hash`.** Reports include a deterministic hash of the loaded snapshot so the reconciler's cache invalidation continues to work.
5. **Zero changes to call sites.** `BvAdapter`'s 5 public method signatures and return types are unchanged. MCP server, reconciler, orchestrator, TUI, and tests do not edit.

## Non-goals

- **Behavioral parity with `bv`.** Different scoring → reconciler may pick a different next task than bv would. This is accepted; brain feedback loop catches genuine problems.
- **Multi-process safety beyond what `BeadsCrateAdapter` already provides.** Graph reads are lock-free snapshot reads through the adapter's reader pool.
- **Streaming / incremental analysis.** Each call loads a full snapshot. At 10k+ issues with sparse dependency graphs this remains fast; if scale changes, revisit.
- **The `bv` UI / TUI / web interface.** SPUR never used these.
- **Fork or modify `bv`.** Forbidden by license rider; not needed regardless.
- **Replacement of `BeadsCrateAdapter` itself.** That's the companion spec's domain.

---

## Architecture

### Module layout

```
crates/spur-pm/src/
  beads_crate/                # Companion spec — already partially built
    mod.rs
    init.rs
    reader_pool.rs
    snapshot.rs
    backoff.rs
    metrics.rs
  graph_engine/               # NEW — this spec's central artifact
    mod.rs                    # GraphEngine struct + 5 public async methods
    snapshot.rs               # GraphSnapshot value type + load_graph_snapshot fn
    score.rs                  # SPUR-owned scoring formulas + ScoreConfig
    metrics.rs                # HITS, Brandes' betweenness, k-core, critical-path
    triage.rs                 # GraphSnapshot → TriageReport
    plan.rs                   # GraphSnapshot → ExecutionPlan
    insights.rs               # GraphSnapshot → GraphInsights
    alerts.rs                 # GraphSnapshot → AlertReport
    subgraph.rs               # GraphSnapshot → DependencyGraph
    raw.rs                    # serializes typed reports → serde_json::Value
  graph.rs                    # Wire types — UNCHANGED
  bv.rs                       # BvAdapter — internals swapped, signatures unchanged
  service.rs                  # PmService construction wiring — minimal change
```

### `GraphSnapshot` value type

```rust
// graph_engine/snapshot.rs
pub struct GraphSnapshot {
    /// Petgraph-backed directed graph: edge from `blocker` → `blocked`.
    pub graph: petgraph::Graph<NodeData, EdgeData, Directed>,
    /// O(1) ID → NodeIndex lookup.
    pub by_id: HashMap<String, NodeIndex>,
    /// Snapshot metadata.
    pub generated_at: DateTime<Utc>,
    pub data_hash: String,            // SHA256, see "data_hash strategy"
    pub label_filter: Option<String>, // None = whole project
}

pub struct NodeData {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub due_at: Option<DateTime<Utc>>,
    pub content_hash: String, // from beads_rust Issue
}

pub struct EdgeData {
    pub kind: DependencyKind, // Blocks, ParentChild, ConditionalBlocks, WaitsFor, RelatedTo, ...
}
```

`DependencyKind::is_blocking()` returns `true` for `Blocks | ParentChild | ConditionalBlocks | WaitsFor` (matching the existing `BLOCKING_TYPES` constant in `beads.rs:166`).

### Loader

```rust
// graph_engine/snapshot.rs
pub fn load_graph_snapshot(
    storage: &beads_rust::storage::sqlite::SqliteStorage,
    label_filter: Option<&str>,
) -> Result<GraphSnapshot>;
```

**Pre-flight check:** before this spec is implemented, verify `beads_rust` 0.2.1 exposes `list_issues`, `get_dependencies` (or equivalent batch listing), and `Issue::content_hash` on its public API. If a "list all dependencies in one query" call doesn't exist, the loader falls back to one raw SQL query through the `&SqliteStorage`'s underlying `rusqlite::Connection` — the schema is bounded in scope and the loader is the only place that knows it.

The loader is called inside `BeadsCrateAdapter::read(|s| ...)`. The closure runs to completion synchronously inside a `spawn_blocking` thread; the resulting `GraphSnapshot` value is `Send` and is returned to the async runtime.

### Analyzer

Five pure functions, one per command:

```rust
// graph_engine/triage.rs
pub fn compute_triage(snap: &GraphSnapshot, cfg: &ScoreConfig) -> TriageReport;

// graph_engine/plan.rs
pub fn compute_plan(snap: &GraphSnapshot, cfg: &ScoreConfig) -> ExecutionPlan;

// graph_engine/insights.rs
pub fn compute_insights(snap: &GraphSnapshot, cfg: &InsightConfig) -> GraphInsights;

// graph_engine/alerts.rs
pub fn compute_alerts(snap: &GraphSnapshot, cfg: &AlertConfig) -> AlertReport;

// graph_engine/subgraph.rs
pub fn compute_subgraph(snap: &GraphSnapshot, root: SubgraphRoot, depth: Option<u32>, format: GraphFormat) -> DependencyGraph;
```

All take a `&GraphSnapshot` and a config; all return a typed report. **Zero IO, zero async, zero side effects.** Tests construct `GraphSnapshot` values directly without mocks.

### `GraphEngine` facade

```rust
// graph_engine/mod.rs
pub struct GraphEngine {
    beads: Arc<BeadsCrateAdapter>,
    score_cfg: ScoreConfig,
    insight_cfg: InsightConfig,
    alert_cfg: AlertConfig,
}

impl GraphEngine {
    pub async fn triage(&self, label: Option<&str>) -> Result<TriageReport> {
        let snap = self.beads.read(move |s| load_graph_snapshot(s, label.as_deref())).await?;
        let mut report = compute_triage(&snap, &self.score_cfg);
        report.raw = raw::serialize_triage(&report);
        Ok(report)
    }
    // ... and 4 more, identical shape
}
```

### `BvAdapter` swap

```rust
// crates/spur-pm/src/bv.rs (after cutover)
pub struct BvAdapter {
    engine: GraphEngine,
}

impl BvAdapter {
    pub async fn connect(beads: Arc<BeadsCrateAdapter>, cfg: GraphConfig) -> Result<Self> {
        Ok(Self { engine: GraphEngine::new(beads, cfg) })
    }

    pub async fn triage(&self, label: Option<&str>) -> Result<TriageReport> {
        self.engine.triage(label).await
    }
    // ... 4 more, identical pass-through
}
```

The public method signatures match the current `BvAdapter` exactly. Call sites in `spur-mcp/src/server.rs:{4373,4392,4411,4429,4453}`, `spur-pm/src/service.rs:{210,214}`, `spur-core/src/orchestrator.rs:1621`, and tests do not change.

### `PmService` wiring

`PmService::try_new` (in `service.rs`) constructs `BvAdapter::connect(beads_crate_adapter, graph_cfg)` instead of probing for the `bv` binary. The `Option<BvAdapter>` field becomes unconditionally `Some(...)` — there is no longer a "graph engine missing" degraded mode.

### Data flow (one analysis call)

```
caller (MCP/reconciler/orchestrator/TUI)
   │  bv.triage(label)
   ▼
BvAdapter::triage
   │  → GraphEngine::triage
   ▼
GraphEngine::triage
   │  beads.read(|s| load_graph_snapshot(s, label))
   ▼
BeadsCrateAdapter::read   ──── spawns blocking thread ────▶  load_graph_snapshot(&SqliteStorage, label)
   │                                                            │  query issues + deps via beads_rust API
   │                                                            │  build petgraph + node table
   │   ◀── returns GraphSnapshot ──────────────────────────────┘
   │
   │  compute_triage(&snap, &score_cfg)            (pure, on async runtime)
   │  raw::serialize_triage(&report)               (pure)
   ▼
TriageReport (with raw populated) → caller
```

---

## SPUR-owned scoring semantics

This is the central design output. Every formula here is the contract.

### `ScoreConfig`

Configurable in `spur.config` under `[graph.score]`. Defaults shipped in `score.rs`.

```toml
[graph.score]
# Triage Recommendation.score weights (sum need not equal 1; result is normalized post hoc)
weight_priority    = 0.30
weight_unblocks    = 0.25
weight_actionable  = 0.20
weight_freshness   = 0.15
weight_age_penalty = 0.10

# Quick win threshold: a "quick win" is an actionable issue with score above this percentile
quick_win_percentile = 0.75

# Tie-break order when scores are equal:
# 1. lower priority number wins (0=highest)
# 2. higher transitive_unblocks_count wins
# 3. lexicographic id (stable, deterministic)
```

### `triage` — `TriageReport`

For each open or in_progress issue, compute:

- `is_actionable` = open OR in_progress, AND all blocking-typed in-edges have `status == closed`.
- `transitive_unblocks` = size of the set of issues reachable via blocking out-edges (BFS).
- `score` (0..1, after normalization) =

  ```
  s = w_p   * (5 - priority) / 5            # priority component, P0=1.0, P4=0.0
    + w_u   * log10(1 + transitive_unblocks)
    + w_a   * (1 if is_actionable else 0)
    + w_f   * sigmoid((30 - days_since_update) / 30)
    + w_age * sigmoid((age_days - 60) / 30)  # mild penalty for old open work
  ```

  Normalize across the candidate set so max score = 1.0. Recommendations sorted desc by score; ties broken by the rule above.

Outputs:

- `quick_ref.top_picks` = top 5 by score (id, title, score, reasons[], unblocks count).
- `recommendations` = top 20 by score with full breakdown.
- `quick_wins` = actionable issues whose `priority >= 2` (low/medium) AND whose score is in the top `quick_win_percentile` — the "small effort, high payoff" bucket.
- `blockers_to_clear` = closed-set complement of `is_actionable` whose `transitive_unblocks_count >= 1`, sorted desc by unblocks count.
- `project_health` = `HealthCounts` (totals), `GraphHealth` (node/edge/density/has_cycles/cycle_count from `tarjan_scc`), optional `velocity` (rolling 7/30 day closes from `closed_at` if present), optional `staleness` (count over `stale_threshold_days`, see alerts).
- `alerts` = top 5 from `compute_alerts` (sev != info), as inline summaries.
- `commands` = `serde_json::Value::Null` (no shell hint generation needed).

`reasons` are short human-readable phrases derived from breakdown deltas: e.g. `["P1 priority", "unblocks 3 downstream", "actionable", "fresh (updated 2d ago)"]`.

### `plan` — `ExecutionPlan`

Tracks are **topological generations** of the actionable subgraph:

- Track 0 = all actionable issues with no blocking in-edges from other open issues.
- Track i = issues whose blocking parents are all in tracks `< i`.
- `track_id` = `"track-A"`, `"track-B"`, ... (Latin alphabetical for stable display).
- Each `TrackItem` = { id, title, priority, status, unblocks: Some(downstream_ids) | None }.
- `reason` = templated: `"Generation N — depends on M completed track(s) above"` or `"Single actionable item"` (track 0 with one item).
- `total_actionable` = count, `total_blocked` = count of open issues with at least one open blocking parent.
- `summary.highest_impact` = id with max `transitive_unblocks_count`; `impact_reason` = templated; `unblocks_count` = that count.

### `insights` — `GraphInsights`

Categories — owned definitions:

| Field | Definition |
|---|---|
| `Bottlenecks` | Top-K by **betweenness centrality** (Brandes' algorithm). High betweenness = many shortest paths pass through. |
| `Keystones` | Top-K by **articulation × out-degree** product. Articulation points whose removal disconnects multiple downstream issues. |
| `Influencers` | Top-K by **PageRank** (petgraph `page_rank` if signature fits, else custom 30-LOC iterative). Damping = 0.85, iterations = 100. |
| `Hubs` | Top-K by **HITS hub score** (issues that point to many authorities). Iterative, 50 iterations. |
| `Authorities` | Top-K by **HITS authority score**. Same algorithm, dual side. |
| `Cores` | Top-K nodes by **k-core decomposition** rank (highest k value wins). |
| `Articulation` | Vector of node IDs from `petgraph::algo::articulation_points`. |
| `Orphans` | Vector of node IDs with degree 0 in the subgraph induced by `label_filter`. |
| `Cycles` | `tarjan_scc` SCCs with size > 1, returned as `Vec<Vec<String>>` of node IDs. |
| `ClusterDensity` | `2 * edge_count / (node_count * (node_count - 1))` for the loaded subgraph. |

`top_what_ifs` is left **empty `Vec<>`** in v1 — counterfactual analysis is deferred. The wire field stays present (always serializes as `[]`) so callers don't break.

K = 10 by default for top-K lists, configurable via `[graph.insights]` section.

### `alerts` — `AlertReport`

Alert types and SPUR-owned thresholds (configurable):

| `alert_type` | Trigger | Severity |
|---|---|---|
| `stale` | open or in_progress AND `now - updated_at > stale_threshold_days` (default 14) | `warning` |
| `cycle` | issue is in any non-trivial SCC | `critical` |
| `cascade` | issue is open + blocking AND `transitive_unblocks_count >= cascade_threshold` (default 5) | `warning` |
| `priority_inversion` | issue blocks an issue with strictly higher priority (numerically lower) | `warning` |
| `orphan_high_priority` | priority ≤ 1 AND degree 0 in graph | `info` |

`summary` is the count by severity. `usage_hints` is a small fixed list of helpful next-step phrases (e.g., `"Use jq '.alerts[] | select(.severity==\"critical\")'"`).

### `subgraph` / `graph_by_label` — `DependencyGraph`

- `--graph-root=<id>`: BFS from root in **both** directions (incoming blocking + outgoing blocking), capped at `depth` (default 2).
- `--label <l>`: full induced subgraph over all issues carrying that label.
- Edge orientation: `from = blocker`, `to = blocked` (semantic: `from` must complete before `to`).
- Formats:
  - `json`: `adjacency` populated; `graph` field None.
  - `dot`: `graph` = deterministic Graphviz DOT string (sorted node + edge order); `adjacency` None.
  - `mermaid`: `graph` = `graph TD\n  N1[\"title\"] --> N2[\"title\"]\n ...`; `adjacency` None.
- `nodes` and `edges` counts always populated.
- `pagerank` field on `GraphNode` populated only when format == json (cheap on small subgraphs).

DOT and Mermaid output is **deterministic** — sorted by node ID then edge tuple.

---

## `data_hash` strategy

Every report includes a `data_hash` field that the reconciler uses for cache invalidation. Stability across invocations on the same DB state is required; instability would cause reconciler thrash.

**Algorithm:**

```
For each NodeData in the snapshot (sorted by id):
    h = SHA256(id || "\x1f" || content_hash || "\x1f" || sorted_labels.join(",") || "\x1f" || sorted_blocking_dep_ids.join(","))

snapshot.data_hash = SHA256(concat of all per-node h, in id-sorted order)
```

`content_hash` is taken from `beads_rust::Issue::content_hash` (already covers id, title, body, status, priority, type, assignee, due_at, version_counter). Adding labels and blocking-dep IDs to the per-node digest ensures schema fields the loader reads are also covered.

The hash is **deterministic** given identical DB state (no time-based inputs, no map iteration order) and **changes** when any included issue's content, labels, or blocking deps change.

Property test (in `graph_engine/snapshot.rs`): for any random `GraphSnapshot`, `data_hash` is invariant under reload from the same DB state and changes after any single mutation.

---

## Algorithm coverage

| Algorithm | Source |
|---|---|
| Topological sort | `petgraph::algo::toposort` |
| SCC (cycle detection + enumeration) | `petgraph::algo::tarjan_scc` |
| `is_cyclic_directed` | `petgraph::algo::is_cyclic_directed` |
| Articulation points | `petgraph::algo::articulation_points` |
| PageRank | `petgraph::algo::page_rank` if signature fits, else custom (~30 LOC) |
| BFS / DFS / reachability | `petgraph::visit::Bfs`, `Dfs` |
| HITS (hubs / authorities) | **Custom** ~30 LOC iterative |
| Betweenness centrality | **Custom** Brandes' algorithm ~80 LOC |
| k-core decomposition | **Custom** Matula-Beck ~50 LOC |
| Critical path on DAG | **Custom** topo + DP ~30 LOC |

Total custom algorithm code: ~250 LOC. All algorithms have well-known textbook descriptions; no peeking at `bv` source required.

`petgraph` is added as a workspace dep at the top-level `Cargo.toml` (currently absent — verified). Pin to a recent stable version (≥ 0.6).

---

## Testing strategy

### Unit tests (per analyzer file)

- Hand-built `GraphSnapshot` fixtures with 5–20 nodes covering: linear chain, diamond, fan-out, cycle, isolated nodes, articulation points.
- Each analyzer asserts the exact expected output structure on the fixture.
- Scoring formulas: golden values for each weight component, then composite scores at known weight settings.
- `data_hash` stability: build two identical snapshots from the same fixture, assert hashes match; mutate one node, assert hashes differ.

### Algorithm correctness tests

- HITS converges on standard textbook graphs (e.g., Kleinberg's example).
- Brandes' betweenness matches hand-computed values on a 6-node graph.
- k-core decomposition matches a hand-traced run.
- Critical-path equals longest path on a known DAG.

### Integration tests (loader + analyzer)

- Stand up a real `BeadsCrateAdapter` against a `TestBeadsWorkspace` (provided by the companion spec).
- Seed a known set of issues + dependencies via `beads_rust` API directly (no `br` CLI).
- Call each `GraphEngine` method, assert the typed report matches an expected fixture.
- Assert `report.raw` (the `serde_json::Value`) round-trips through `serde_json::to_string` → `serde_json::from_str` → typed report unchanged.

### MCP-passthrough snapshot tests

- For each of the 5 commands, a snapshot-test JSON file under `crates/spur-pm/tests/snapshots/` captures the full `raw` output for a canonical seeded workspace.
- CI fails on snapshot drift; updates are explicit reviewable commits.
- This is the audit trail for "what fields does brain actually consume from `raw`?" — if a brain prompt reads a field, it's in the snapshot.

### Benchmarks (informational)

- Criterion benchmarks on the analyzer functions for graph sizes 100, 1k, 10k.
- Not gating; informational only. Today's scale (406) means all five reports run in < 50ms.

---

## Sequencing & cutover

### Strict prerequisite

The companion `BeadsCrateAdapter` spec must complete its cutover (Phase 4 of that plan: `BeadsAdapter` deleted, `BeadsCrateAdapter` is the unconditional `IssueTracker`) **before** the graph engine cutover lands. Reason: the graph engine's loader requires direct `&SqliteStorage` access; that's only available unconditionally after the companion cutover.

The graph engine work is **developed in parallel** with the companion plan — using `TestBeadsWorkspace` fixtures and direct `beads_rust` API access — so it can land in a single PR shortly after the companion completes.

### Internal milestones (developer-facing only — not user-visible)

- **M1**: `subgraph` + `alerts` (simplest; no scoring formulas; concrete schemas).
- **M2**: + `triage` (introduces `ScoreConfig`; highest reconciler impact).
- **M3**: + `plan` (topo generations; builds on M2's actionable computation).
- **M4**: + full `insights` (HITS, betweenness, k-core, articulation; the algorithm-heavy piece).

Each milestone is a **draft-PR checkpoint** with green tests. The single user-visible cutover ships at M4.

### Cutover (single PR)

1. Add `petgraph` to workspace `Cargo.toml`.
2. Add `crates/spur-pm/src/graph_engine/` module, wired to `lib.rs`.
3. Modify `crates/spur-pm/src/bv.rs`:
   - Drop subprocess code (`run_bv_raw`, `run_robot`).
   - Replace internal field with `engine: GraphEngine`.
   - All 5 public methods become 1-line delegates to `GraphEngine`.
4. Modify `crates/spur-pm/src/service.rs::PmService::try_new`:
   - Remove `bv` binary probe.
   - Construct `BvAdapter::connect(beads_crate_adapter, graph_cfg)` unconditionally.
   - `bv: Option<BvAdapter>` field becomes `bv: Arc<BvAdapter>` (always present).
5. Update tests in `crates/spur-pm/tests/bv_triage.rs` to use `TestBeadsWorkspace` directly (no longer requires `bv` install).
6. Update the install scripts / docs to drop the `brew install dicklesworthstone/tap/bv` step.

### Rollback

Single git revert. The companion `BeadsCrateAdapter` is unaffected; it works the same with or without `bv`.

### Quiescence

No special quiescence protocol needed beyond what `BeadsCrateAdapter` already enforces. The graph engine holds no state beyond `GraphEngine` config; reads are stateless.

---

## Risks & mitigations

| # | Risk | Severity | Probability | Mitigation |
|---|---|---|---|---|
| 1 | Brain agents parse `raw` fields we don't generate | HIGH | MEDIUM | MCP-passthrough snapshot tests cover every field consumed by current brain prompts; audit current prompts before cutover; failures fail CI loudly. |
| 2 | Reconciler picks different next task post-cutover (scoring drift) | MEDIUM | MEDIUM | Per user direction: trust the brain feedback loop. Configurable weights in `[graph.score]` allow rapid retuning without redeployment. |
| 3 | `data_hash` instability → reconciler cache thrash | HIGH | LOW (with care) | Property test asserts hash invariance under same-state reload; deterministic input order; SHA256 over sorted inputs. |
| 4 | `beads_rust` 0.2.1 doesn't expose dep listing we need | MEDIUM | LOW | Pre-flight check before implementation; fallback to raw SQL via the underlying `rusqlite::Connection`; loader is the only place that knows the schema. |
| 5 | HITS/Brandes/k-core implementation bugs | LOW | LOW | Golden tests with hand-computed ground truth on standard textbook graphs. |
| 6 | `petgraph` `page_rank` API mismatch | LOW | LOW | Custom 30-LOC PageRank as fallback; well-known iterative algorithm. |
| 7 | Companion spec slips → graph engine cutover slips | LOW | LOW | Development happens in parallel; only the cutover-merge waits. |

---

## Open questions / deferred

- **`top_what_ifs`** — counterfactual "what if I closed issue X" deltas. v1 leaves the field as empty `Vec<>`. Add later if a brain consumer asks for it.
- **Velocity / staleness in `project_health`** — depends on `closed_at` being present in `beads_rust::Issue`. Verify and either populate or leave `None`.
- **`commands` field in `TriageReport`** — bv populates this with shell-hint strings. SPUR leaves it `serde_json::Value::Null`. Re-evaluate if a brain consumer needs it.
- **External configurability of `[graph.insights]` top-K** — start with K=10 hardcoded; promote to config when motivated.
- **Benchmarks at 100k** — informational only at current scale; revisit when SPUR users cross 10k issues.
- **Optional: ASCII-art subgraph format** — possible future addition; not required by any current call site.

---

## References

- Companion spec: `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md`
- Companion plan: `docs/superpowers/plans/2026-05-05-beads_rust-direct-crate-adapter.md`
- Current adapter: `crates/spur-pm/src/bv.rs`
- Wire types: `crates/spur-pm/src/graph.rs`
- Call sites:
  - `crates/spur-mcp/src/server.rs:{4373, 4392, 4411, 4429, 4453}`
  - `crates/spur-pm/src/service.rs:{210, 214}`
  - `crates/spur-core/src/orchestrator.rs:1621`
  - `crates/spur-tui/src/components/status_bar.rs:158`
- `petgraph` algorithms: https://docs.rs/petgraph/latest/petgraph/algo/
- Brandes' betweenness: Brandes, U. (2001). "A faster algorithm for betweenness centrality." *Journal of Mathematical Sociology* 25(2):163–177.
- HITS: Kleinberg, J. (1999). "Authoritative sources in a hyperlinked environment." *Journal of the ACM* 46(5):604–632.
- k-core: Matula, D. W., Beck, L. L. (1983). "Smallest-last ordering and clustering and graph coloring algorithms." *Journal of the ACM* 30(3):417–427.
- Codex feasibility review (delegation `d2fc25a5-e2d9-4993-975c-99bbf4e2e1a8`): scope-down go; clean-room from public algorithms; recommend new crate (we differ — embedded in `spur-pm` per user direction and to avoid dep-cycle vs. `BeadsCrateAdapter`).
