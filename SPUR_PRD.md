# SPUR — Product Requirements Document

**Version:** 1.0
**Date:** April 12, 2026
**Author:** Kevin
**Status:** Draft

---

## 1. Executive Summary

### Product Vision

SPUR is a Rust-native TUI (Terminal User Interface) that orchestrates multiple AI coding agents through the Agent Client Protocol (ACP). It uses a "brain" agent (Claude Code or Kiro CLI) as a ReAct reasoning engine to intelligently route tasks across worker agents (Codex, Gemini, OpenCode, and others), while integrating with project management tools (Linear, Plane, GitHub Issues) to create a closed-loop automation pipeline.

**One-liner:** *"Issue in, PR out — across every agent."*

### Problem Statement

Developers in 2026 face three compounding pain points:

1. **Rate limit fragility.** Claude Code Max users ($100–200/mo) routinely exhaust 5-hour session windows in under 90 minutes. When limits hit, all work stops.
2. **Single-vendor lock-in.** Claude Code subagents and Agent Teams are Claude-only. No native mechanism exists to delegate work to Codex, Kiro, or Gemini based on task fit.
3. **Manual coordination overhead.** Developers manually copy issues from Linear, paste context into agents, collect outputs, create PRs, and update issue status — a workflow that consumes 30–60 min/day of senior engineering time.

### Solution

SPUR sits between project management tools and AI coding agents as a **coordination layer**:

```
Linear/Plane/GitHub ←→ SPUR TUI ←→ Claude Code / Kiro / Codex / Gemini
       (work)          (brain)              (workers)
```

The tool does NOT replace any agent. It makes every agent more effective by routing the right task to the right agent at the right time.

---

## 2. Target Users

### Primary Persona: "The Orchestrator"

- **Role:** Senior/Staff Engineer or Tech Lead at a startup or mid-size company (10–200 employees)
- **Current tooling:** Claude Code Max ($100–200/mo), plus 1–2 additional agent CLIs (Kiro, Codex, Cursor)
- **Monthly AI spend:** $200–600 across tools
- **Pain frequency:** Hits rate limits 2–5x per week, manually juggles 2–3 terminal tabs with different agents
- **Technical profile:** Comfortable with terminal workflows, uses tmux/zellij, familiar with git worktrees
- **Decision driver:** Productivity and flow preservation, not cost alone

### Secondary Persona: "The Team Lead"

- **Role:** Engineering Manager or Director overseeing 3–10 developers
- **Pain:** No visibility into which agents the team uses, total AI spend per project, or whether agents are being used effectively
- **Decision driver:** Cost visibility, standardization, and governance

### Anti-Persona (Not For)

- Junior developers who use a single AI assistant casually
- Non-technical users who need a GUI
- Enterprise teams requiring SOC2/HIPAA compliance at launch

---

## 3. Product Principles

1. **The tool is the nervous system, not the brain.** SPUR coordinates; agents think. Never compete with the reasoning capabilities of Claude, Kiro, or Codex.
2. **Open source core, proprietary intelligence.** The TUI, ACP client, and workflow engine are MIT-licensed. Routing intelligence and team analytics are commercial.
3. **Single binary, zero dependencies.** `cargo install spur-cli` or `curl | sh`. No Node.js, no Python, no Docker required.
4. **Protocol-first, not vendor-first.** ACP is the integration layer. Any agent that speaks ACP works with SPUR automatically.
5. **Visibility over automation.** Show the developer what's happening (ReAct traces, cost tracking, agent status) before automating decisions away.

---

## 4. Architecture

### System Overview

```
┌──────────────────────────────────────────────────────────┐
│  SPUR TUI  (ratatui + crossterm)                        │
│  ┌─────────────┬────────────┬──────────────────────────┐ │
│  │ Issue Queue │ ReAct Log  │ Agent Sessions           │ │
│  │ (synced)    │ (trace)    │ (live streams)           │ │
│  └─────────────┴────────────┴──────────────────────────┘ │
├──────────────────────────────────────────────────────────┤
│  Workflow Engine                                        │
│  ┌────────────────────────────────────────────────────┐  │
│  │ TOML parser → DAG builder → Task scheduler        │  │
│  │ Rate limit monitor → Failover router              │  │
│  └────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────┤
│  PM Adapter Layer          │  ACP Session Manager       │
│  ┌──────────────────────┐  │  ┌───────────────────────┐ │
│  │ Linear (GraphQL)     │  │  │ JSON-RPC 2.0 / stdio  │ │
│  │ Plane  (REST + MCP)  │  │  │ Session persistence   │ │
│  │ GitHub (REST)        │  │  │ Crash recovery        │ │
│  └──────────────────────┘  │  │ Structured events     │ │
│                            │  └───────────────────────┘ │
├────────────────────────────┴────────────────────────────┤
│  Agent Registry                                        │
│  ┌──────────┬──────────┬──────────┬──────────┬────────┐ │
│  │ Claude   │ Kiro CLI │ Codex    │ Gemini   │ Custom │ │
│  │ Code     │          │ CLI      │ CLI      │        │ │
│  └──────────┴──────────┴──────────┴──────────┴────────┘ │
└──────────────────────────────────────────────────────────┘
```

### Core Components

| Component | Responsibility | Key Crates |
|---|---|---|
| **TUI Renderer** | Terminal UI with panels, keybindings, theming | `ratatui`, `crossterm` |
| **ACP Client** | JSON-RPC 2.0 transport, session lifecycle, event parsing | `serde_json`, `tokio` |
| **Workflow Engine** | TOML workflow parsing, DAG execution, task scheduling | `toml`, `petgraph` |
| **PM Adapters** | Issue ingestion, status updates, PR linking | `reqwest`, `graphql_client` |
| **Agent Registry** | Agent discovery, capability mapping, health checks | custom |
| **Brain Connector** | ReAct loop with brain agent, delegation prompts | ACP Client |
| **Cost Tracker** | Token/credit usage per agent, per task, per project | `rusqlite` |

### Data Flow

```
1. PM tool webhook/poll → Issue arrives
2. Workflow engine matches issue to workflow definition
3. Brain agent receives issue context + routing hints
4. Brain reasons (ReAct): THINK → ACT → OBSERVE → repeat
5. Brain delegates subtasks via SPUR to worker agents (ACP)
6. Workers execute in isolated sessions
7. Results flow back through brain for synthesis
8. SPUR pushes PR link + status update to PM tool
9. Human reviews in TUI → approve/reject/redirect
```

---

## 5. Feature Specifications

### Phase 1: Foundation (Weeks 1–6)

#### F1.1 — Agent Discovery & Registry

**Description:** Auto-detect installed ACP-compatible agents and maintain a registry of their capabilities, cost profiles, and health status.

**Acceptance Criteria:**
- `spur init` scans `$PATH` for known agent binaries (claude, kiro-cli, codex, gemini)
- `spur agents` displays a table: name, version, ACP support (yes/no), status (ready/error), last used
- `spur agents add <path>` registers a custom agent
- Agent config stored in `~/.spur/agents.toml`

**Agent Config Schema:**
```toml
[[agents]]
name = "claude-code"
command = "claude"
args = ["--experimental-acp"]
capabilities = ["architecture", "refactoring", "debugging", "code-review"]
cost_tier = "high"
rate_limit_window = "5h"

[[agents]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
capabilities = ["spec-driven", "security", "full-stack"]
cost_tier = "medium"

[[agents]]
name = "codex"
command = "codex"
args = ["--acp"]
capabilities = ["quick-edits", "tests", "python"]
cost_tier = "low"
```

#### F1.2 — ACP Session Manager

**Description:** Manage ACP agent sessions with full lifecycle support: spawn, send prompts, receive streaming responses, reconnect on crash, and graceful shutdown.

**Acceptance Criteria:**
- Spawn agent process via `Command` with stdio pipes
- Send JSON-RPC 2.0 `initialize`, `session/create`, `session/prompt` messages
- Parse streaming `session/notification` events (text chunks, tool calls, status updates)
- Persist session IDs to `~/.spur/sessions/` for reconnection
- Auto-restart crashed agents with exponential backoff (max 3 retries)
- `spur sessions` lists active sessions with agent, status, duration, token usage

**ACP Message Flow:**
```
SPUR → Agent:  {"jsonrpc":"2.0","method":"initialize","id":1}
Agent → SPUR:  {"jsonrpc":"2.0","result":{"capabilities":{...}},"id":1}
SPUR → Agent:  {"jsonrpc":"2.0","method":"session/create","id":2}
Agent → SPUR:  {"jsonrpc":"2.0","result":{"sessionId":"abc123"},"id":2}
SPUR → Agent:  {"jsonrpc":"2.0","method":"session/prompt","id":3,
                 "params":{"sessionId":"abc123","prompt":[{"type":"text","text":"..."}]}}
Agent → SPUR:  {"jsonrpc":"2.0","method":"session/notification",
                 "params":{"type":"text","text":"Working on..."}} (streaming)
```

#### F1.3 — TUI Dashboard

**Description:** Multi-panel terminal interface showing agent sessions, logs, and status.

**Layout:**
```
┌─ SPUR v0.1.0 ──────────────────────────────────────────┐
│ Agents [1]         │ Active Session                    │
│ ┌────────────────┐ │ ┌──────────────────────────────┐  │
│ │ ● claude  IDLE │ │ │ 🧠 THINK: Analyzing ENG-142 │  │
│ │ ● kiro    BUSY │ │ │ 🔧 ACT: Reading src/auth.rs │  │
│ │ ○ codex   OFF  │ │ │ 👁 OBSERVE: Found JWT bug   │  │
│ │ ○ gemini  OFF  │ │ │ 🔧 ACT: Delegating to kiro  │  │
│ └────────────────┘ │ │ ...streaming...              │  │
│                    │ └──────────────────────────────┘  │
│ Cost Today [2]     │                                   │
│ ┌────────────────┐ │                                   │
│ │ claude: $4.20  │ │                                   │
│ │ kiro:   $1.80  │ │                                   │
│ │ total:  $6.00  │ │                                   │
│ └────────────────┘ │                                   │
├────────────────────┴───────────────────────────────────┤
│ [q]uit [a]gents [s]essions [r]un [c]ost [?]help       │
└────────────────────────────────────────────────────────┘
```

**Keybindings:**
- `q` — quit
- `a` — focus agent panel
- `s` — focus session panel
- `r` — run a workflow or ad-hoc prompt
- `c` — toggle cost panel
- `Tab` — cycle between panels
- `Enter` — interact with selected agent/session
- `Esc` — back to overview
- `1-9` — jump to agent session by index

#### F1.4 — Ad-hoc Task Execution

**Description:** Run a single task through a brain agent without a predefined workflow.

**Usage:**
```bash
# Interactive mode
spur run "fix the authentication bypass in jwt.rs"

# With explicit brain and workers
spur run --brain kiro --workers claude,codex "implement user profile API"

# With specific agent (no brain, direct execution)
spur exec --agent codex "write tests for src/auth/"

# Fire-and-forget
spur run --background "refactor the database layer"
```

**Acceptance Criteria:**
- Brain agent receives task description + codebase context
- Brain can delegate to worker agents via structured tool calls
- ReAct trace displayed in real-time in TUI
- Final output summarized and displayed on completion
- Exit code reflects success (0) or failure (1)

### Phase 2: Workflow Engine (Weeks 7–12)

#### F2.1 — TOML Workflow Definitions

**Description:** Declarative workflow definitions that map triggers to agent pipelines.

**Schema:**
```toml
[workflow]
name = "bug-fix"
description = "Automated bug fix pipeline"
version = "1.0"

[workflow.trigger]
source = "linear"             # linear | plane | github | manual
filter.label = ["bug"]
filter.priority = ["urgent", "high"]
filter.team = "engineering"

[workflow.brain]
agent = "kiro"
prompt = """
You are a bug-fix coordinator. Given the issue context:
1. Analyze the bug scope and identify affected files
2. Choose the best worker agent based on the bug type:
   - Security bugs → kiro (spec-driven security review)
   - Performance bugs → codex (fast targeted edits)
   - Complex logic bugs → claude (deep reasoning)
3. Delegate the fix to the chosen worker
4. Review the worker's output
5. If tests pass, approve. If not, iterate.
"""

[workflow.routing]
security = { agent = "kiro", match = "auth|jwt|csrf|xss|injection" }
performance = { agent = "codex", match = "slow|timeout|latency|memory" }
default = { agent = "claude-code" }

[workflow.pipeline]
steps = [
  { name = "fix", action = "delegate", timeout = "15m" },
  { name = "test", action = "exec", agent = "codex", command = "run tests" },
  { name = "review", action = "delegate", agent = "claude-code", prompt = "review this diff for correctness" },
]

[workflow.completion]
create_pr = true
update_issue_status = "in-review"
notify = ["slack:#dev-alerts"]
```

#### F2.2 — Rate Limit Detection & Failover

**Description:** Monitor agent rate limit status and automatically reroute tasks when limits are hit.

**Acceptance Criteria:**
- Detect rate limit signals from ACP session notifications (error codes, retry-after headers)
- Detect heuristic signals: response latency > 30s, repeated errors, empty responses
- Maintain a rate limit state machine per agent: AVAILABLE → THROTTLED → EXHAUSTED → RECOVERING
- When brain agent is throttled, switch to backup brain (configurable)
- When worker agent is throttled, reroute to next-best agent by capability match
- Display rate limit status in TUI with countdown timers
- Log all failover events for post-hoc analysis

**Failover Config:**
```toml
[failover]
strategy = "capability-match"    # capability-match | round-robin | cost-priority
brain_fallback = ["claude-code", "kiro"]
cooldown_minutes = 30

[failover.overrides]
claude-code = { fallback = "kiro", reason = "rate-limit" }
kiro = { fallback = "codex", reason = "rate-limit" }
```

#### F2.3 — Cost Tracking

**Description:** Track token/credit usage per agent, per task, per project with local SQLite storage.

**Data Model:**
```sql
CREATE TABLE usage_events (
  id INTEGER PRIMARY KEY,
  timestamp TEXT NOT NULL,
  agent TEXT NOT NULL,
  session_id TEXT,
  task_id TEXT,
  project TEXT,
  input_tokens INTEGER,
  output_tokens INTEGER,
  estimated_cost_usd REAL,
  duration_seconds INTEGER,
  status TEXT  -- success | error | timeout | rate_limited
);
```

**CLI Commands:**
```bash
spur cost                    # Today's summary
spur cost --week             # Weekly breakdown
spur cost --by agent         # Per-agent breakdown
spur cost --by project       # Per-project breakdown
spur cost --export csv       # Export for spreadsheets
```

### Phase 3: PM Integration (Weeks 13–18)

#### F3.1 — Linear Integration

**Capabilities:**
- OAuth2 authentication via `spur connect linear`
- Poll or webhook for new/updated issues
- Read issue context: title, description, labels, priority, team, linked PRs
- Write: update issue status, add comments (agent activity log), link external PRs
- Agent Plan API: push step-by-step progress back to Linear UI

**Config:**
```toml
[integrations.linear]
enabled = true
team = "ENG"
poll_interval = "30s"
auto_assign_labels = ["spur-managed"]
status_mapping.in_progress = "In Progress"
status_mapping.in_review = "In Review"
status_mapping.done = "Done"
```

#### F3.2 — Plane Integration

**Capabilities:**
- REST API + MCP server connection
- Same read/write capabilities as Linear
- Self-hosted support (custom Plane URL)
- Webhook listener for real-time issue updates

#### F3.3 — GitHub Issues Integration

**Capabilities:**
- GitHub App or PAT authentication
- Issue ↔ PR linking
- Label-based workflow triggers
- Comment-based status updates
- CI status monitoring (GitHub Actions)

### Phase 4: Team & Commercial (Weeks 19–26)

#### F4.1 — Team Dashboard (Commercial)

- Centralized cost analytics across all team members
- Per-developer usage breakdown
- Agent utilization heatmaps
- Budget alerts and caps

#### F4.2 — Smart Router (Commercial)

- ML-based task classification: analyze task description to predict best agent
- Historical success rate per agent per task type
- Cost-optimized routing: minimize spend while maintaining quality threshold
- A/B testing: route 20% of tasks to Agent B to measure quality vs Agent A

#### F4.3 — Unified Billing (Commercial)

- Single invoice for all agent API usage
- Team-wide API key management
- Per-project budget allocation
- BYOK (Bring Your Own Keys) with pass-through billing

---

## 6. Technical Requirements

### Performance

| Metric | Target |
|---|---|
| Binary size | < 15 MB |
| Memory usage (idle) | < 30 MB |
| Memory usage (5 active sessions) | < 100 MB |
| ACP message latency (SPUR overhead) | < 1 ms |
| TUI render frame rate | 30 FPS |
| Startup time | < 500 ms |
| Agent spawn time | < 2 s |

### Compatibility

| Requirement | Specification |
|---|---|
| Platforms | Linux (x86_64, aarch64), macOS (aarch64, x86_64), Windows (x86_64) |
| Terminal emulators | iTerm2, Alacritty, Kitty, Windows Terminal, tmux, zellij |
| Rust edition | 2021, MSRV 1.75 |
| ACP spec version | 1.0 (as of Feb 2026) |

### Dependencies (Minimal)

```toml
[dependencies]
ratatui = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
reqwest = { version = "0.12", features = ["json"] }
rusqlite = { version = "0.32", features = ["bundled"] }
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = "0.3"
directories = "5"
```

### Security

- All agent communication via stdio (no network exposure)
- API keys stored in OS keychain via `keyring` crate, never in plaintext
- PM tool tokens encrypted at rest
- No telemetry by default; opt-in anonymous usage stats
- All data stored locally in `~/.spur/`

---

## 7. CLI Reference

### Global Commands

```
spur init                          Initialize SPUR in current directory
spur agents                        List registered agents
spur agents add <path>             Register a custom agent
spur agents remove <name>          Remove an agent
spur sessions                      List active sessions
spur sessions kill <id>            Terminate a session
spur run <task>                    Run an ad-hoc task
spur exec --agent <name> <task>    Execute directly on a specific agent
spur watch                         Open TUI dashboard
spur cost                          Show cost summary
spur connect <service>             Authenticate with a PM tool
spur workflow run <file>           Execute a workflow definition
spur workflow validate <file>      Validate a TOML workflow
spur version                       Show version info
```

### Configuration Hierarchy

```
1. CLI flags (highest priority)
2. Environment variables (SPUR_BRAIN, SPUR_AGENTS, etc.)
3. Project config: .spur/config.toml (per-repo)
4. User config: ~/.spur/config.toml (global)
5. Defaults (lowest priority)
```

---

## 8. Metrics & Success Criteria

### Phase 1 Success (Week 6)

- SPUR can spawn and manage ACP sessions with ≥ 3 different agents
- TUI renders agent sessions with streaming output
- `spur run` completes an ad-hoc task end-to-end
- 0 known crash bugs on Linux and macOS

### Phase 2 Success (Week 12)

- TOML workflows execute multi-step pipelines
- Rate limit failover triggers within 5 seconds of detection
- Cost tracking captures ≥ 90% of token usage events
- 500+ GitHub stars

### Phase 3 Success (Week 18)

- Closed-loop: Linear issue → agent fix → PR → status update
- Plane and GitHub Issues integrations functional
- 2,000+ GitHub stars, 200+ daily active users

### Phase 4 Success (Week 26)

- Team dashboard with ≥ 3 paying teams
- Smart router shows ≥ 15% cost reduction vs manual agent selection
- $5K MRR milestone

---

## 9. Competitive Landscape

| Tool | Language | Interface | ACP Native | Cross-Agent | PM Integration | Workflow Engine |
|---|---|---|---|---|---|---|
| **SPUR** | **Rust** | **TUI** | **Yes** | **Yes** | **Yes** | **Yes** |
| ACPX | Node.js | CLI only | Yes | Yes | No | No |
| TUICommander | Rust+Tauri | Desktop | No (PTY) | Detection | No | No |
| Ralph | TypeScript | TUI (read-only) | Partial | Yes | No | Hat system |
| Agent Orchestrator | Node.js | Web dashboard | No | Yes | GitHub only | YAML |
| Gas Town | TypeScript | CLI+tmux | No | Claude only | No | No |
| Claude Agent Teams | Built-in | Terminal | No | Claude only | No | No |

### SPUR's Unique Position

SPUR is the only tool that combines:
1. Rust single binary (no runtime dependencies)
2. Native ACP protocol support (structured, not PTY scraping)
3. Cross-vendor agent orchestration (any ACP agent)
4. PM tool integration (Linear, Plane, GitHub)
5. Declarative workflow engine (TOML-based)
6. Interactive TUI with real-time visibility

---

## 10. Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|---|---|---|---|
| Anthropic builds native ACP into Claude Code | Low (explicitly declined) | High | SPUR is cross-vendor; native ACP would actually help by simplifying integration |
| ACP spec changes significantly | Medium | Medium | Abstract ACP behind internal trait; version-pin spec support |
| Agent CLI interfaces break between versions | Medium | Medium | Integration tests per agent, version detection, adapter pattern |
| Rate limit detection heuristics produce false positives | High | Low | Configurable thresholds, manual override, learn from user corrections |
| Low adoption due to "extra tool" fatigue | Medium | High | Zero-config `spur init` auto-detection, immediate value in first session |
| PM tool API changes | Low | Medium | Adapter pattern, community-maintained integrations |

---

## 11. Open Questions

1. **Brain selection default:** Should SPUR default to Kiro (native ACP, cheaper) or Claude Code (better reasoning, needs adapter) as the brain agent?
2. **Workflow sharing:** Should workflows be shareable via a community registry (like Homebrew taps)?
3. **MCP integration:** Should SPUR expose itself as an MCP server so that agents can call SPUR tools (e.g., "check issue status")?
4. **Local model support:** Should SPUR support local models (via Ollama) as worker agents for cost-free fallback?
5. **Notification channels:** Beyond TUI, should SPUR support Slack/Discord/Telegram notifications for completed tasks?

---

## 12. Glossary

| Term | Definition |
|---|---|
| **ACP** | Agent Client Protocol — open standard (by Zed Industries + JetBrains) for structured communication between clients and AI coding agents via JSON-RPC 2.0 over stdio |
| **Brain agent** | The primary agent that performs ReAct reasoning, task decomposition, and delegation to worker agents |
| **Worker agent** | An agent that receives delegated subtasks from the brain and executes them |
| **ReAct** | Reasoning + Acting loop: THINK → ACT → OBSERVE → repeat until task complete |
| **Failover** | Automatic rerouting of tasks from a rate-limited or failed agent to an alternative agent |
| **Workflow** | A TOML-defined pipeline that maps triggers (e.g., new issue with label "bug") to agent actions |
| **Session** | A persistent ACP connection to an agent, maintaining conversation context across multiple prompts |
| **PM Adapter** | Module that connects SPUR to a project management tool (Linear, Plane, GitHub Issues) |

---

*SPUR — drive your agents into coordinated action.*
