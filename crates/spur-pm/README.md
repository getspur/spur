# spur-pm

Project Management (PM) service adapters for Spur. This crate abstracts the underlying issue tracking, planning, and task management systems used by Spur's brains and workers.

## Architectural Directive: Beads as the Source of Truth

The core architectural principle of `spur-pm` is that **Beads (`beads_rust`) is the definitive local Source of Truth (SoT)** for all project management, task dependencies, and graph analysis within Spur.

### The `beads_crate` Module (`src/beads_crate/`)
This module provides direct, in-memory SQLite linkage to the `beads_rust` crate. 
- All advanced features (DAG dependency graph, execution planning, graph triage, cycle detection) operate exclusively on the Beads data model.
- Brains, review gates, and subagents coordinate their state, reviews, and signals using the Beads PM primitives.
- Locally, the `.beads/` directory in a workspace is the definitive database.

### External PM Services (GitHub, Linear, Plane, etc.)
While `spur-pm` contains interfaces (like `IssueTracker`) and implementations for external PMs (e.g., `GitHubAdapter` in `src/github.rs`), **they are not intended to be parallel sources of truth**.

Instead, the architecture is designed so that external PM services **ingest and sync through Beads**:
1. **Ingestion**: Issues from external systems (GitHub, Linear, Plane) will be fetched and mirrored into the local Beads SQLite database.
2. **Local Operations**: Spur (the orchestrator, workers, graph engine) reads and writes *exclusively* against the local Beads database to eliminate network latency and enable complex relational queries.
3. **Synchronization**: Changes made locally by the agent (e.g., status updates, comments, new task breakdowns) are synced back out to the external PM service. 

This offline-first approach ensures that Spur always has a fast, strictly-ordered, and graph-relational database for its autonomous planning algorithms (`graph_engine`), while remaining compatible with whatever external PM systems human teams are already using.

## Module Overview
- `src/beads_crate/` - Direct SQLite adapter to `beads_rust` (The Source of Truth).
- `src/graph_engine/` - Autonomy, DAG planning, and insights algorithms (runs on Beads data).
- `src/github.rs` - GitHub CLI adapter (for external syncing and PR creation).
- `src/service.rs` - Multiplexing service (`PmService`) that routes standard operations.
- `src/adapter.rs` - Core traits (`IssueTracker`, `PrService`) for PM operations.
- `src/advanced.rs` - `BeadsAdvanced` trait for beads-specific lifecycle management.
- `src/types.rs` - Standardized internal PM representations (`Issue`, `IssueSummary`, `PmEvent`, `PmSource`).
