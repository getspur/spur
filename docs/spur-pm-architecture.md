# spur-pm Detailed Architecture

> Produced 2026-04-16. Covers current state, beads_rust integration evaluation, and recommended v2 architecture.

## 1. Current Architecture (v1)

### 1.1 Crate Overview

`spur-pm` is a ~2k-line support service crate providing project management adapters. It sits at the leaf of the dependency graph — consumed by `spur-core` (Orchestrator) and `spur-mcp` (MCP Server), with no reverse dependencies.

```mermaid
graph TB
    subgraph "Consumers"
        CORE["spur-core<br/><i>Orchestrator</i>"]
        MCP["spur-mcp<br/><i>MCP Server</i>"]
        CLI["spur-cli<br/><i>Binary</i>"]
    end

    subgraph "spur-pm"
        LIB["lib.rs<br/><i>pub use re-exports</i>"]
        ADAPTER["adapter.rs<br/><i>PmAdapter trait</i>"]
        TYPES["types.rs<br/><i>Issue, PrParams,<br/>IssueFilter, PmEvent,<br/>PmSource</i>"]
        GH["github.rs<br/><i>GitHubAdapter<br/>shells to gh CLI</i>"]
    end

    subgraph "External"
        GH_CLI["gh CLI<br/><i>GitHub CLI tool</i>"]
        GH_API["GitHub API"]
    end

    CORE -->|"use spur_pm::{PmAdapter, GitHubAdapter}"| LIB
    MCP -->|"DelegationRequest(__pm_*)"| CORE
    CLI -->|"direct adapter use"| LIB
    LIB --> ADAPTER
    LIB --> TYPES
    LIB --> GH
    GH -->|"tokio::process::Command"| GH_CLI
    GH_CLI -->|"REST API"| GH_API

    style ADAPTER fill:#e94560,stroke:#e94560,color:#fff
    style GH fill:#0f3460,stroke:#0f3460,color:#fff
    style TYPES fill:#533483,stroke:#533483,color:#fff
```

### 1.2 File Map

| File | Lines | Responsibility |
|---|---|---|
| `lib.rs` | 7 | Re-exports: `PmAdapter`, `GitHubAdapter`, `types::*` |
| `adapter.rs` | 25 | `PmAdapter` trait — 6 async methods |
| `types.rs` | 72 | `Issue`, `IssueSummary`, `IssueFilter`, `IssueUpdate`, `PrParams`, `PmEvent`, `PmSource` |
| `github.rs` | 400 | `GitHubAdapter` — shells to `gh` CLI, JSON deser, `From` impls |

### 1.3 PmAdapter Trait

```rust
#[async_trait]
pub trait PmAdapter: Send + Sync {
    async fn connect(&mut self) -> Result<()>;
    async fn get_issue(&self, id: &str) -> Result<Issue>;
    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<IssueSummary>>;
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> Result<()>;
    async fn create_pr(&self, params: PrParams) -> Result<String>;
    async fn poll(&self) -> Result<Vec<PmEvent>>;
}
```

### 1.4 Data Flow — MCP → PM Operation

```mermaid
sequenceDiagram
    participant Brain as Brain Agent
    participant MCP as spur-mcp Server
    participant Orch as Orchestrator
    participant PM as handle_pm_operation()
    participant GH as GitHubAdapter
    participant CLI as gh CLI

    Brain->>MCP: MCP tool call (get_issue)
    MCP->>MCP: Validate args, create UUID
    MCP->>Orch: DelegationRequest{agent: "__pm_get_issue"}
    Note over MCP,Orch: mpsc channel, oneshot response

    Orch->>PM: handle_pm_operation("__pm_get_issue", json)
    PM->>GH: GitHubAdapter::new(None).with_cwd(repo_root)
    PM->>GH: adapter.connect()
    GH->>CLI: gh auth status
    GH->>CLI: gh repo view --json nameWithOwner
    PM->>GH: adapter.get_issue(id)
    GH->>CLI: gh issue view {id} --repo {repo} --json ...
    CLI-->>GH: JSON stdout
    GH-->>PM: Issue
    PM-->>Orch: DelegationResult{summary: json}
    Orch-->>MCP: oneshot response
    MCP-->>Brain: MCP tool result
```

### 1.5 Type Model

```mermaid
classDiagram
    class PmSource {
        <<enum>>
        GitHub
        Linear
        Plane
    }

    class Issue {
        +String id
        +PmSource source
        +String title
        +String body
        +Vec~String~ labels
        +Option~String~ priority
        +Option~String~ assignee
        +String status
        +Vec~String~ linked_prs
        +String url
    }

    class IssueSummary {
        +String id
        +PmSource source
        +String title
        +Vec~String~ labels
        +String status
        +String url
    }

    class IssueFilter {
        +Vec~String~ labels
        +Option~String~ status
        +Option~String~ assignee
        +Option~DateTime~ since
    }

    class IssueUpdate {
        +Option~String~ status
        +Option~String~ comment
        +Vec~String~ add_labels
        +Vec~String~ remove_labels
    }

    class PrParams {
        +String title
        +String body
        +String head_branch
        +Option~String~ base_branch
        +Option~String~ repo
    }

    class PmEvent {
        <<enum>>
        IssueCreated(IssueSummary)
        IssueUpdated(IssueSummary)
    }

    Issue --> PmSource
    IssueSummary --> PmSource
    PmEvent --> IssueSummary
```

### 1.6 Event Bus Integration

PM operations emit events through `SpurEventBody` variants:

```mermaid
flowchart LR
    subgraph "spur-pm operations"
        GET["get_issue"]
        UPDATE["update_issue"]
        PR["create_pr"]
        POLL["poll"]
    end

    subgraph "SpurEventBody variants"
        IR["IssueReceived{source, id}"]
        IU["IssueUpdated{source, id, status}"]
        PC["PrCreated{url}"]
    end

    subgraph "Subscribers"
        DASH["Dashboard<br/>activity log"]
        LINEAGE["ExecutorLineage"]
        SINK["EventSink<br/>NDJSON"]
    end

    GET -->|"orchestrator emits"| IR
    UPDATE --> IU
    PR --> PC
    POLL --> IR
    POLL --> IU

    IR --> DASH
    IU --> DASH
    PC --> DASH
    IR --> LINEAGE
    IR --> SINK
```

---

## 2. beads_rust Integration Evaluation

### 2.1 What is beads_rust?

A local-first, non-invasive issue tracker storing tasks in SQLite with JSONL export for git collaboration. ~20k lines of Rust, CLI binary `br`.

| Aspect | Detail |
|---|---|
| Crate type | Binary only (`[[bin]] name = "br"`) |
| Edition | 2024 (requires Rust nightly 1.88+) |
| Storage | SQLite (via `fsqlite` pure-Rust fork) + JSONL |
| Build dep | Requires sibling `frankensqlite` checkout |
| License | MIT with OpenAI/Anthropic rider |
| Contributions | Not accepted |
| JSON API | `--json` flag on all commands |
| PR support | None (local issue tracker only) |

### 2.2 MCTS Strategy Evaluation

Five integration strategies evaluated with weighted scoring (Feasibility ×3, Pattern alignment ×2, Maintenance ×2, Performance ×1, Type safety ×1):

```mermaid
bar
    title Integration Strategy Scores (max 90)
    "A: Direct lib dep" : 30
    "B: CLI shelling" : 74
    "C: MCP bridge" : 42
    "D: JSONL file" : 49
    "E: Hybrid" : 46
```

| Strategy | Feasibility | Pattern | Maintenance | Perf | Safety | Total |
|---|---|---|---|---|---|---|
| A: Direct lib dep | 2 | 3 | 1 | 9 | 7 | **30** |
| B: CLI shelling | 9 | 10 | 8 | 5 | 6 | **74** ✅ |
| C: MCP bridge | 5 | 4 | 4 | 6 | 5 | **42** |
| D: JSONL file | 7 | 2 | 6 | 8 | 4 | **49** |
| E: Hybrid | 6 | 3 | 5 | 7 | 5 | **46** |

**Winner: CLI shelling** — identical pattern to GitHubAdapter→`gh`, zero build coupling, `br --json` is the stable public API.

### 2.3 PmAdapter Method Mapping

| PmAdapter method | br command | Notes |
|---|---|---|
| `connect()` | `br version` | Verify binary exists, `.beads/` initialized |
| `get_issue(id)` | `br show {id} --json` | Full issue with 30+ fields |
| `list_issues(filter)` | `br list --json --status {s} --label {l}` | Supports priority range filters |
| `update_issue(id, update)` | `br update` + `br comments add` + `br label add/remove` | Multiple commands for compound updates |
| `create_pr(params)` | ❌ Not supported | Returns `Err("beads_rust does not support PR creation")` |
| `poll()` | `br list --json --status open` | Client-side `since` filtering |

### 2.4 Integration Verdict

```
✅ RECOMMENDED — CLI shelling strategy
⚠️  PARTIAL — no PR support (fundamental, acceptable)
⚠️  REQUIRES — br binary installed on host
✅ ZERO build coupling (no nightly, no frankensqlite)
✅ IDENTICAL pattern to existing GitHubAdapter
```

---

## 3. Recommended Architecture (v2)

### 3.1 Component Diagram

```mermaid
graph TB
    subgraph "Consumers"
        CORE["spur-core<br/><i>Orchestrator</i>"]
        MCP["spur-mcp<br/><i>MCP Server</i>"]
    end

    subgraph "spur-pm v2"
        SVC["PmService<br/><i>owns registry,<br/>caching, events</i>"]
        REG["AdapterRegistry<br/><i>config-driven factory</i>"]
        TRAIT["PmAdapter trait"]

        subgraph "Adapters"
            GH["GitHubAdapter<br/><i>gh CLI</i>"]
            BR["BeadsAdapter<br/><i>br CLI</i>"]
            LIN["LinearAdapter<br/><i>Phase 3</i>"]
            PLN["PlaneAdapter<br/><i>Phase 3</i>"]
        end

        TYPES["types.rs<br/><i>Issue, PmSource::Beads,<br/>PrParams, PmEvent</i>"]
    end

    subgraph "External CLIs"
        GH_CLI["gh"]
        BR_CLI["br"]
    end

    CORE --> SVC
    MCP --> SVC
    SVC --> REG
    REG --> GH
    REG --> BR
    REG --> LIN
    REG --> PLN
    GH -.->|"impl"| TRAIT
    BR -.->|"impl"| TRAIT
    LIN -.->|"impl"| TRAIT
    PLN -.->|"impl"| TRAIT
    GH --> GH_CLI
    BR --> BR_CLI

    style SVC fill:#e94560,stroke:#e94560,color:#fff
    style REG fill:#0f3460,stroke:#0f3460,color:#fff
    style BR fill:#533483,stroke:#533483,color:#fff
    style TRAIT fill:#16213e,stroke:#16213e,color:#fff
```

### 3.2 New File Layout

```
crates/spur-pm/src/
├── lib.rs              # Re-exports
├── adapter.rs          # PmAdapter trait (unchanged)
├── types.rs            # +PmSource::Beads
├── registry.rs         # NEW: AdapterRegistry (config → adapter factory)
├── service.rs          # NEW: PmService (caching, event emission)
├── github.rs           # GitHubAdapter (existing)
└── beads.rs            # NEW: BeadsAdapter (shells to br CLI)
```

### 3.3 BeadsAdapter Data Flow

```mermaid
sequenceDiagram
    participant Brain as Brain Agent
    participant SVC as PmService
    participant REG as AdapterRegistry
    participant BA as BeadsAdapter
    participant BR as br CLI

    Brain->>SVC: get_issue("beads", "bd-abc123")
    SVC->>REG: get_adapter(PmSource::Beads)
    REG-->>SVC: &BeadsAdapter (cached singleton)
    SVC->>BA: get_issue("bd-abc123")
    BA->>BR: br show bd-abc123 --json
    BR-->>BA: {"id":"bd-abc123","title":"...","status":"open",...}
    BA->>BA: BrIssueView → Issue (type mapping)
    BA-->>SVC: Issue
    SVC->>SVC: emit(IssueReceived{source: Beads, id})
    SVC-->>Brain: Issue JSON
```

### 3.4 Type Mapping: br → spur-pm

```mermaid
flowchart LR
    subgraph "br Issue (30+ fields)"
        B_ID["id: bd-abc123"]
        B_TITLE["title"]
        B_DESC["description"]
        B_STATUS["status: Open|InProgress|Blocked|..."]
        B_PRI["priority: 0-4"]
        B_TYPE["issue_type: task|bug|feature|..."]
        B_ASSIGN["assignee"]
        B_LABELS["labels: Vec"]
        B_DEPS["dependencies: Vec"]
        B_COMMENTS["comments: Vec"]
    end

    subgraph "spur-pm Issue (10 fields)"
        S_ID["id"]
        S_TITLE["title"]
        S_BODY["body"]
        S_STATUS["status: string"]
        S_PRI["priority: Option string"]
        S_ASSIGN["assignee"]
        S_LABELS["labels"]
        S_PRS["linked_prs: empty"]
        S_URL["url: file://..."]
        S_SRC["source: Beads"]
    end

    B_ID --> S_ID
    B_TITLE --> S_TITLE
    B_DESC --> S_BODY
    B_STATUS -->|"to_lowercase()"| S_STATUS
    B_PRI -->|"P{n}"| S_PRI
    B_ASSIGN --> S_ASSIGN
    B_LABELS --> S_LABELS
```

### 3.5 Architectural Deficiencies Addressed

```mermaid
flowchart TB
    subgraph "v1 Problems"
        P1["Adapter created fresh<br/>per operation"]
        P2["handle_pm_operation()<br/>free function hack"]
        P3["__pm_* pseudo-agent<br/>routing"]
        P4["No caching"]
        P5["Hardcoded GitHub"]
    end

    subgraph "v2 Solutions"
        S1["AdapterRegistry<br/>singleton lifecycle"]
        S2["PmService<br/>owns adapter + events"]
        S3["Direct method dispatch<br/>no pseudo-agents"]
        S4["LRU cache in PmService"]
        S5["Config-driven<br/>multi-adapter"]
    end

    P1 -->|"fixed by"| S1
    P2 -->|"replaced by"| S2
    P3 -->|"eliminated by"| S3
    P4 -->|"added"| S4
    P5 -->|"generalized"| S5

    style P1 fill:#8b0000,color:#fff
    style P2 fill:#8b0000,color:#fff
    style P3 fill:#8b0000,color:#fff
    style P4 fill:#8b0000,color:#fff
    style P5 fill:#8b0000,color:#fff
    style S1 fill:#006400,color:#fff
    style S2 fill:#006400,color:#fff
    style S3 fill:#006400,color:#fff
    style S4 fill:#006400,color:#fff
    style S5 fill:#006400,color:#fff
```

### 3.6 Config Integration

```toml
# .spur/config.toml

[pm.github]
repo = "owner/repo"

[pm.beads]
workspace = "."          # path to .beads/ directory
auto_sync = false        # run `br sync --flush-only` after mutations
```

### 3.7 PmSource Enum Extension

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmSource {
    GitHub,
    Linear,
    Plane,
    Beads,  // NEW
}
```

---

## 4. Risk Matrix

| Risk | Severity | Mitigation |
|---|---|---|
| `br` binary not installed | Medium | `connect()` returns clear error with install URL |
| `br --json` schema changes | Medium | Pin to known version, integration tests |
| No PR support in beads | Low | Brain agent handles error, uses GitHub for PRs |
| `br` process spawn latency | Low | Local SQLite, ~5ms per invocation |
| `.beads/` not initialized | Low | `connect()` checks, returns actionable error |
| Poll floods on first call | Medium | Same as GitHubAdapter — return all as IssueCreated |
| Nightly toolchain infection | N/A | CLI shelling avoids entirely |

---

## 5. Implementation Priority

1. **BeadsAdapter** — `beads.rs` implementing PmAdapter via `br` CLI shelling (~200 lines)
2. **PmSource::Beads** — Add variant to enum, update serde (~5 lines)
3. **AdapterRegistry** — Config-driven factory replacing hardcoded GitHubAdapter (~100 lines)
4. **PmService** — Replace `handle_pm_operation()` free function (~150 lines)
5. **MCP routing cleanup** — Remove `__pm_*` pseudo-agent hack, route through PmService directly
