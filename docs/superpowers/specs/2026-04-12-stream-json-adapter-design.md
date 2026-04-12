# StreamJsonAdapter: Claude Code Integration via stream-json Protocol

## Problem

SPUR needs to communicate with Claude Code as a brain agent. Claude Code does not speak ACP — it speaks its own stream-json protocol over stdin/stdout. None of the three existing adapters (NativeAcpConnection, CliWrapAdapter, StdioAdapter) can parse Claude Code's structured JSON events.

## Verified Protocol Details

### Claude Code CLI Flags (verified from `claude --help` v2.1.104)

Required flags for bidirectional streaming:
```
-p                              Non-interactive mode (required)
--input-format stream-json      Accept JSON messages on stdin (bidirectional)
--output-format stream-json     Emit structured JSON events on stdout
--verbose                       Required by stream-json output
--bare                          Skip hooks, plugins, CLAUDE.md auto-discovery
--include-partial-messages      Stream partial message chunks as they arrive
```

Constraint discovered by live testing: `--output-format stream-json` **requires** `--verbose` — errors without it.

### Stream-json Event Format (verified from live test)

**Output events (stdout, one JSON object per line):**

```jsonc
// Event 1: System init (emitted once at startup)
{"type":"system","subtype":"init","session_id":"uuid","model":"claude-sonnet-4-6",
 "tools":["Bash","Edit","Read"],"permissionMode":"acceptEdits",
 "claude_code_version":"2.1.104",...}

// Event 2: Assistant message (per response, possibly many with --include-partial-messages)
{"type":"assistant","message":{"id":"uuid","role":"assistant",
 "content":[{"type":"text","text":"..."}],
 "usage":{"input_tokens":N,"output_tokens":N}},
 "session_id":"..."}

// Event 3: Result (end of turn)
{"type":"result","subtype":"success","is_error":false,"duration_ms":N,
 "result":"final text","total_cost_usd":0.05,"session_id":"...",
 "usage":{"input_tokens":N,"output_tokens":N,...}}
```

With `--verbose`, additional event types are expected for tool use, thinking, and errors.

**Input events (stdin, when `--input-format=stream-json`):**

```jsonc
// User message
{"type":"user","content":"fix the bug in auth.rs"}
```

### Key Behavioral Notes

- Bidirectional mode (`--input-format stream-json` + `--output-format stream-json`) keeps a single persistent process for all turns. Multi-turn is implicit — no `--resume` needed.
- `--bare` with `ANTHROPIC_API_KEY` env var or user's existing auth. SPUR never touches credentials.
- `--mcp-config <path>` can inject SPUR's MCP server for delegation tools at spawn time.
- `--max-budget-usd <amount>` caps cost per invocation.
- `--permission-mode <mode>` controls permission behavior (default, acceptEdits, bypassPermissions, dontAsk).

## Design

### Architecture

```
SPUR Orchestrator
    │
    │ calls AgentConnection::prompt()
    ▼
StreamJsonAdapter (implements AgentConnection trait)
    │
    │ spawns persistent process, reads NDJSON stdout, writes JSON stdin
    ▼
claude -p --input-format stream-json --output-format stream-json
        --verbose --bare --include-partial-messages
```

The adapter implements the existing `AgentConnection` trait — no changes to the orchestrator or other adapters.

### File Structure

```
crates/spur-acp/src/
├── protocol/
│   ├── mod.rs                      # module declaration
│   └── claude_events.rs            # serde types + parse_event()
└── connection/
    └── stream_json_adapter.rs      # AgentConnection impl
```

### Module 1: `protocol/claude_events.rs` (~100 LOC)

Serde types for Claude's stream-json event format:

```rust
use serde::Deserialize;

/// Top-level event from Claude Code's stream-json output.
/// Each line of stdout is one of these.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeEvent {
    #[serde(rename = "system")]
    System(SystemEvent),

    #[serde(rename = "assistant")]
    Assistant(AssistantEvent),

    #[serde(rename = "result")]
    Result(ResultEvent),
}

#[derive(Debug, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    pub session_id: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub is_error: Option<bool>,
    pub duration_ms: Option<u64>,
    pub result: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub session_id: String,
    pub usage: Option<Usage>,
}

/// User message sent to Claude via stdin.
#[derive(Debug, serde::Serialize)]
pub struct UserMessage {
    #[serde(rename = "type")]
    pub msg_type: String,  // always "user"
    pub content: String,
}

/// Parse a single line of stream-json output.
pub fn parse_event(line: &str) -> Result<ClaudeEvent, serde_json::Error> {
    serde_json::from_str(line)
}
```

### Module 2: `connection/stream_json_adapter.rs` (~300 LOC)

Implements `AgentConnection` using the protocol parser:

**Lifecycle:**

| AgentConnection method | StreamJsonAdapter behavior |
|---|---|
| `initialize()` | Spawn `claude -p --input-format stream-json --output-format stream-json --verbose --bare --include-partial-messages [--mcp-config path]`. Read the first stdout line — expect `ClaudeEvent::System` with session_id and model. Store session metadata. |
| `new_session()` | Return stored session_id from init event. The process IS the session. |
| `prompt()` | Write `{"type":"user","content":"..."}` + newline to stdin. Spawn background task reading stdout NDJSON. Map each `ClaudeEvent` to `SessionNotification`. Stream ends on `ClaudeEvent::Result`. Store cost from result event. |
| `cancel()` | Send SIGTERM to child process. |
| `shutdown()` | Close stdin. Wait up to 3 seconds. SIGKILL if needed. (Same pattern as StdioAdapter.) |
| `health()` | Check child process alive. Return cached AgentHealth. |

**Event mapping in `prompt()`:**

```rust
match parse_event(&line) {
    Ok(ClaudeEvent::Assistant(evt)) => {
        for block in &evt.message.content {
            match block {
                ContentBlock::Text { text } => {
                    // Emit SessionUpdate::AgentMessageChunk with text
                    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                    let notif = SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(chunk),
                    );
                    tx.send(notif).await;
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    // Phase 2: Emit as ToolCallStart if ACP SDK supports it
                    // Phase 1: Emit as text "[Tool: {name}] {input}"
                }
                ContentBlock::ToolResult { content, .. } => {
                    // Phase 2: Emit as ToolCallEnd
                    // Phase 1: Emit as text "[Result] {content}"
                }
            }
        }
    }
    Ok(ClaudeEvent::Result(evt)) => {
        // Store cost for later retrieval
        if let Some(cost) = evt.total_cost_usd {
            cost_store.store(cost, Ordering::Relaxed);
        }
        // Emit completion notification (stream ends after this)
        break;
    }
    Ok(ClaudeEvent::System(_)) => {
        // Unexpected mid-stream init — log and skip
    }
    Err(e) => {
        // Unknown event type — log at debug level, don't crash
        tracing::debug!("StreamJsonAdapter: unparseable event: {e}");
    }
}
```

### Agent Config Schema

```toml
[[agents]]
name = "claude-code"
transport = "stream-json"
command = "claude"
args = [
    "-p",
    "--input-format", "stream-json",
    "--output-format", "stream-json",
    "--verbose",
    "--bare",
    "--include-partial-messages",
]
capabilities = ["architecture", "refactoring", "debugging", "code-review"]
cost_tier = "high"
auth = "user-managed"

# Optional overrides (injected as additional args by the adapter):
# mcp_config = "/path/to/spur-mcp-config.json"
# permission_mode = "default"
# max_budget_usd = 5.0
# model = "opus"
```

The `transport` field determines which adapter to instantiate:
- `"native"` → NativeAcpConnection
- `"cli-wrap"` → CliWrapAdapter
- `"stdio"` → StdioAdapter
- `"stream-json"` → StreamJsonAdapter (new)

### Phased Delivery

| Phase | Scope | LOC | Enables |
|---|---|---|---|
| 1 | Basic text streaming + cost extraction + session lifecycle | ~200 | Claude works as brain, cost visible in TUI |
| 2 | Rich events: ToolCallStart/End mapped from tool_use blocks | ~100 | Full ReAct trace (Act/Observe) in TUI |
| 3 | MCP config injection + permission passthrough + model override | ~100 | Delegation via MCP tools, permission UX, model selection |

### Why Not Other Approaches

| Approach | Verdict | Reason |
|---|---|---|
| Generic NDJSON adapter with config DSL | Rejected | YAGNI — no other agent uses this format. Premature abstraction. |
| Separate ACP bridge process | Deferred | Parser module can be extracted later if needed. Extra process adds complexity today. |
| Native in orchestrator | Rejected | Violates AgentConnection abstraction. Creates vendor lock-in. |

## Files to Create/Modify

| File | Action |
|---|---|
| `crates/spur-acp/src/protocol/mod.rs` | Create: module declaration |
| `crates/spur-acp/src/protocol/claude_events.rs` | Create: serde types + parser |
| `crates/spur-acp/src/connection/stream_json_adapter.rs` | Create: AgentConnection impl |
| `crates/spur-acp/src/connection/mod.rs` | Modify: add `pub mod stream_json_adapter` |
| `crates/spur-acp/src/lib.rs` | Modify: add `pub mod protocol` |
| `crates/spur-acp/src/config.rs` | Modify: add `"stream-json"` transport variant |

## Success Criteria

- `StreamJsonAdapter` implements `AgentConnection` trait fully
- Claude Code spawns with correct flags and establishes bidirectional communication
- Text responses stream to TUI as `AgentMessageChunk` events
- Cost is extracted from result events and available to the cost tracker
- Multi-turn works without process restart (send new user message on stdin, get new response)
- Unknown/unparseable events are logged but don't crash the adapter
- Process lifecycle (spawn, cancel, shutdown) follows the same pattern as StdioAdapter
