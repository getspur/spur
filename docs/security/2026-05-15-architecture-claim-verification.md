# SPUR Architecture Claim Verification (2026-05-15)

Scope: read-only verification across `crates/` (+ `docs/PRIVACY.md` for documented telemetry controls), excluding `resource/`, `resources/landing/`, `.opencode/`, and vendored `node_modules/`.

## Claim 1
**Claim:** SPUR does NOT directly call any LLM provider HTTP API. All prompts flow via ACP IPC to local agent processes (Claude Code, Gemini CLI, Codex, etc.) installed/configured independently.

**Verdict:** **TRUE**

**Evidence**

1) Prompt path is ACP/session IPC, not provider HTTP calls:

- `crates/spur-core/src/orchestrator/interactive_loop.rs:1213`
```rust
let _turn_guard = TurnGuard::arm(scheduler.turn_flag());
let prompt_started_at = std::time::Instant::now();
let mut stream = match b.connection.prompt(prompt_request).await {
    Ok(s) => {
```

- `crates/spur-acp/src/connection/mod.rs:66-69`
```rust
/// 1. `initialize()` -- negotiate protocol version and capabilities.
/// 2. `new_session()` -- create a working session (with cwd + MCP servers).
/// 3. `prompt()` -- send messages and stream back notifications.
/// 4. `cancel()` -- cancel an in-flight prompt for a session.
```

2) Native ACP transport is subprocess + stdio IPC:

- `crates/spur-acp/src/connection/native.rs:169-170`
```rust
/// This is the "real" ACP implementation that spawns an agent subprocess and
/// communicates via the Agent Client Protocol over stdio.
```

3) Agent wiring is to local commands/binaries (user-installed):

- `crates/spur-acp/src/seed_agents.toml:3-5`
```toml
# `spur init` parses this, scans $PATH for each `command`, and registers
# matching entries into the in-memory AgentRegistry. The CLI then writes
# that registry to .spur/config.toml as the user's starting config.
```

- `crates/spur-acp/src/seed_agents.toml:81-85`
```toml
name = "codex"
command = "npx"
args = ["--yes", "@zed-industries/codex-acp@0.14.0"]
transport = "acp"
kind = "codex-acp"
```

4) Connection factory uses ACP/stdio/CLI transports only (no provider-specific HTTP client path):

- `crates/spur-core/src/orchestrator/connection.rs:410-418`
```rust
match config.transport {
    TransportKind::Acp => Box::new(NativeAcpConnection::new_with_kind(
        config.name.clone(),
        config.command.clone(),
        spawn_args,
        config.kind,
        permission_tx,
    )),
```

5) Targeted provider-domain scan (`api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com`, `api.cohere.ai`, `api.mistral.ai`, `api.groq.com`, `api.together.xyz`) returned no hits in `crates/` runtime code.

6) HTTP egress found for telemetry endpoint (non-prompt path):

- `crates/spur-telemetry/src/client.rs:8`
```rust
const DEFAULT_POSTHOG_ENDPOINT: &str = "https://us.i.posthog.com";
```

**Recommended PDF wording**
SPUR does not directly call first-party LLM provider HTTPS endpoints for prompt execution. Prompt traffic is brokered to user-installed local agent runtimes over ACP/IPC (subprocess + stdio/session RPC). Separate non-prompt outbound integrations (for example telemetry or optional integrations) may still perform HTTPS requests.

## Claim 2
**Claim:** SPUR's MCP server binds to loopback only (127.0.0.1 / localhost / unix socket), never to 0.0.0.0 or externally-routable interfaces.

**Verdict:** **TRUE**

**Evidence**

1) Main MCP callback server bind site is hard-coded loopback:

- `crates/spur-mcp/src/server/mod.rs:575-577`
```rust
/// Start listening on a random localhost port.
///
/// Returns the MCP endpoint URL (e.g. `http://127.0.0.1:12345/mcp`) and
```

- `crates/spur-mcp/src/server/mod.rs:606-607`
```rust
let listener = TcpListener::bind("127.0.0.1:0")
    .await
```

2) Worker MCP server bind site is also hard-coded loopback:

- `crates/spur-mcp/src/worker_server.rs:435-436`
```rust
/// Bind a fresh TCP listener on `127.0.0.1:0`, generate an in-process HMAC
/// key from the OS RNG, and spawn the accept loop.
```

- `crates/spur-mcp/src/worker_server.rs:454`
```rust
let listener = TcpListener::bind("127.0.0.1:0").await?;
```

3) No host-bind config/env override was found at these bind sites; only unrelated session keepalive env exists (`SPUR_MCP_SESSION_KEEPALIVE_SECS`).

**Recommended PDF wording**
SPUR MCP endpoints bind to loopback-only TCP listeners (`127.0.0.1` on ephemeral ports) and do not expose a `0.0.0.0`/public-interface bind mode in current server startup paths.

## Claim 3
**Claim:** SPUR's anonymous telemetry can be disabled by a documented environment variable (or config flag), and payload schema excludes prompt text and source-code content. Integration point: commit `0286a60e`.

**Verdict:** **TRUE**

**Evidence**

1) Documented env-var disable is explicit:

- `docs/PRIVACY.md:54-60`
```md
## Disable Telemetry

Environment disable (session/runtime):
SPUR_TELEMETRY=0 spur
```

2) Runtime enforcement of env-var disable:

- `crates/spur-telemetry/src/consent.rs:51-56`
```rust
fn telemetry_env_disables() -> bool {
    match env::var("SPUR_TELEMETRY") {
        Ok(value) => {
            let value = value.trim();
            value.is_empty() || value == "0"
```

- `crates/spur-telemetry/src/consent.rs:30-33`
```rust
pub fn resolve(cfg: &TelemetryConfig) -> Consent {
    if telemetry_env_disables() || ci_env_disables() {
        return Consent::none();
```

3) Config-flag path exists (`tier1_crash`, `tier1_perf`, `tier2_usage`):

- `crates/spur-telemetry/src/config.rs:13-16`
```rust
pub struct TelemetryConfig {
    pub anonymous_id: Uuid,
    pub tier1_crash: bool,
    pub tier1_perf: bool,
    pub tier2_usage: bool,
```

4) Telemetry event schemas are strongly-typed and do not include prompt/source-content fields:

- `crates/spur-telemetry/src/tier1_events.rs:102-107`
```rust
pub struct LlmRequestDuration {
    pub model_name: ModelName,
    pub duration_ms: u64,
    pub token_count_bucket: u32,
    pub outcome: Outcome,
}
```

- `crates/spur-telemetry/src/tier2_events.rs:187-191`
```rust
pub struct McpToolCalled {
    pub server_name: McpServerName,
    pub tool_name: McpToolName,
    pub outcome: crate::tier1_events::Outcome,
}
```

- `crates/spur-telemetry/src/lib.rs:159-163`
```rust
let mut props = serde_json::Map::new();
props.insert("spur_version".to_string(), state.spur_version.into());
for (key, value) in event.into_props() {
    props.insert(key.to_string(), value);
}
```

5) Crash telemetry similarly emits metadata/hash/sanitized stack, not prompt or source text blobs:

- `crates/spur-telemetry/src/crash.rs:121-128`
```rust
properties: json!({
    "panic_type": report.panic_type,
    "payload_hash": report.payload_hash,
    "sanitized_stack": report.sanitized_stack,
    "crate": report.crate_,
    "module": report.module,
    "line": report.line,
}),
```

6) Integration point confirmation:

- `git show --stat 0286a60e` shows telemetry integration adding `crates/spur-telemetry/*`, CLI telemetry commands, and `docs/PRIVACY.md`.

**Recommended PDF wording**
SPUR telemetry supports explicit opt-out via `SPUR_TELEMETRY=0` and persisted per-scope toggles. Event schemas are constrained to operational metadata (for example durations, model class buckets, tool/server categories, outcomes) and do not serialize prompt bodies or source-code content.

## Summary Table

| Claim | Verdict | Notes |
|---|---|---|
| 1. No direct LLM provider HTTP API; prompt flow via ACP IPC | TRUE | No provider endpoint hits; prompt path is ACP/session IPC to local agent subprocesses. |
| 2. MCP binds loopback only | TRUE | Both MCP bind sites use `TcpListener::bind("127.0.0.1:0")`; no public bind path found. |
| 3. Telemetry disable + schema excludes prompt/source content | TRUE | `SPUR_TELEMETRY=0` documented + enforced; typed event payloads omit prompt/source text fields. |
