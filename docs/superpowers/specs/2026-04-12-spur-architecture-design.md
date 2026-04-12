# SPUR Architecture Design Spec

**Date:** 2026-04-12
**Status:** Approved
**Approach:** Value-first resequencing (Approach B) with brain-agent-on-top architecture

---

## 1. Core Concept

SPUR is a Rust-based multi-agent orchestrator. It is the nervous system, not the brain.

- **Agents think.** SPUR plumbs.
- A brain agent (Kiro CLI / Claude Code) runs ReAct reasoning on top.
- The brain delegates subtasks to worker agents through SPUR's infrastructure.
- SPUR manages process lifecycle, worktree isolation, PM integration, cost tracking, and user-facing interfaces.

One-liner: *"Issue in, PR out — across every agent."*

---

## 2. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  SPUR (Rust Orchestrator)                                   │
│                                                             │
│  ┌─── Proactive Driver ──────────────────────────────────┐  │
│  │ Receives work (PM tool event / user CLI / TUI)        │  │
│  │ Spawns brain agent via ACP                            │  │
│  │ Feeds issue context as prompt                         │  │
│  │ Handles delegation callbacks from brain               │  │
│  │ Spawns workers in isolated worktrees                  │  │
│  │ Collects results, creates PRs, updates PM tools       │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌─── ACP Client ───┐  ┌─── MCP Server (callback) ──────┐  │
│  │ Spawn agents      │  │ delegate_to_worker             │  │
│  │ Send prompts      │  │ delegate_parallel              │  │
│  │ Receive responses │  │ list_available_workers          │  │
│  │ Manage sessions   │  │ get_issue_context              │  │
│  └──────────────────┘  │ update_issue                    │  │
│                        │ create_pr                        │  │
│                        │ report_progress                  │  │
│                        │ get_session_cost                 │  │
│                        └────────────────────────────────┘  │
│                                                             │
│  ┌─── Support ───────────────────────────────────────────┐  │
│  │ Worktree Manager │ Cost Tracker │ PM Adapters │ TUI   │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
         │ ACP (spawn + prompt)              ▲ MCP (callbacks)
         ▼                                   │
┌─────────────────────┐          ┌──────────────────────┐
│ Brain Agent         │──────────│ Agent's MCP Client   │
│ (Kiro / Claude Code)│          │ connects to SPUR's   │
│                     │          │ MCP server at init    │
│ ReAct reasoning     │          └──────────────────────┘
│ Own MCP tools:      │
│   filesystem, bash, │
│   git               │
│ Calls SPUR MCP      │
│   to delegate       │
└────────┬────────────┘
         │ SPUR intercepts delegation,
         │ spawns workers via ACP
    ┌────┼────┐
    ▼    ▼    ▼
 Worker Worker Worker
 (any agent, own MCP tools, isolated worktree)
```

### Data Flow

```
1. GitHub webhook/poll → SPUR: "Issue #42 created"
2. SPUR spawns brain (Kiro) via ACP
   └─ ACP initialize: passes SPUR MCP server endpoint
   └─ Kiro's MCP client auto-connects to SPUR's MCP tools
3. SPUR sends prompt: "Fix issue #42: JWT tokens don't expire..."
4. Brain ReAct loop:
   ├─ THINK → ACT (own MCP tools) → OBSERVE
   ├─ ACT: delegate_to_worker("codex", "write JWT expiry tests...")
   └─ SPUR spawns Codex in worktree, returns result
5. Brain synthesizes, signals completion
6. SPUR: merge worktree → create PR → update issue → log costs
```

### Decision Boundaries

| Decision | Who | Why |
|----------|-----|-----|
| Which agent is the brain | SPUR (user config) | Infrastructure concern |
| When to trigger a workflow | SPUR (PM events / CLI) | Infrastructure concern |
| Brain failover on rate limit | SPUR | Infrastructure concern |
| What to delegate, to whom | Brain agent | Reasoning concern |
| How to fix the bug | Brain / Workers | Coding concern |
| How to decompose the task | Brain agent | Reasoning concern |

---

## 3. Crate Structure

```
spur/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── spur-core/              # Orchestrator engine, workflow driver
│   ├── spur-acp/               # ACP client, trait AgentTransport, agent registry
│   ├── spur-mcp/               # MCP callback server (delegation tools for agents)
│   ├── spur-pm/                # PM adapters (GitHub, Linear, Plane)
│   ├── spur-worktree/          # Git worktree lifecycle management
│   ├── spur-cost/              # SQLite cost tracking
│   ├── spur-tui/               # Ratatui dashboard
│   └── spur-cli/               # Clap CLI, binary entry point
├── agent-prompts/              # System prompts for brain agents
│   ├── brain-kiro.md
│   ├── brain-claude.md
│   └── worker-default.md
└── workflows/                  # Example TOML workflow definitions
    └── bug-fix.toml
```

Dependency flow (no cycles):

```
spur-cli → spur-tui → spur-core
                       spur-core → spur-acp
                       spur-core → spur-mcp
                       spur-core → spur-pm
                       spur-core → spur-worktree
                       spur-core → spur-cost
```

---

## 4. `spur-acp` — ACP Client & Agent Transport

### `trait AgentTransport`

```rust
#[async_trait]
pub trait AgentTransport: Send + Sync {
    async fn initialize(&mut self, mcp_endpoint: Option<McpEndpoint>) -> Result<AgentCapabilities>;
    async fn create_session(&mut self) -> Result<SessionId>;
    async fn prompt(
        &mut self,
        session: SessionId,
        prompt: Vec<PromptBlock>,
    ) -> Result<Pin<Box<dyn Stream<Item = SessionEvent>>>>;
    async fn cancel(&mut self, session: SessionId) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;
    fn health(&self) -> AgentHealth;
}
```

### Transport Implementations

| Transport | When | How |
|-----------|------|-----|
| `AcpTransport` | Agent supports ACP (Kiro, Claude Code experimental) | JSON-RPC 2.0 over stdio, full streaming, session persistence, MCP passthrough | Phase 1 |
| `StdioTransport` | Agent has stdin/stdout but no ACP | Raw prompt in, raw text out. No sessions, no streaming events, no MCP passthrough | Phase 2 |
| `CliWrapTransport` | Fallback for any CLI tool | Spawns `agent-binary <task>` per invocation. One-shot, no sessions | Phase 1 |

### `SessionEvent`

```rust
pub enum SessionEvent {
    TextDelta(String),
    ToolCallStart { id: String, name: String, input: Value },
    ToolCallResult { id: String, output: Value },
    StatusUpdate(AgentStatus),
    RateLimitHit { retry_after: Option<Duration> },
    Error { code: i32, message: String },
    Complete { session_id: SessionId },
}
```

### Agent Registry

```rust
pub struct AgentRegistry {
    agents: HashMap<String, AgentConfig>,
}

pub struct AgentConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub transport: TransportKind,        // Acp | Stdio | CliWrap
    pub capabilities: Vec<String>,
    pub role: AgentRole,                 // Brain | Worker | Both
    pub cost_tier: CostTier,             // High | Medium | Low
    pub rate_limit_window: Option<Duration>,
}
```

### Process Lifecycle

- Spawn via `tokio::process::Command` with stdin/stdout pipes
- Watchdog timer per session (configurable, default 30 min)
- On crash: exponential backoff retry (1s, 4s, 16s, max 3 attempts)
- On hang (no output for configurable timeout, default 2 min): send cancel, then SIGTERM, then SIGKILL
- Clean shutdown on SPUR exit: send `shutdown` to all active sessions, wait 5s, SIGTERM stragglers

---

## 5. `spur-mcp` — MCP Callback Server

SPUR runs a lightweight MCP server passed to the brain agent during ACP initialization. Transport: Unix domain socket (`/tmp/spur-mcp-<session-id>.sock`).

### Tools Exposed to Brain Agent

**Delegation:**
- `delegate_to_worker(agent, task, context_files?)` — Spawns worker in isolated worktree. Blocks until worker completes. Returns `{ status, diff, summary, cost }`.
- `delegate_parallel(tasks: [{agent, task}])` — Spawns multiple workers concurrently. Returns when all complete. Returns `[{ status, diff, summary, cost }]`.
- `list_available_workers()` — Returns `[{ name, capabilities, health, cost_tier }]`.

**PM Context:**
- `get_issue(source, id)` — Returns `{ title, body, labels, priority, links }`.
- `update_issue(source, id, status?, comment?)` — Updates issue in PM tool.
- `create_pr(title, body, branch)` — Creates PR, returns URL.

**Orchestration:**
- `report_progress(message)` — Displayed in TUI, logged.
- `get_session_cost()` — Returns current session cost breakdown.

### Design Decisions

1. `delegate_to_worker` **blocks**. Brain's ReAct loop pauses during delegation, resumes when result arrives. No polling.
2. `delegate_parallel` for concurrent work. Brain pays one tool call, gets N workers.
3. **Workers do NOT get SPUR MCP tools.** Workers are leaf executors. No delegation chains.
4. Brain crafts the delegation prompt (it knows what context to include). Optional `context_files` hints which files the worker needs.

### Fallback for Agents Without MCP Passthrough

If the brain doesn't support MCP connection during ACP init, SPUR falls back to prompt-based delegation:

- SPUR's prompt includes: *"To delegate, respond with `<spur:delegate agent="codex">task</spur:delegate>`"*
- SPUR parses streaming output for these blocks
- On match: pauses stream, spawns worker, injects result as follow-up prompt

---

## 6. `spur-worktree` — Git Worktree Lifecycle

### Rules

- Brain works in the **original repo directory** (trunk)
- Each worker gets an isolated worktree

### Lifecycle

```
1. Snapshot brain's dirty state
   └─ git stash create → stash_ref
   └─ Create temp branch: spur/brain-snapshot-<timestamp>
   └─ Apply stash, commit

2. Create worktree
   └─ git worktree add .spur/worktrees/<session-id> spur/brain-snapshot-<timestamp>
   └─ Branch: spur/worker-<agent>-<session-id>

3. Worker executes in worktree CWD
   └─ Uses own MCP tools, all changes scoped to worktree

4. Worker completes
   └─ SPUR collects git diff
   └─ Auto-commits worker changes on worker branch

5. Merge strategy
   └─ Single worker: cherry-pick onto brain's branch
   └─ Parallel workers: sequential cherry-pick, abort on conflict → notify brain

6. Cleanup
   └─ git worktree remove, git branch -d
```

### Conflict Handling

On merge conflict, SPUR does NOT auto-resolve. Returns to brain via MCP tool response:
```json
{ "status": "conflict", "files": ["..."], "markers": "..." }
```
Brain decides: resolve itself, re-delegate, or ask the user.

### Constraints

- Max concurrent worktrees: configurable, default 5
- Worktree directory: `.spur/worktrees/` (gitignored)
- Stale cleanup: on `spur` startup, remove worktrees older than 24h
- Workers can commit locally but SPUR never pushes worker branches to remote

### Data Structure

```rust
pub struct WorktreeManager {
    repo_root: PathBuf,
    active: HashMap<SessionId, WorktreeInfo>,
}

pub struct WorktreeInfo {
    pub session_id: SessionId,
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub agent: String,
    pub created_at: Instant,
}
```

---

## 7. `spur-core` — Orchestrator Engine

### Core Struct

```rust
pub struct Orchestrator {
    registry: AgentRegistry,
    mcp_server: McpCallbackServer,
    worktrees: WorktreeManager,
    cost_tracker: CostTracker,
    pm: PmAdapterSet,
    config: SpurConfig,
}
```

### Execution Modes

**Ad-hoc** (`spur run <task>`):
1. Resolve brain agent from config or `--brain` flag
2. Optionally fetch issue context if task references an issue
3. Build brain prompt: task + available workers + system instructions
4. Start MCP callback server for this session
5. Spawn brain agent via ACP, pass MCP endpoint
6. Send prompt, stream events
7. Handle MCP callbacks (delegate_to_worker, etc.) as they arrive
8. On brain completion: collect all diffs, create PR if configured
9. Log costs, clean up worktrees

**Workflow** (`spur workflow run <file>`):
1. Match trigger to workflow definition
2. Fetch issue context from PM adapter
3. Build brain prompt from workflow template
4. Execute same pipeline as ad-hoc
5. On completion: run workflow.completion actions

### Event Bus

All components communicate through a central event channel:

```rust
pub enum SpurEvent {
    // Lifecycle
    BrainSpawned { agent: String, session: SessionId },
    WorkerSpawned { agent: String, session: SessionId, worktree: PathBuf },
    SessionCompleted { session: SessionId, result: TaskResult },

    // Streaming
    AgentOutput { session: SessionId, event: SessionEvent },

    // Orchestration
    DelegationRequested { from: SessionId, to_agent: String, task: String },
    DelegationCompleted { worker_session: SessionId, result: DelegationResult },
    ConflictDetected { files: Vec<PathBuf> },

    // Rate limits
    RateLimitDetected { agent: String, retry_after: Option<Duration> },
    BrainFailover { from: String, to: String },

    // Cost
    CostUpdate { session: SessionId, delta: CostDelta },

    // PM
    IssueReceived { source: PmSource, id: String },
    PrCreated { url: String },
    IssueUpdated { source: PmSource, id: String, status: String },
}
```

TUI and cost tracker subscribe independently. Orchestrator emits, consumers react.

### Rate Limit Failover

When the brain hits a rate limit:
1. Detect via `SessionEvent::RateLimitHit`
2. Persist brain's current task state (original prompt + delegation results so far)
3. Spawn next brain in `brain_fallback` list
4. Send resume prompt: original task + completed subtasks + remaining work
5. New brain picks up approximately where the old one left off

Worker rate limits: report failure to brain via MCP tool response, brain decides.

---

## 8. `spur-pm` — PM Adapters

### Trait

```rust
#[async_trait]
pub trait PmAdapter: Send + Sync {
    async fn connect(&mut self, config: &PmConfig) -> Result<()>;
    async fn get_issue(&self, id: &str) -> Result<Issue>;
    async fn list_issues(&self, filter: IssueFilter) -> Result<Vec<IssueSummary>>;
    async fn update_issue(&self, id: &str, update: IssueUpdate) -> Result<()>;
    async fn create_pr(&self, params: PrParams) -> Result<PrUrl>;
    async fn poll(&self) -> Result<Vec<PmEvent>>;
}
```

### Common Data Model

```rust
pub struct Issue {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub priority: Option<Priority>,
    pub assignee: Option<String>,
    pub status: String,
    pub linked_prs: Vec<String>,
    pub url: String,
}

pub enum PmSource { GitHub, Linear, Plane }
```

### Implementations

| Adapter | Auth | API | Phase |
|---------|------|-----|-------|
| `GitHubAdapter` | `gh` CLI (already authenticated) | `gh issue view`, `gh pr create`, `gh issue edit` | Phase 1 |
| `LinearAdapter` | OAuth2 via `spur connect linear` | GraphQL | Phase 3 |
| `PlaneAdapter` | API key, self-hosted URL | REST | Phase 3 |

Phase 1 GitHub adapter leans on `gh` CLI to avoid OAuth/token management.

Polling via `gh issue list --json ... --since <last_poll>` on configurable interval (default 30s). Webhook listener (localhost HTTP) added in Phase 3.

---

## 9. `spur-cost` — Cost Tracking

### SQLite Schema (`~/.spur/cost.db`)

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    agent TEXT NOT NULL,
    role TEXT NOT NULL,                -- "brain" | "worker"
    parent_session TEXT,               -- brain's session ID (NULL for brain)
    task_summary TEXT,
    project TEXT,
    issue_ref TEXT,                     -- "github:owner/repo#42"
    started_at TEXT NOT NULL,
    ended_at TEXT,
    status TEXT NOT NULL,               -- running | completed | failed | rate_limited | cancelled
    duration_seconds INTEGER,
    estimated_cost_usd REAL
);

CREATE TABLE delegation_log (
    id INTEGER PRIMARY KEY,
    brain_session TEXT NOT NULL REFERENCES sessions(id),
    worker_session TEXT NOT NULL REFERENCES sessions(id),
    task TEXT NOT NULL,
    agent TEXT NOT NULL,
    requested_at TEXT NOT NULL,
    completed_at TEXT,
    status TEXT NOT NULL,               -- success | failed | conflict | timeout
    diff_stats TEXT                     -- "+42 -7 across 3 files"
);
```

### Cost Estimation (Phase 1 Heuristic)

```rust
pub fn estimate_cost(tier: CostTier, duration: Duration) -> f64 {
    match tier {
        CostTier::High   => duration.as_secs_f64() * 0.008,  // ~$0.50/min
        CostTier::Medium => duration.as_secs_f64() * 0.003,  // ~$0.18/min
        CostTier::Low    => duration.as_secs_f64() * 0.001,  // ~$0.06/min
    }
}
```

Refined when agents report actual token counts via ACP.

`delegation_log` builds the dataset for Phase 4 Smart Router (agent success rates by task type).

---

## 10. `spur-cli` & `spur-tui`

### CLI Commands

```
spur init                              Auto-detect agents, create ~/.spur/agents.toml
spur agents                            List agents: name, transport, health, role
spur agents add <path>                 Register custom agent
spur agents remove <name>              Remove agent
spur agents check                      Health-check all agents

spur run <task>                        Ad-hoc task through brain agent
spur run --brain kiro <task>           Override brain
spur run --issue github:owner/repo#42  Pull issue context
spur run --background <task>           Detached execution

spur exec --agent codex <task>         Direct single-agent (no brain, no delegation)

spur sessions                          List active/recent sessions
spur sessions show <id>                Session detail: events, delegations, cost
spur sessions kill <id>                Terminate + cleanup

spur cost [--week] [--by agent|project] [--export csv]

spur connect github                    Verify gh CLI auth

spur workflow validate <file>          Check TOML (Phase 3)
spur workflow run <file>               Execute workflow (Phase 3)

spur watch                             Launch TUI (Phase 2)
```

### TUI (Phase 2, Weeks 11-12)

Two-panel layout: agents panel + active session panel. Subscribes to `SpurEvent` channel.

Phase 1 has no TUI. Streaming terminal output with `[brain:kiro]`, `[worker:codex]` prefixes.

---

## 11. Configuration

### User Config (`~/.spur/config.toml`)

```toml
[brain]
default = "kiro"
fallback = ["claude-code"]

[[agents.entries]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
role = "both"
capabilities = ["spec-driven", "security", "full-stack"]
cost_tier = "medium"

[[agents.entries]]
name = "claude-code"
command = "claude"
args = ["--experimental-acp"]
transport = "acp"
role = "both"
capabilities = ["architecture", "refactoring", "debugging", "code-review"]
cost_tier = "high"

[[agents.entries]]
name = "codex"
command = "codex"
args = ["--acp"]
transport = "acp"
role = "worker"
capabilities = ["quick-edits", "tests", "python"]
cost_tier = "low"

[[agents.entries]]
name = "gemini"
command = "gemini"
args = []
transport = "cli-wrap"
role = "worker"
capabilities = ["large-context", "analysis", "documentation"]
cost_tier = "low"

[failover]
cooldown_minutes = 30

[worktree]
max_concurrent = 5
stale_cleanup_hours = 24

[cost]
db_path = "~/.spur/cost.db"

[pm.github]
enabled = true
use_gh_cli = true
```

### Project Config (`.spur/config.toml`)

```toml
[project]
name = "my-app"

[brain]
default = "kiro"

[brain.prompt]
append = """
This is a Rust web service using Axum + SQLx.
Key directories: src/api/ (handlers), src/db/ (queries), src/auth/ (JWT).
Always run `cargo test` after changes.
"""

[pm.github]
repo = "owner/my-app"
auto_label = "spur-managed"

[worktree]
max_concurrent = 3
```

### Agent Prompts (`agent-prompts/brain-kiro.md`)

System instructions injected into the brain's first prompt, teaching it how to use SPUR's MCP delegation tools:

```markdown
You are coordinating a coding task. You have two kinds of tools:

1. Your own tools (filesystem, bash, git) — use these to investigate and code directly.
2. SPUR delegation tools — use these to hand work to specialized worker agents.

Available SPUR tools:
- delegate_to_worker(agent, task) — send a subtask to a worker agent
- delegate_parallel(tasks) — send multiple independent subtasks concurrently
- list_available_workers() — see available agents and their strengths
- get_issue_context(source, id) — fetch issue details from GitHub/Linear
- create_pr(title, body, branch) — create a pull request
- report_progress(message) — update the human on what you're doing

When to delegate vs do it yourself:
- Delegate when subtasks are INDEPENDENT and can run in parallel
- Delegate to match agent strengths (e.g., codex for tests, claude for deep reasoning)
- Do it yourself for quick tasks or when you need tight iterative control
- Always review worker output before approving
```

Priority: CLI flags > Project `.spur/config.toml` > User `~/.spur/config.toml`.

---

## 12. Phasing & Milestones

### Phase 1 — Single-Agent Pipeline (Weeks 1-6)

**Week 1-2: `spur-acp` + `spur-cli` skeleton**
- `trait AgentTransport` with `AcpTransport` and `CliWrapTransport`
- `AgentRegistry` + `agents.toml` config parsing
- `spur init` — scan `$PATH`, detect agents, generate config
- `spur agents` / `spur agents check`
- Milestone: `spur agents` shows detected agents with health status

**Week 3-4: `spur-mcp` + `spur-core` orchestrator**
- MCP callback server with `delegate_to_worker`, `list_available_workers`
- Orchestrator `run_adhoc` flow
- `SpurEvent` channel
- `spur run <task>` with streaming terminal output
- `spur exec --agent <name> <task>`
- Milestone: Brain agent responds to tasks. Delegation flows through SPUR.

**Week 5-6: `spur-pm` GitHub + `spur-worktree`**
- `GitHubAdapter` via `gh` CLI
- `spur run --issue github:owner/repo#42`
- `WorktreeManager` — create/cleanup worktrees for workers
- Milestone: **"Issue in, PR out" works end-to-end.**

### Phase 2 — Multi-Agent + Observability (Weeks 7-12)

**Week 7-8:** Concurrent workers, `delegate_parallel`, `StdioTransport`
**Week 9-10:** SQLite cost DB, `spur cost` commands, delegation logging
**Week 11-12:** Two-panel TUI, `spur watch`, session management commands

### Phase 3 — Workflows + PM Expansion (Weeks 13-18)

**Week 13-14:** TOML workflow engine, DAG builder, `spur workflow run`
**Week 15-16:** Rate limit detection + brain/worker failover
**Week 17-18:** Linear adapter (GraphQL), Plane adapter (REST)

### Phase 4 — Commercial (Weeks 19-26)

Team dashboard, Smart Router (from delegation_log data), hosted SPUR.

### Success Criteria

| Phase | Metric |
|-------|--------|
| Phase 1 | 10 developers complete a real task through SPUR and report time savings |
| Phase 2 | 50% of delegated subtasks succeed on first attempt |
| Phase 3 | Average issue-to-PR time reduced 40% vs manual workflow |
| Phase 4 | $5K MRR with <5% monthly churn |

---

## 13. Risks

| Risk | Type | Likelihood | Mitigation |
|------|------|-----------|------------|
| Market too narrow | Strategic | Medium | 6-week MVP validates fast; PM integration alone is valuable |
| Process management fragility | Execution | High | Watchdog timers, CliWrapTransport fallback, disproportionate testing |
| ACP too immature (Claude experimental) | Technical | Medium | `trait AgentTransport` with stdio/CLI fallbacks |
| Brain ignores delegation tools | UX | Low | Crafted system prompts + examples per brain agent |
| ACP MCP-passthrough inconsistent | Technical | Medium | Prompt-based delegation fallback |
| Worktree merge conflicts | Execution | Medium | No auto-resolve; report to brain for decision |
| Brain rate-limited (circular dependency) | Technical | Medium | Use different providers for brain vs workers (Kiro/Bedrock as brain, Claude/Anthropic as worker) |
| Cost tracking approximate | Technical | Low | Heuristic in Phase 1, refined with agent-reported tokens later |

---

## 14. Key Design Decisions Log

1. **SPUR is infrastructure, not intelligence.** Agents think, SPUR plumbs.
2. **Brain-agent-on-top.** An existing agent (Kiro/Claude Code) runs ReAct orchestration. SPUR provides the delegation mechanism.
3. **MCP callback for delegation.** Brain calls SPUR's MCP tools to delegate. Fallback: prompt-based structured output for agents without MCP passthrough.
4. **ACP as primary transport, with fallbacks.** `trait AgentTransport` with ACP, stdio, and CLI-wrap implementations.
5. **Workers are leaf nodes.** No delegation chains. Workers do not get SPUR's MCP tools.
6. **`delegate_to_worker` blocks.** Simplest model — brain pauses during delegation.
7. **GitHub via `gh` CLI in Phase 1.** Avoids OAuth complexity. Uses existing auth.
8. **CLI-first, TUI-later.** Phase 1 is terminal streaming output. TUI is Phase 2 polish.
9. **Provider diversity is a feature.** Different providers for brain vs workers = independent rate limit pools.
10. **Collect delegation quality data from day one.** `delegation_log` feeds Phase 4 Smart Router.
