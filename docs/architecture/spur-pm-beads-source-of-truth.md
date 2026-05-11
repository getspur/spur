# spur-pm Architecture: Beads as Source of Truth

Status: Architectural direction

`spur-pm` owns Spur's project-management substrate. Its direction is local-first: the `beads_rust` crate, through the repository's `.beads/beads.db` SQLite store, is the definitive source of truth for issues, labels, dependencies, comments, status, and graph-derived planning state.

External PM services such as GitHub, Linear, and Plane are integrations around that local truth. They may provide upstream data to ingest and downstream targets to sync, but they must not become parallel authorities for Spur's orchestration decisions.

## Core Principle

Beads is the operational source of truth because Spur's core workflow needs local, durable, dependency-aware state:

- Worker and brain coordination depends on issue status, labels, comments, and audit breadcrumbs being available locally.
- Graph algorithms need a complete local issue/dependency graph.
- Multi-agent workflows need stable IDs and deterministic reads that are not gated on network availability.
- External services vary in issue model fidelity. GitHub Issues, for example, has no native dependency graph matching Beads' blocking relationship model.

The PM abstraction can expose multiple `PmSource` values (`Beads`, `GitHub`, `Linear`, `Plane`) in `crates/spur-pm/src/types.rs`, but Spur's authoritative local domain model is the Beads-shaped one: priority, issue type, blocking dependencies, due dates, comments, labels, and status all map naturally to `beads_rust`.

## Current Implementation

### Public PM Contract

`crates/spur-pm/src/adapter.rs` defines two public integration contracts:

- `IssueTracker`: read, list, create, update, add dependency, and poll issues.
- `PrService`: create pull requests.

This split is intentional. Issue state and dependency state belong to the local PM source of truth. Pull request creation is an external side effect and can remain a GitHub service even when issues are Beads-backed.

`crates/spur-pm/src/types.rs` defines the shared wire model. The structs are deliberately richest-source-first:

- `Issue` carries Beads-native fields such as `priority`, `issue_type`, `blocked_by`, `due_at`, and timestamps.
- `IssueCreate` includes parent and blocking dependencies.
- `IssueUpdate` supports status, comments, labels, priority, assignee, and body changes.
- `PmSource` already names GitHub, Linear, and Plane, but those enum values should be interpreted as provenance/integration identifiers, not as permission to create competing PM stores.

### PmService Routing Boundary

`crates/spur-pm/src/service.rs` is the runtime boundary for PM operations. Its current backend enum has two modes:

```rust
enum PmBackendInner {
    Beads {
        beads: Box<BeadsCrateAdapter>,
        github: Option<GitHubAdapter>,
    },
    GitHub {
        adapter: GitHubAdapter,
    },
}
```

When `.beads/` exists and Beads is enabled, `PmService::try_new_with_actor` opens `BeadsCrateAdapter` first and returns the `Beads` backend. In this mode, issue reads/writes go to Beads and GitHub is only retained as an optional PR service. This is the desired architectural shape.

The `GitHub` backend still exists as fallback behavior when Beads is absent and GitHub is enabled. That fallback is transitional compatibility. It should not be extended into a peer source of truth for dependency-aware Spur operations.

### BeadsCrateAdapter

`crates/spur-pm/src/beads_crate/adapter.rs` is the concrete Beads substrate. It links to `beads_rust` directly rather than shelling out to `br`.

Important properties:

- The adapter stores paths and config, not long-lived SQLite handles.
- Each read opens a fresh `SqliteStorage` inside `spawn_blocking`, runs the closure, and drops the storage before returning.
- Writes acquire `.beads/.write.lock` with backoff, open `SqliteStorage`, execute the mutation, and checkpoint WAL best-effort.
- `read_snapshot` and `validate_and_commit` provide a coarse snapshot/CAS flow for read-compute-write cases.
- `auto_flush` runs under the same write lock and calls `beads_rust::sync::auto_flush`.

This makes `.beads/beads.db` the durable local state and centralizes concurrency discipline in one adapter.

### Beads IssueTracker Implementation

`crates/spur-pm/src/beads_crate/issue_tracker.rs` maps the shared PM contract into `beads_rust`:

- `br_to_pm_issue` and `br_to_pm_summary` convert Beads issues into `Issue` and `IssueSummary`.
- `get_issue` loads the issue row, labels, and dependencies, then populates `blocked_by` from blocking dependency types.
- `list_issues` converts `IssueFilter` into `beads_rust::storage::sqlite::ListFilters`.
- `create_issue` generates a Beads ID, writes the issue, applies labels, records parent-child dependencies, and records blocking dependencies.
- `update_issue` applies field updates, label mutations, and comments.
- `add_dependency` writes a Beads `blocks` dependency.
- `poll` uses the persisted boundary-safe cursor in `crates/spur-pm/src/poll_cursor.rs`.

This implementation is the source-of-truth path for Spur PM mutations.

### Beads-Only Advanced Surface

`crates/spur-pm/src/advanced.rs` defines `BeadsAdvanced`, and `crates/spur-pm/src/beads_crate/beads_advanced.rs` implements it for `BeadsCrateAdapter`.

The methods are intentionally Beads-only:

- `list_ready`
- `list_comments`
- `add_comment`
- `remove_dependency`
- `dep_cycles`

`PmService::advanced()` returns this surface only when the active backend is Beads. Callers should treat `None` as "the local PM source of truth is unavailable", not as a cue to emulate advanced graph semantics in a remote issue tracker.

### Graph Engine

`crates/spur-pm/src/bv.rs` now wraps the native `GraphEngine` instead of an external `bv` process. `BvAdapter::from_beads` receives an `Arc<BeadsCrateAdapter>`, preserving the historical API surface while making graph analysis local and Beads-backed.

`crates/spur-pm/src/graph_engine/snapshot.rs` builds `GraphSnapshot` from Beads:

- It lists issues from `beads_rust`.
- It loads labels and dependencies.
- It represents edges from blocker to blocked issue.
- It treats `blocks`, `parent-child`, `conditional-blocks`, and `waits-for` as blocking dependency kinds.
- It computes a deterministic `data_hash` from content hash, labels, and blocking dependencies.

`crates/spur-pm/src/graph_engine/mod.rs` then runs pure analyzers over that snapshot:

- `triage`
- `plan`
- `insights`
- `alerts`
- `subgraph`
- `graph_by_label`

This is why external PMs must be ingested into Beads before Spur uses them for planning. The graph engine does not operate over GitHub/Linear/Plane APIs; it operates over the local Beads graph.

## Target Pattern: Ingest and Sync

External PM integrations should follow a two-phase pattern.

### Ingest

Ingest pulls remote PM data into Beads and records provenance on the local issue.

Expected responsibilities:

- Create or update local Beads issues using `BeadsCrateAdapter`.
- Preserve remote identity in Beads fields such as `source_system`, `source_repo`, and `external_ref` when available through `beads_rust`.
- Convert remote labels, status, assignees, priorities, parent links, and dependencies into the Beads domain model.
- Keep remote bodies/comments as data on Beads issues when Spur needs them for local work.
- Deduplicate by remote provenance, not by title.

Once ingested, local Beads IDs are what Spur uses for orchestration, graph analysis, worker assignment, signals, and audit trails.

### Local Operation

Spur operates only against Beads for PM state:

- Brain-worker task state is read from and written to Beads.
- Dependency-aware planning uses `GraphEngine` over `GraphSnapshot`.
- Ready lists, cycles, comments, and dependency edits use `BeadsAdvanced`.
- Polling and cursors track local Beads updates.

Network failures in GitHub, Linear, or Plane must not block local graph reasoning once data has been ingested.

### Sync

Sync pushes selected Beads changes back to the external service that owns the user-facing remote artifact.

Expected responsibilities:

- Translate Beads status, labels, comments, and assignment changes back into the remote model.
- Use remote provenance fields to select the destination object.
- Treat sync as an eventually consistent side effect.
- Detect and surface conflicts instead of letting the remote overwrite local Beads state silently.
- Preserve Beads as the conflict-resolution authority unless the user explicitly chooses to re-ingest remote changes.

PR creation is a special case of external side effect. The current `PmService::create_pr` path already follows that shape: when Beads is active, issues remain local while `GitHubAdapter` can still create a PR.

## Recommended Module Direction

The current crate is close to the desired shape, but some names and fallback paths still reflect the older "multiple backends" model.

Recommended direction:

- Keep `BeadsCrateAdapter` as the only implementation allowed to satisfy source-of-truth issue operations in normal Spur repositories.
- Keep `GitHubAdapter` as PR service and, if needed, as a remote import/export helper.
- Add explicit ingestion/sync modules for remote PMs instead of extending `PmBackendInner` with more peer issue backends.
- Use `PmSource` as provenance and display metadata, not backend authority.
- Prefer APIs named around movement of data: `ingest_from_github`, `sync_to_github`, `ingest_from_linear`, `sync_to_linear`.
- Gate graph and advanced features on Beads availability, as `PmService::analyzer()` and `PmService::advanced()` already do.

Conceptually:

```mermaid
graph TD
    %% Define styles for clarity
    classDef external fill:#f9f2f4,stroke:#333,stroke-width:1px
    classDef local fill:#e1f5fe,stroke:#333,stroke-width:1px,stroke-dasharray: 5 5
    classDef adapter fill:#e8eaf6,stroke:#0288d1,stroke-width:2px
    classDef engine fill:#fff3e0,stroke:#f57c00,stroke-width:2px

    subgraph External["External PM Services"]
        GH["GitHub Issues"]
        LN["Linear"]
        PL["Plane"]
    end
    class GH,LN,PL external

    subgraph SpurPM["spur-pm Architecture"]
        Ingest["Ingest Module<br/>(Reads remote, translates to Beads)"]
        Sync["Sync Module<br/>(Pushes local state changes)"]
        
        subgraph LocalTruth["Local Source of Truth"]
            DB[(".beads/beads.db<br/>(SQLite)")]
            Adapter["BeadsCrateAdapter<br/>(Concurrency, CRUD, Polling)"]
        end
        class LocalTruth local
        class Adapter adapter
        
        Engine["GraphEngine<br/>(Insights, Planning, Triage)"]
        class Engine engine
    end

    subgraph Core["Spur Core"]
        Brain["Brain Agents<br/>(Orchestration)"]
        Workers["Worker Agents<br/>(Execution)"]
        TUI["Spur TUI"]
    end

    %% Ingest Flow
    GH -.->|API Fetch| Ingest
    LN -.->|API Fetch| Ingest
    PL -.->|API Fetch| Ingest
    Ingest -->|Translate to IssueCreate<br/>(Preserve provenance)| Adapter

    %% Core Operations
    Adapter <==>|Read/Write<br/>Under File Lock| DB
    Adapter ==>|Load GraphSnapshot| Engine
    Engine -->|Dependency-aware planning| Brain
    
    Brain <==>|Issue Tracker API| Adapter
    Workers <==>|Signals, Updates| Adapter
    TUI <==>|Render state| Adapter

    %% Sync Flow
    Adapter -.->|Poll/Events| Sync
    Sync -.->|Translate & Push side-effects| GH
    Sync -.->|Translate & Push side-effects| LN
    Sync -.->|Translate & Push side-effects| PL
```

## Invariants

Future `spur-pm` changes should preserve these invariants:

1. Beads is the only PM store used for dependency-aware orchestration.
2. External PM systems are provenance, ingestion sources, and sync targets.
3. The graph engine reads only Beads state through `BeadsCrateAdapter`.
4. Beads-only capabilities must remain Beads-gated through `PmService::advanced()`.
5. Remote sync failures must not mutate local truth backwards without explicit conflict handling.
6. PR creation remains separate from issue authority.
7. New remote PM support should not add a peer `IssueTracker` backend unless the repository has no `.beads/` and is running in a documented degraded mode.

## Open Implementation Gaps

The current source already implements the Beads-first runtime path, direct `beads_rust` adapter, Beads-only advanced surface, and native graph engine. Remaining gaps for the formal ingest/sync architecture are:

- No explicit remote ingest/sync module exists yet.
- `GitHubAdapter` still implements `IssueTracker` and can be selected as a fallback issue backend.
- `create_issue` initializes Beads provenance fields such as `external_ref`, `source_system`, and `source_repo` to `None`; ingest code should populate them.
- Conflict handling for remote-vs-local updates is not yet modeled as a first-class sync concern.
- Linear and Plane are represented in `PmSource` but do not yet have concrete ingest/sync adapters.

These gaps do not change the architectural target. They identify where future implementation should converge.

## Philosophical Stance: The Local Execution Engine vs. The Human Projection

When evaluating the ingest/sync architecture from first principles, we must clearly define the distinct roles of the two systems:
1. **The Local PM (Beads):** This is a high-speed, 0-latency execution engine and data structure optimized for autonomous agents and graph algorithms.
2. **The External PM (GitHub/Linear):** This is an eventually-consistent reporting dashboard and collaboration surface optimized for human cadence and cross-team communication.

Because the remote PM follows human cadences, we intentionally accept certain architectural trade-offs to protect the integrity and speed of the local agent loop:

- **Acceptable Trade-off: Lossy Translation.** External PMs do not need a perfect programmatic representation of the Spur dependency graph. Injecting `Blocks: #123` into a GitHub Markdown body is acceptable because the human only needs visual context. The agent relies entirely on the strict, local Beads DAG.
- **Acceptable Trade-off: State Simplification.** Spur primarily requires binary state understanding (`Actionable` vs. `Done`). We do not need to perfectly mirror complex external workflow states (e.g., "Awaiting QA", "In Triage") locally, so long as the terminal states map correctly.

However, to prevent autonomous agents from hallucinating or executing expensive computational work on stale requirements, we cannot rely solely on manual or slow-periodic syncing. We must enforce **Lazy Validation / Eager Push**:
- Before a worker begins execution on a Beads issue, the system must perform a lightweight check against the remote PM (or rely on a webhook listener) to ensure the local requirements haven't been altered by a human.
- After a worker completes a task, state changes are eagerly pushed back to the remote to close the human feedback loop.

## First Principles Analysis: Flaws & Mitigations

When evaluating this architecture from first principles—reducing the system to its fundamental realities—several structural flaws and risks emerge in the "Beads as Local SoT vs. External PM" dynamic. 

### 1. The "Split-Brain" Conflict (Authority vs. Reality)
- **The Flaw:** We declare Beads as the local Source of Truth for *Spur*, but the external PM (e.g., Linear, GitHub) is the actual Source of Truth for the *human organization*. If a human edits a GitHub issue description while Spur concurrently mutates the local Beads representation, a naive Sync/Ingest loop creates a race condition.
- **The Mitigation:** We cannot use "last-write-wins". The Sync module must implement **Three-Way Merging** or maintain a `last_synced_remote_version` watermark. If the remote version diverges from the last known watermark, Spur must pause sync, surface a `Conflict` to the agent/user, and refuse to overwrite human data.

### 2. Lossy Translation & The Dependency Impedance Mismatch
- **The Flaw:** Beads relies on a strict DAG (Directed Acyclic Graph) for blocking dependencies. Linear supports this natively, but **GitHub does not**. To sync a Beads dependency to GitHub, we must serialize it into the Markdown body (e.g., `Depends on #123`). If a human edits that Markdown, parsing it back into a strict DAG during Ingestion is highly fragile.
- **The Mitigation:** We must accept that syncing to lower-fidelity PMs (like GitHub) is a "lossy projection." The architecture must separate **Core State** (Beads) from **Projected State** (GitHub). Ingesting dependencies from Markdown should be treated as *hints* rather than absolute truths, requiring Brain verification before breaking local Beads edges.

### 3. Latency, Polling, and Rate Limits
- **The Flaw:** Currently, external PM interaction relies on CLI wrapping (`gh issue list`) or API polling. Polling is slow, consumes rate limits, and guarantees that Spur operates on stale data for up to `N` minutes between polls. An autonomous agent acting on a stale graph will hallucinate or make redundant plans.
- **The Mitigation:** The Ingest architecture must evolve from a "Pull/Poll" model to a **"Push/Webhook"** model. A local daemon or MCP server must listen for real-time webhooks from GitHub/Linear to invalidate local Beads state instantly.

### 4. Lifecycle and State Machine Rigidity
- **The Flaw:** Beads has a fixed set of statuses (`open`, `in_progress`, `closed`). Modern PMs (Linear, Plane) use highly customizable workflow states ("Triage", "In QA", "Awaiting Deployment").
- **The Mitigation:** The Ingest module must include a **State Mapping Configuration** (`config.toml`) that binds remote custom states to the core Beads primitives, while retaining the raw remote state in a metadata field so it isn't lost during the round-trip Sync.

---

## Roadmap: Onboarding External PM Sources

To safely transition from the current monolithic `PmBackendInner` to a scalable Ingest/Sync architecture, we must sequence the work carefully to avoid breaking existing workflows.

### Phase 1: The Integration Substrate (Foundation)
1. **Identity Registry:** Introduce an `external_links` or robust metadata schema in Beads to durably map `beads_id` <-> `(source_system, remote_id, remote_version_hash)`.
2. **Define the `ExternalPmSync` Trait:** Create a formal interface separated from `IssueTracker`. 
   - `fetch_changes_since(timestamp)`
   - `push_mutations(diff)`
3. **Conflict Detection API:** Implement the watermark-checking logic to prevent silent overwrites.

### Phase 2: GitHub as the Reference Implementation
1. **Deprecate `GitHubAdapter` as an `IssueTracker`:** Move GitHub out of the `PmBackendInner` routing path.
2. **Implement GitHub Sync:** Use the new `ExternalPmSync` trait to project Beads issues to GitHub.
3. **Markdown Dependency Injection:** Implement the logic to inject/parse `Blocks: #XYZ` into GitHub issue bodies, clearly defining it as a lossy projection.

### Phase 3: Linear Integration (High Fidelity)
Linear is the ideal target for this architecture because its native model closely mirrors Beads.
1. **Linear GraphQL Client:** Build a native rust client for Linear.
2. **Native Graph Sync:** Map Beads `blocks` edges directly to Linear `blocks`/`is blocked by` issue relations.
3. **State Mapping:** Implement the configuration layer to map Linear's custom workflow states to Beads' `Status` enum.

### Phase 4: Plane & Event-Driven Realtime Sync
1. **Plane REST API Adapter:** Implement the Sync trait for Plane.
2. **Webhook Listener (Daemonization):** Introduce a lightweight local server (or leverage the MCP server) to receive webhooks from GitHub/Linear/Plane, instantly triggering targeted Ingestion into the `.beads/` database to achieve zero-latency graph updates.

## Related Source

- `crates/spur-pm/src/service.rs`: PM routing, Beads-first construction, GitHub PR sidecar, graph/advanced accessors.
- `crates/spur-pm/src/adapter.rs`: public `IssueTracker` and `PrService` contracts.
- `crates/spur-pm/src/types.rs`: shared issue, update, filter, event, and source model.
- `crates/spur-pm/src/beads_crate/adapter.rs`: direct `beads_rust` adapter and local concurrency discipline.
- `crates/spur-pm/src/beads_crate/issue_tracker.rs`: Beads-backed issue CRUD, dependencies, and polling.
- `crates/spur-pm/src/advanced.rs`: Beads-only extension contract.
- `crates/spur-pm/src/beads_crate/beads_advanced.rs`: ready queries, comments, dependency removal, cycle detection.
- `crates/spur-pm/src/bv.rs`: compatibility wrapper over native graph analysis.
- `crates/spur-pm/src/graph_engine/`: local Beads graph snapshot and analyzers.
