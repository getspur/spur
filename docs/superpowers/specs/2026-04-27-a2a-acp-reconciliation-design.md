# A2A ↔ ACP reconciliation for SPUR

**Status:** informational design memo (no immediate code impact)
**Date:** 2026-04-27
**Owners:** Kevin Truong (kevin.truong.ds@gmail.com)
**Scope:** Strategic + technical reconciliation of Google's A2A (Agent2Agent) protocol with Zed's ACP (Agent Client Protocol) in the context of SPUR's role as an AI-agent control plane. Synthesis of two parallel research dispatches (gemini for strategic narrative; kimi for technical surface mapping).

**Relationship to v1/v2 specs:** **none for v1+v2 ship scope.** Both v1 and v2 target the editor↔agent (ACP) layer, which the research confirms stays as-is. A2A would be a *parallel* surface for brain↔worker / worker↔worker paths, not a replacement. This memo informs SPUR's longer-arc architecture but does not block or modify the v1/v2 implementation plans.

---

## 1. TL;DR

A2A and ACP solve different problems and are best understood as **complementary layers in a 3-layer agent-protocol stack**:

| Layer | Protocol | Direction | SPUR usage |
|---|---|---|---|
| Tool ↔ Agent | **MCP** (Model Context Protocol) | Agent calls tools | Used by codex-acp internally; passed through transparently |
| Client ↔ Agent | **ACP** (Agent Client Protocol) | IDE/CLI talks to agent | SPUR's current foundation; v1+v2 extend this layer |
| Agent ↔ Agent | **A2A** (Agent2Agent Protocol) | Peer agents collaborate | **Not yet used by SPUR**; relevant for brain↔worker and worker↔worker paths |

**Recommendation:** **R3 outbound-only** — keep ACP as SPUR's primary protocol for the editor/IDE-integration path, and add A2A as an *outbound* capability so SPUR can delegate sub-tasks to external A2A-compliant agents. Defer "SPUR exposes itself as an A2A server" until concrete demand exists. Adopt the **AgentCard pattern** internally for worker capability discovery even before any A2A wire integration.

This recommendation is converged across both gemini's strategic analysis and kimi's technical mapping.

## 2. Background

### 2.1 What is A2A

Per gemini's research (sources: a2aproject.dev, Linux Foundation announcements):

- **Origin:** Introduced by Google in April 2025; donated to the Linux Foundation in June 2025. Backed by 50+ industry partners (AWS, Microsoft, Salesforce, SAP, ServiceNow). IBM's competing "Agent Communication Protocol" (also abbreviated ACP, confusingly) was *absorbed into* A2A v1.0 — the standards landscape consolidated faster than typical battles.
- **Problem solved:** the "silo problem" — agents built on different frameworks (LangChain, CrewAI, Semantic Kernel) or hosted on different platforms could not collaborate without bespoke integration. A2A provides a standard messaging tier so a "Client Agent" can discover, authenticate, and delegate to a "Remote Agent" generically.
- **Design philosophy:** lightweight, enterprise-ready, network-native. Heavy reliance on existing web standards rather than reinvention.

### 2.2 What is ACP (recap)

ACP is a JSON-RPC 2.0 protocol for client↔agent communication, optimized for IDE/CLI integration. Used by Zed, codex-acp, claude-code-acp, kiro-acp, and others. Primary transport is stdio. Wire format and types verified extensively in v1 and v2 specs.

### 2.3 What was researched and how

- **gemini** (delegation `5f858efe`): strategic + design overview, comparison to ACP/MCP, what A2A could mean for SPUR. Output was 8K lines of largely repetitive thinking; the actionable deliverable lived in the final ~60 lines.
- **kimi** (delegation `1aedea90`): concrete A2A v1.0 protocol surface inventory + side-by-side mapping to ACP + reconciliation patterns R1/R2/R3 with cost/risk/value. Output was incomplete (kimi fabricated a file write that didn't happen); actionable content extracted from kimi's thinking trace.

## 3. The 3-layer stack thesis

Both gemini and kimi independently arrived at the same conclusion:

```
                         ┌─────────────────────────────┐
                         │  Editor / CLI / Human user  │
                         └─────────────┬───────────────┘
                                       │ ACP
                                       │ (client ↔ agent)
                              ┌────────▼────────┐
              ┌──── A2A ─────►│   Agent (peer)  │◄──── A2A ────┐
              │               └────────┬────────┘              │
              │                        │ MCP                   │
              │                        │ (agent ↔ tool)        │
       ┌──────▼─────┐         ┌────────▼────────┐      ┌──────▼─────┐
       │ Agent peer │         │  Tool / DB / FS │      │ Agent peer │
       └────────────┘         └─────────────────┘      └────────────┘
```

The three protocols are **additive, not competitive**. A complete production-grade agent system would speak all three. Each protocol has a clean primary use case:

- **MCP** = agent calls tools (Anthropic-led; well-established)
- **ACP** = editor/CLI gives context to agent (Zed-led; growing)
- **A2A** = agents collaborate as peers (Google-led, now Linux Foundation; expanding)

## 4. Concrete protocol surface comparison

Synthesized from gemini's strategic table + kimi's technical inventory.

| Axis | A2A v1.0 | ACP (current) | Notes |
|---|---|---|---|
| **Topology** | Peer-to-peer agent↔agent (symmetric) | Client-server editor↔agent (asymmetric) | Different shapes; a peer-to-peer A2A connection has no "client side" methods |
| **Unit of work** | `Task` — explicit state machine: SUBMITTED → WORKING → INPUT_REQUIRED → AUTH_REQUIRED → COMPLETED/FAILED/CANCELED/REJECTED. Designed for hours-to-days lifecycles. | `Session` + `PromptTurn` — interactive, request/response within a session. No long-lived task primitive. | A2A tasks ≈ "long-lived workflow"; ACP sessions ≈ "interactive conversation" |
| **Primary RPC count** | ~11 typed methods (SendMessage, GetTask, ListTasks, CancelTask, SubscribeToTask, push-notification CRUD, GetExtendedAgentCard) | ~10 agent-side + ~7 client-side (initialize, authenticate, session/{new,prompt,load,set_*,close}, plus client-side fs/* and terminal/*) | Comparable surface area; very different shapes |
| **Transport** | HTTPS + SSE / gRPC over HTTP/2 + TLS / HTTP+JSON+SSE / custom | stdio (newline-delimited JSON-RPC 2.0) primary; streamable HTTP draft | A2A is network-native; ACP is process-native |
| **Discovery** | `/.well-known/agent-card.json` + registries + JWS-signed AgentCards | ACP Registry; agents launched as subprocess via known binary | A2A discovery is HTTP-style (URL-based); ACP discovery is local-process |
| **Capability negotiation** | AgentCard advertises `streaming`, `pushNotifications`, `stateTransitionHistory`, `extendedAgentCard`, skills, supported interfaces, securitySchemes | `initialize` exchanges `clientCapabilities` + `agentCapabilities` (loadSession, mcpCapabilities, promptCapabilities, sessionCapabilities) | A2A is OpenAPI-shaped (declarative); ACP is RPC-shaped (handshake) |
| **Auth** | OpenAPI-style `securitySchemes` (APIKey, HTTPAuth, OAuth 2.0, OIDC, mTLS); in-task `AUTH_REQUIRED` state | `authenticate` method with `authMethods` list | A2A inherits enterprise patterns; ACP is simpler |
| **Notifications** | `TaskStatusUpdateEvent`, `TaskArtifactUpdateEvent` via SSE or webhook push | `session/update` carrying `SessionUpdate` union (chunks, tool_call, plan, available_commands_update, current_mode_update, etc.) | A2A push allows fully async; ACP requires open connection |
| **Client-side methods** | None (peer-to-peer is symmetric) | Rich: `fs/read_text_file`, `fs/write_text_file`, `terminal/{create,output,release,wait_for_exit,kill}`, `session/request_permission` | **This is ACP's single biggest unique value** — A2A has no concept of "agent asks editor to read/write a file" |
| **Long-running tasks** | Native via push webhooks + state transitions | None — sessions are interactive; long work via streaming chunks within a connection | A2A is the right shape for hour-scale autonomous work |
| **Governance** | Linux Foundation (since June 2025) | Community, led by Zed | A2A has institutional backing; ACP has reference-implementation backing |

### 4.1 The 3 biggest concept-level differences (extracted from the matrix)

1. **A2A's `Task` state machine vs. ACP's interactive `Session`/`PromptTurn`.** A2A's task can be paused, asynchronously updated via webhooks, and resumed across days. ACP sessions assume the client process is alive throughout the work. For SPUR's brain-orchestrating-workers pattern (where workers may run for hours), A2A's task model is structurally better.
2. **A2A has no client-side methods.** Symmetric peer-to-peer means there's no `fs/read` from one peer's perspective on another peer's files. ACP's `fs/*` and `terminal/*` are the editor giving the agent access to its environment — fundamentally different model. SPUR cannot replace ACP with A2A for the editor-integration path without losing this.
3. **A2A's discovery is HTTP-native** (`/.well-known/agent-card.json`); ACP's is local-process-native. A2A is built for "agents on the public internet"; ACP is built for "agent running locally as my subprocess." SPUR runs workers locally today, but a future "SPUR connects to a hosted Claude or hosted GPT-5" scenario fits A2A's shape much better.

## 5. Reconciliation patterns evaluated

### 5.1 R1 — ACP-only (status quo)

| Axis | Assessment |
|---|---|
| Scope | Zero new code |
| Blast radius | None |
| What becomes possible | — |
| What becomes harder | Cannot interoperate with the growing A2A ecosystem (50+ partners). Worker↔worker delegation stays ad-hoc. Long-running autonomous tasks (hours) are awkward in the session model. |
| Cost / risk / value | 0 / 0 / 0 |

**Verdict:** acceptable as a default while v1+v2 ship; not a long-term position.

### 5.2 R2 — Dual-stack (ACP for verticals, A2A for peers)

| Axis | Assessment |
|---|---|
| Scope | Add A2A client and server runtimes alongside ACP. New `crates/spur-a2a/` crate mirroring `crates/spur-acp/`. New connection types in orchestrator. Updated worker discovery. |
| Blast radius | Medium-large. Touches connection layer, orchestrator, registry. Two protocols to maintain forever. |
| What becomes possible | SPUR can both consume A2A agents (outbound) and be consumed as one (inbound). Brain↔worker and worker↔worker paths formalized. Hosted-agent integration (GPT-5 over the network) becomes natural. |
| What becomes harder | Two protocols' worth of message-shape evolution to track. Two notification pumps. Type-mapping between ACP `SessionUpdate` and A2A `TaskStatusUpdateEvent`. Risk of architectural drift between the two stacks. |
| Cost / risk / value | High / Medium / High |

**Verdict:** the right destination eventually, but a big bet to commit to today without concrete demand.

### 5.3 R3 — A2A bridge (translator inside spur-acp)

| Axis | Assessment |
|---|---|
| Scope | A translator module that maps incoming A2A connections to internal ACP sessions (or vice versa). Lives inside `spur-acp` as an adapter; reuses existing ACP session machinery. |
| Blast radius | Small-medium. New adapter module; minimal change to orchestrator (translator presents an `AgentConnection`-shaped surface). |
| What becomes possible | SPUR can speak A2A externally without a parallel internal stack. Outbound: brain delegates a task to an external A2A agent — the bridge translates `delegate_to_worker` calls into A2A `SendMessage`. Inbound: A2A agents send tasks to SPUR — bridge translates to `session/new` + `session/prompt`. |
| What becomes harder | The mapping between A2A's `Task` state machine and ACP's `Session`/`PromptTurn` is semi-lossy (e.g. `INPUT_REQUIRED` has no clean ACP analogue; `request_permission` has no A2A analogue). Edge cases need explicit policy. |
| Cost / risk / value | Medium / Medium / Medium-high |

**Verdict:** the right next step after v1+v2 ship. Practical, scoped, validates demand before committing to R2.

### 5.4 R3 sub-variant — outbound-only (recommended)

Restrict the bridge to **SPUR-as-A2A-client** for v3:

- SPUR initiates A2A `SendMessage` calls to external agents.
- SPUR consumes `TaskStatusUpdateEvent` notifications and rolls them up into the internal session-update stream.
- SPUR does **not** expose itself as an A2A server (no inbound; defer until requested).

This narrowing eliminates the hardest mapping problem (A2A `request_permission` ⇒ ACP `request_permission` is fine; the reverse doesn't fit cleanly). It also delivers the highest-value capability first: integration with hosted/external A2A agents.

## 6. Recommendation for SPUR

**Adopt R3 outbound-only as the v3 architectural target**, with two prep steps usable independently of any A2A integration:

### 6.1 Prep step A — Adopt the AgentCard pattern internally

Before writing any A2A wire code, restructure SPUR's worker registry to use an AgentCard-shaped descriptor:

```jsonc
// spur-internal AgentCard (no A2A wire — yet)
{
  "name": "codex",
  "version": "0.4.5",
  "description": "Generalist coding agent; strong at greenfield + refactors.",
  "skills": [{"name": "rust-refactor", "description": "..."}, ...],
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionHistory": false
  },
  "defaultInputModes": ["text/plain"],
  "defaultOutputModes": ["text/plain", "application/x-diff"]
}
```

Today this lives implicitly in `list_available_workers`'s response. Formalising it as a typed `AgentCard` struct (vendor-neutral, reusing A2A's exact field names where sensible) means future A2A wire integration is a serialization change, not a refactor.

**Effort:** small — type extraction + getter additions. ~100 LOC + tests.

### 6.2 Prep step B — Decouple delegation from in-process plumbing

Brain↔worker dispatches today are tightly coupled to the orchestrator's task channel. Adding a level of indirection (`DelegationTransport` trait with one impl for in-process and a stub for "future A2A") clarifies the seam.

**Effort:** medium. Trait definition + in-process impl. ~150-300 LOC. No behaviour change.

### 6.3 Step C — Implement the outbound A2A bridge

After v1+v2 land and prep steps A + B are in place, implement the outbound-only A2A bridge:

- New `crates/spur-acp/src/adapter/a2a_outbound.rs`.
- Implements `AgentConnection` for an external A2A agent identified by AgentCard URL.
- Maps `prompt` → A2A `SendMessage`; consumes `TaskStatusUpdateEvent` SSE stream and rewrites as `SessionNotification`.
- Limited to the subset of A2A features SPUR's ACP-shaped interfaces can express.

**Effort:** medium-large. New adapter, new transport, new auth handling. ~800-1500 LOC + tests.

## 7. Implications for v1 and v2

**None.** Both v1 (`/model`, `/effort` config-options pickers) and v2 (ACP-first arg pickers via `AvailableCommand.input` + `_meta`) target the editor↔agent (ACP) path, which stays as SPUR's primary surface in all three reconciliation options (R1, R2, R3). Ship v1+v2 on schedule; A2A work begins after.

The only adjustment v1+v2 *might* benefit from: when defining new types (e.g. `AdvertisedCommand` in v1, `ArgPickerSpec` in v2), choose names and shapes that won't collide with future A2A-derived types (`AgentCard`, `AgentSkill`, etc.). Current names are clear of that namespace. ✓

## 8. Open questions

- **When is the right moment to commit to R3?** Suggested trigger: when a user concretely asks SPUR to integrate a hosted/external agent that doesn't speak ACP, OR when the worker↔worker dependency tracking grows beyond the current ad-hoc message-passing model.
- **Should SPUR contribute to ACP or A2A standards?** Both would benefit from SPUR's multi-agent control-plane experience. ACP is the closer fit (SPUR is built on it); A2A is the higher-leverage fit (SPUR's worker pattern is closer to A2A's design center).
- **MCP exposure?** SPUR's tool ecosystem is currently MCP-mediated via agent integrations. No direct MCP code in SPUR. Likely stays this way unless SPUR offers tools *to* external agents.

## 9. Action items (for tracking; not in v1/v2 scope)

1. **Prep step A — internal AgentCard**: extract worker descriptors into a typed `AgentCard` struct in `spur-mcp` (where `list_available_workers` lives). Vendor-neutral, A2A-shape-compatible. ~100 LOC.
2. **Prep step B — DelegationTransport trait**: define the trait, port `delegate_to_worker` to use it via an in-process impl. No behaviour change. ~150-300 LOC.
3. **Track ACP and A2A spec evolution**: subscribe to release feeds for both; flag any A2A schema changes that affect AgentCard compatibility.
4. **Defer R3 implementation** until concrete demand surfaces.

## 10. Sources cited

- A2A spec & docs: `https://a2aproject.dev/`, Linux Foundation A2A announcements, A2A GitHub spec repo (gemini and kimi worker outputs)
- ACP spec & docs: `https://agentclientprotocol.com/`, agent-client-protocol GitHub repo
- MCP spec & docs: Anthropic MCP documentation (referenced for layer comparison only)
- SPUR internal: `crates/spur-acp/`, `crates/spur-mcp/`, v1 and v2 design specs (this directory)
- Worker delegation IDs: `5f858efe-e5b7-4a77-bc3c-b40598cd570d` (gemini), `1aedea90-4e99-4592-a368-e4726ca8899d` (kimi)

## 11. Appendix — research dispatch hygiene notes

For future research-style worker delegations:

- **gemini** is good at exploratory analysis but produces extensive thinking-trace output; the actionable deliverable is typically buried at the end. Subagent summarization is reliable. For shorter outputs, consider tighter "max-words" constraint + explicit "no thinking trace, deliverable only" instruction.
- **kimi** has had two issues this session: a stash-snapshot infrastructure failure (since resolved) and a fabricated file-write claim. For tasks requiring file artifacts, prefer **codex** (single-file edits is its `good_for`) or **claude-code** (multi-file with rationale). Reserve kimi for inline-output research where deliverables can be extracted from the response text.
