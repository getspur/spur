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

// Event 2: Assistant message (per response, many with --include-partial-messages)
{"type":"assistant","message":{"id":"uuid","role":"assistant",
 "content":[
   {"type":"text","text":"Let me look at..."},
   {"type":"tool_use","id":"tu_01","name":"Bash","input":{"command":"ls"}},
   {"type":"thinking","thinking":"I need to check..."}
 ],
 "usage":{"input_tokens":N,"output_tokens":N}},
 "session_id":"..."}

// Event 3: Result (end of turn)
{"type":"result","subtype":"success","is_error":false,"duration_ms":N,
 "result":"final text","total_cost_usd":0.05,"session_id":"...",
 "usage":{"input_tokens":N,"output_tokens":N,...}}
```

**Input events (stdin, when `--input-format=stream-json`):**

```jsonc
// User message
{"type":"user","content":"fix the bug in auth.rs"}
```

### Key Behavioral Notes

- Bidirectional mode keeps a single persistent process for all turns. Multi-turn is implicit — no `--resume` needed.
- `--bare` with `ANTHROPIC_API_KEY` env var or user's existing auth. SPUR never touches credentials.
- `--mcp-config <path>` injects SPUR's MCP server for delegation tools at spawn time.
- `--max-budget-usd <amount>` caps cost per invocation.
- `--permission-mode <mode>` controls permission behavior.
- `--system-prompt <prompt>` injects a system prompt at spawn time.

## Design

### Architecture

```
Orchestrator (unchanged)
    │ calls AgentConnection::prompt()
    │ receives Stream<Item=SessionNotification>
    │ emits SpurEvent::AgentNotification { notification }
    ▼
StreamJsonAdapter (implements AgentConnection trait)
    │
    │ prompt(): creates (tx, rx) channel per turn
    │           sends tx to background reader
    │           writes JSON user message to stdin
    │           returns rx as Stream
    │
    ├── Background Reader Task (persistent, spawned at initialize)
    │   │ reads stdout NDJSON lines continuously
    │   │ parses each line → ClaudeEvent
    │   │ maps content blocks → ACP SDK SessionNotification types
    │   │ sends to current turn's tx
    │   │ on Result event: drops tx (stream ends), waits for next
    │   ▼
    │
    └── claude -p --input-format stream-json --output-format stream-json
              --verbose --bare --include-partial-messages
```

### Critical Design Decision: Uses Real ACP SDK Types

The TUI (`session_detail.rs:241-293`) pattern-matches DIRECTLY on ACP SDK `SessionUpdate` variants:
```rust
match &notification.update {
    SessionUpdate::AgentThoughtChunk(chunk) → react_trace.append_think()
    SessionUpdate::AgentMessageChunk(chunk) → react_trace.append_message()
    SessionUpdate::ToolCall(tc) → react_trace.push(TraceKind::Act { tool: tc.title })
    SessionUpdate::ToolCallUpdate(tcu) → react_trace.push(TraceKind::Observe { text: tcu.fields.raw_output })
    SessionUpdate::Plan(plan) → react_trace.push(formatted plan)
}
```

The adapter MUST produce these REAL types — not custom intermediates. This means the full ReAct trace works from day one without TUI changes.

### Critical Design Decision: Channel-of-Senders Pattern

Borrowed from `NativeAcpConnection` (`native.rs:284-289`): each `prompt()` call creates a fresh `mpsc::channel`, sends the sender to the background reader, and returns the receiver as a Stream. When the background reader sees a Result event, it drops the sender → channel closes → stream ends → orchestrator knows the turn is complete.

This solves multi-turn without `--resume` (expensive process respawn) or idle timeouts (unreliable).

### File Structure

```
crates/spur-acp/src/
├── protocol/
│   ├── mod.rs                      # module declaration
│   └── claude_events.rs            # serde types + parse + map to ACP types
└── connection/
    └── stream_json_adapter.rs      # AgentConnection impl + background reader
```

### Module 1: `protocol/claude_events.rs` (~120 LOC)

```rust
use serde::Deserialize;
use agent_client_protocol::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate, SessionId,
};

/// Top-level event from Claude Code's stream-json output.
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
}

#[derive(Debug, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<AssistantContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: serde_json::Value },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: serde_json::Value },
}

#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub is_error: Option<bool>,
    pub duration_ms: Option<u64>,
    pub result: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub session_id: String,
}

/// User message written to Claude's stdin.
#[derive(Debug, serde::Serialize)]
pub struct UserMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,  // always "user"
    pub content: &'a str,
}

/// Parse a single stdout line.
pub fn parse_event(line: &str) -> Result<ClaudeEvent, serde_json::Error> {
    serde_json::from_str(line)
}

/// Map a ClaudeEvent to zero or more ACP SessionNotifications.
pub fn map_to_notifications(
    event: &ClaudeEvent,
    session_id: &SessionId,
) -> Vec<SessionNotification> {
    match event {
        ClaudeEvent::Assistant(evt) => {
            evt.message.content.iter().filter_map(|block| {
                let update = match block {
                    AssistantContentBlock::Text { text } => {
                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text.clone())));
                        SessionUpdate::AgentMessageChunk(chunk)
                    }
                    AssistantContentBlock::Thinking { thinking } => {
                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(thinking.clone())));
                        SessionUpdate::AgentThoughtChunk(chunk)
                    }
                    AssistantContentBlock::ToolUse { name, input, .. } => {
                        // Construct AcpToolCall with title and raw_input
                        // (other fields use defaults — TUI only reads these two)
                        SessionUpdate::ToolCall(AcpToolCall::new(name.clone(), Some(input.clone())))
                    }
                    AssistantContentBlock::ToolResult { content, .. } => {
                        // Construct AcpToolCallUpdate with raw_output
                        SessionUpdate::ToolCallUpdate(AcpToolCallUpdate::new(Some(content.clone())))
                    }
                };
                Some(SessionNotification::new(session_id.clone(), update))
            }).collect()
        }
        ClaudeEvent::System(_) => vec![],  // internal, not forwarded
        ClaudeEvent::Result(_) => vec![],  // handled by reader loop (ends stream)
    }
}
```

### Module 2: `connection/stream_json_adapter.rs` (~280 LOC)

```rust
pub struct StreamJsonAdapter {
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    child: Option<Child>,
    stdin: Option<BufWriter<ChildStdin>>,
    /// Send new per-turn senders to the background reader.
    sender_tx: Option<mpsc::Sender<mpsc::Sender<SessionNotification>>>,
    /// Session ID from the system/init event.
    session_id: Option<SessionId>,
    /// Model from the system/init event.
    model: Option<String>,
    /// Cumulative cost across all turns.
    total_cost: Arc<Mutex<f64>>,
    health_status: AgentHealth,
}
```

**Lifecycle:**

| Method | Behavior |
|---|---|
| `initialize()` | Spawn claude with args. Read first stdout line → expect system/init → store session_id and model. Create sender channel. Spawn background reader task. |
| `new_session()` | Return stored session_id. The process IS the session. |
| `prompt(request)` | Extract text from content blocks. Create `(notif_tx, notif_rx)`. Send `notif_tx` to background reader via sender channel. Write `{"type":"user","content":"..."}\n` to stdin. Return `notif_rx` wrapped as Stream via `unfold()`. |
| `cancel()` | SIGTERM child process. |
| `shutdown()` | Close stdin. Wait 3s. SIGKILL if needed. Drop sender channel (background reader exits). |
| `health()` | Check `child.id()`. Return cached AgentHealth. |

**Background reader task (spawned once in `initialize()`):**

```rust
async fn reader_loop(
    stdout: BufReader<ChildStdout>,
    mut sender_rx: mpsc::Receiver<mpsc::Sender<SessionNotification>>,
    session_id: SessionId,
    cost: Arc<Mutex<f64>>,
    agent_name: String,
) {
    let mut lines = stdout.lines();
    let mut current_tx: Option<mpsc::Sender<SessionNotification>> = None;

    loop {
        // If no current sender, wait for one from prompt()
        if current_tx.is_none() {
            match sender_rx.recv().await {
                Some(tx) => current_tx = Some(tx),
                None => break,  // adapter shut down
            }
        }

        // Read next line from stdout
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,  // stdout EOF, process died
            Err(e) => {
                tracing::error!(agent = %agent_name, "stdout read error: {e}");
                break;
            }
        };

        // Parse event
        let event = match parse_event(&line) {
            Ok(e) => e,
            Err(_) => {
                tracing::debug!(agent = %agent_name, "unparseable event, skipping");
                continue;
            }
        };

        // Handle result (end of turn)
        if let ClaudeEvent::Result(ref r) = event {
            if let Some(c) = r.total_cost_usd {
                *cost.lock().unwrap() += c;
            }
            // Drop sender → stream ends → orchestrator sees turn complete
            current_tx = None;
            continue;
        }

        // Map to notifications and send
        if let Some(ref tx) = current_tx {
            for notif in map_to_notifications(&event, &session_id) {
                if tx.send(notif).await.is_err() {
                    break;  // receiver dropped
                }
            }
        }
    }
}
```

### Agent Config

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
    "--permission-mode", "acceptEdits",
]
capabilities = ["architecture", "refactoring", "debugging", "code-review"]
cost_tier = "high"
auth = "user-managed"
# Optional:
# system_prompt = "You are a brain agent..."
# mcp_config = ".spur/mcp-brain.json"
# max_budget_usd = 10.0
# model = "opus"
```

The `transport` field determines which adapter the registry instantiates:
- `"native"` → NativeAcpConnection
- `"cli-wrap"` → CliWrapAdapter
- `"stdio"` → StdioAdapter
- `"stream-json"` → StreamJsonAdapter (new)

### Files to Create/Modify

| File | Action |
|---|---|
| `crates/spur-acp/src/protocol/mod.rs` | Create: `pub mod claude_events;` |
| `crates/spur-acp/src/protocol/claude_events.rs` | Create: serde types + parse + map |
| `crates/spur-acp/src/connection/stream_json_adapter.rs` | Create: AgentConnection impl |
| `crates/spur-acp/src/connection/mod.rs` | Modify: add `pub mod stream_json_adapter; pub use stream_json_adapter::StreamJsonAdapter;` |
| `crates/spur-acp/src/lib.rs` | Modify: add `pub mod protocol;` and re-export StreamJsonAdapter |
| `crates/spur-acp/src/config.rs` | Modify: add `"stream-json"` transport variant to config parsing |

### Error Handling

| Scenario | Handling |
|---|---|
| Claude crashes mid-turn | stdout EOF → reader exits → tx dropped → stream ends → orchestrator emits BrainError |
| stdin write fails | prompt() returns Err → orchestrator handles |
| Unparseable JSON line | log at debug, skip line, continue reading |
| First event not system/init | initialize() returns Err |
| Events between turns (no sender) | Dropped (acceptable: diagnostic/cleanup events) |
| Sender channel closed | Reader exits cleanly (adapter shutting down) |

### Success Criteria

- StreamJsonAdapter implements AgentConnection trait fully
- Claude Code spawns and establishes bidirectional JSON communication
- Text responses appear in TUI as agent messages (TraceKind::AgentMessage)
- Thinking text appears as TraceKind::Think
- Tool calls appear as TraceKind::Act with tool name and args
- Tool results appear as TraceKind::Observe with output
- Cost is extracted from result events and tracked
- Multi-turn works: second prompt() on same process produces new response stream
- Process lifecycle (cancel, shutdown) is clean
- Unknown event types don't crash the adapter
