# ACP Vendor Meta Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Normalize vendor-specific ACP `_meta` extensions through a new `SpurToolMeta` struct in spur-acp, and close five session-detail rendering gaps using only normalized types — zero vendor tokens in spur-tui.

**Architecture:** Extend the existing `spur-acp::adapter` pattern (already used for `ObservePayload`, `ModeBadge`) with a new `extract_tool_meta(tc, kind) -> SpurToolMeta` dispatcher. Per-vendor modules read `tc.meta` JSON paths; spur-tui consumes the normalized struct. A CI grep-guard enforces that no vendor tokens leak into `crates/spur-tui/`.

**Tech Stack:** Rust, `agent-client-protocol` v0.10.4, `serde_json::Map`, ratatui (TUI), tokio, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-04-17-acp-vendor-meta-unification-design.md`

---

## File Structure

### Create

- `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_bash_with_meta.json` — claude fixture carrying `_meta.claudeCode.toolName`.
- `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_subagent_task.json` — parent/child Task fixture.
- `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_edit_with_diff.json` — inline diff content fixture.
- `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/user_message_chunk_replay.json` — Gap #1 fixture.
- `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_terminal.json` — Gap #3 terminal-content fixture.
- `crates/spur-acp/tests/tool_meta_extraction.rs` — new integration test file for `extract_tool_meta`.
- `scripts/check-no-vendor-meta-leak.sh` — grep-based guardrail.
- `.github/workflows/vendor-leak-check.yml` — CI job wrapping the script.
- `docs/spur/acp-meta-conventions.md` — convention reference doc.

### Modify

- `crates/spur-acp/src/adapter/mod.rs` — add `SpurToolMeta` struct, `extract_tool_meta` dispatcher.
- `crates/spur-acp/src/adapter/claude.rs` — add `extract_tool_meta` free function.
- `crates/spur-acp/src/adapter/codex.rs` — add stub `extract_tool_meta`.
- `crates/spur-acp/src/adapter/kiro.rs` — add stub `extract_tool_meta`.
- `crates/spur-acp/src/adapter/generic.rs` — no-op (Generic kind uses `SpurToolMeta::default()` inline in dispatcher).
- `crates/spur-acp/src/lib.rs` — re-export `SpurToolMeta`, `extract_tool_meta`.
- `crates/spur-tui/src/components/react_trace/mod.rs` — add `append_user_message`.
- `crates/spur-tui/src/views/session_detail.rs` — wire normalized meta, add `tool_depth`, fix Gaps #1-#5.

### Notes

- `AgentKind` has no `Gemini` variant — Gemini falls under `Generic`. No `adapter/gemini.rs` is created; the spec's "gemini stub" is served by Generic's inline default.
- `ToolCall.meta` is Rust-side name for `_meta` (serde-renamed via `#[serde(rename = "_meta")]`). Type is `Option<serde_json::Map<String, serde_json::Value>>`.
- `ToolCall` ID field is `.tool_call_id: ToolCallId`, NOT `.id`.
- Existing fixture directory pattern is `tests/fixtures/notifications/<agent-name>/*.json` (not `tests/adapter_fixtures/<vendor>/`). Follow the existing pattern.

---

## Task 1: Add `SpurToolMeta` struct + per-vendor extractors (infrastructure)

**Files:**
- Create: `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_bash_with_meta.json`
- Create: `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_subagent_task.json`
- Create: `crates/spur-acp/tests/tool_meta_extraction.rs`
- Modify: `crates/spur-acp/src/adapter/mod.rs`
- Modify: `crates/spur-acp/src/adapter/claude.rs`
- Modify: `crates/spur-acp/src/adapter/codex.rs`
- Modify: `crates/spur-acp/src/adapter/kiro.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Create the `tool_call_bash_with_meta.json` fixture**

Write this file exactly:

```json
{
  "sessionId": "sess-cc-meta-001",
  "update": {
    "sessionUpdate": "tool_call",
    "toolCallId": "tc-bash-meta-001",
    "title": "Bash",
    "kind": "execute",
    "rawInput": { "command": "ls" },
    "_meta": {
      "claudeCode": {
        "toolName": "Bash"
      }
    }
  }
}
```

- [ ] **Step 2: Create the `tool_call_subagent_task.json` fixture**

Write this file exactly:

```json
{
  "sessionId": "sess-cc-meta-001",
  "update": {
    "sessionUpdate": "tool_call",
    "toolCallId": "tc-subagent-edit-001",
    "title": "Edit",
    "kind": "edit",
    "rawInput": { "path": "src/foo.rs" },
    "_meta": {
      "claudeCode": {
        "toolName": "Edit",
        "parentToolUseId": "tc-task-parent-001"
      }
    }
  }
}
```

- [ ] **Step 3: Write the failing integration tests**

Create `crates/spur-acp/tests/tool_meta_extraction.rs`:

```rust
use agent_client_protocol::{SessionNotification, SessionUpdate};
use spur_acp::{adapter::extract_tool_meta, AgentKind};

fn load(rel: &str) -> SessionNotification {
    let path = format!(
        "{}/tests/fixtures/notifications/{}",
        env!("CARGO_MANIFEST_DIR"),
        rel
    );
    let json = std::fs::read_to_string(&path).expect("read fixture");
    serde_json::from_str(&json).expect("parse fixture")
}

#[test]
fn claude_extracts_tool_name_from_meta() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert_eq!(meta.tool_name.as_deref(), Some("Bash"));
    assert_eq!(meta.parent_tool_use_id, None);
}

#[test]
fn claude_extracts_parent_tool_use_id_from_meta() {
    let n = load("claude-code-acp/tool_call_subagent_task.json");
    let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert_eq!(meta.tool_name.as_deref(), Some("Edit"));
    assert_eq!(meta.parent_tool_use_id.as_deref(), Some("tc-task-parent-001"));
}

#[test]
fn claude_returns_default_when_meta_absent() {
    let n = load("claude-code-acp/tool_call_bash.json"); // existing fixture, no _meta
    let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
    let meta = extract_tool_meta(tc, AgentKind::ClaudeCodeAcp);
    assert!(meta.tool_name.is_none());
    assert!(meta.parent_tool_use_id.is_none());
}

#[test]
fn generic_kind_always_returns_default() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
    let meta = extract_tool_meta(tc, AgentKind::Generic);
    assert!(meta.tool_name.is_none());
    assert!(meta.parent_tool_use_id.is_none());
}

#[test]
fn codex_stub_returns_default() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
    let meta = extract_tool_meta(tc, AgentKind::CodexAcp);
    assert!(meta.tool_name.is_none());
}

#[test]
fn kiro_stub_returns_default() {
    let n = load("claude-code-acp/tool_call_bash_with_meta.json");
    let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
    let meta = extract_tool_meta(tc, AgentKind::Kiro);
    assert!(meta.tool_name.is_none());
}
```

- [ ] **Step 4: Run tests — verify they fail**

Run: `cargo test --package spur-acp --test tool_meta_extraction`
Expected: all 6 tests FAIL with "unresolved import `spur_acp::adapter::extract_tool_meta`" or similar compile error.

- [ ] **Step 5: Add `SpurToolMeta` struct and dispatcher to `adapter/mod.rs`**

Append to `crates/spur-acp/src/adapter/mod.rs` (after the existing `mode_badge` function, before any `#[cfg(test)]`):

```rust
/// Normalized view of vendor-specific `_meta` extensions on a ToolCall.
///
/// Fields are added ONLY when a concept is genuinely cross-vendor and NOT
/// already expressed by an ACP spec field. Adding a field is a design
/// change — see `docs/spur/acp-meta-conventions.md`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpurToolMeta {
    /// Vendor-specific tool identity (e.g. "Bash", "Edit", "/spec-init").
    /// Prefer this over `tc.title` for identity-sensitive rendering.
    pub tool_name: Option<String>,

    /// ID of the parent ToolCall when this call was spawned by a
    /// subagent / Task mechanism. Used for render indentation.
    pub parent_tool_use_id: Option<String>,
}

/// Extract a `SpurToolMeta` from a `ToolCall` using the vendor's
/// `_meta.<vendor>.*` convention. Returns default for unknown/absent meta.
pub fn extract_tool_meta(tc: &ToolCall, kind: AgentKind) -> SpurToolMeta {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => {
            claude::extract_tool_meta(tc)
        }
        AgentKind::CodexAcp => codex::extract_tool_meta(tc),
        AgentKind::Kiro => kiro::extract_tool_meta(tc),
        AgentKind::Generic => SpurToolMeta::default(),
    }
}
```

- [ ] **Step 6: Implement `claude::extract_tool_meta`**

Append to `crates/spur-acp/src/adapter/claude.rs`:

```rust
use agent_client_protocol::ToolCall;

/// Read `_meta.claudeCode.{toolName, parentToolUseId}` from a ToolCall.
/// Absent keys produce `None`; non-string values are treated as absent.
pub fn extract_tool_meta(tc: &ToolCall) -> super::SpurToolMeta {
    let cc = tc
        .meta
        .as_ref()
        .and_then(|m| m.get("claudeCode"));
    super::SpurToolMeta {
        tool_name: cc
            .and_then(|v| v.get("toolName"))
            .and_then(|v| v.as_str())
            .map(String::from),
        parent_tool_use_id: cc
            .and_then(|v| v.get("parentToolUseId"))
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}
```

- [ ] **Step 7: Add stub `extract_tool_meta` to codex and kiro**

Append to `crates/spur-acp/src/adapter/codex.rs`:

```rust
use agent_client_protocol::ToolCall;

/// Codex `_meta` extractor stub.
/// TODO(vendor-onboarding): replace with real extractor when codex emits
/// recognizable `_meta.codex.*` fields. See
/// docs/spur/acp-meta-conventions.md.
pub fn extract_tool_meta(_tc: &ToolCall) -> super::SpurToolMeta {
    super::SpurToolMeta::default()
}
```

Append to `crates/spur-acp/src/adapter/kiro.rs`:

```rust
use agent_client_protocol::ToolCall;

/// Kiro `_meta` extractor stub.
/// TODO(vendor-onboarding): replace with real extractor when kiro emits
/// recognizable `_meta.kiro.*` fields. See
/// docs/spur/acp-meta-conventions.md.
pub fn extract_tool_meta(_tc: &ToolCall) -> super::SpurToolMeta {
    super::SpurToolMeta::default()
}
```

- [ ] **Step 8: Re-export from lib.rs**

In `crates/spur-acp/src/lib.rs`, locate the existing `pub use agent_client_protocol::{...}` block around line 37-47. Do NOT modify it. Instead, add ONE new line below it:

```rust
pub use adapter::{extract_tool_meta, SpurToolMeta};
```

Place this line directly after the `pub use agent_client_protocol::{...}` block's closing `};`.

- [ ] **Step 9: Run tests — verify they pass**

Run: `cargo test --package spur-acp --test tool_meta_extraction`
Expected: all 6 tests PASS.

- [ ] **Step 10: Run full spur-acp test suite for regressions**

Run: `cargo test --package spur-acp`
Expected: all tests pass, including existing `adapter_fixtures`, `adapter_smoke`, `adapter_idempotence`.

- [ ] **Step 11: Commit**

```bash
git add crates/spur-acp/src/adapter/mod.rs \
        crates/spur-acp/src/adapter/claude.rs \
        crates/spur-acp/src/adapter/codex.rs \
        crates/spur-acp/src/adapter/kiro.rs \
        crates/spur-acp/src/lib.rs \
        crates/spur-acp/tests/tool_meta_extraction.rs \
        crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_bash_with_meta.json \
        crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_subagent_task.json
git commit -m "feat(spur-acp): add SpurToolMeta + per-vendor extract_tool_meta

Normalizes vendor _meta extensions into a cross-vendor struct.
Claude extractor reads _meta.claudeCode.{toolName, parentToolUseId};
codex/kiro return default stubs; Generic always default.

Refs: docs/superpowers/specs/2026-04-17-acp-vendor-meta-unification-design.md"
```

---

## Task 2: Wire `SpurToolMeta` into session_detail — close Gaps #4 and #5

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Locate the SessionDetailView struct definition**

Run: `rg -n "struct SessionDetailView" crates/spur-tui/src/views/session_detail.rs`
Note the struct body line range — you will add a field there.

- [ ] **Step 2: Add `tool_depth` field to SessionDetailView**

Inside the `SessionDetailView` struct body, add this field (place it near other map-like state fields):

```rust
    /// Maps ToolCall id -> render depth for subagent nesting.
    /// Populated on each ToolCall; read on subsequent ToolCalls to resolve
    /// the parent's depth. Capped at 8 to prevent runaway indentation.
    tool_depth: std::collections::HashMap<String, u8>,
```

- [ ] **Step 3: Initialize `tool_depth` in SessionDetailView constructors**

Run: `rg -n "fn new|SessionDetailView \{" crates/spur-tui/src/views/session_detail.rs` to find every SessionDetailView literal construction and every `fn new`/`fn with_*` that returns one. In EACH constructor that initializes the struct fields, add:

```rust
            tool_depth: std::collections::HashMap::new(),
```

Ensure it appears in the struct literal in every constructor or the build will fail.

- [ ] **Step 4: Find the `SessionUpdate::ToolCall(tc)` handler**

Run: `rg -n "SessionUpdate::ToolCall\(" crates/spur-tui/src/views/session_detail.rs`
You are looking for the match arm in `handle_spur_event` around session_detail.rs:1176-1188 (from the initial audit).

- [ ] **Step 5: Rewrite the `ToolCall` handler to use `SpurToolMeta`**

Replace the current `SessionUpdate::ToolCall(tc) => { ... }` arm body with:

```rust
SessionUpdate::ToolCall(tc) => {
    let meta = spur_acp::adapter::extract_tool_meta(tc, self.agent_cfg.kind);
    let display_name = meta
        .tool_name
        .as_deref()
        .unwrap_or(tc.title.as_str());
    let depth = meta
        .parent_tool_use_id
        .as_ref()
        .and_then(|pid| self.tool_depth.get(pid).copied())
        .map(|d| d.saturating_add(1).min(8))
        .unwrap_or(0);
    self.tool_depth.insert(tc.tool_call_id.0.to_string(), depth);

    let indent = "  ".repeat(depth as usize);
    let title = format!("{}{}", indent, display_name);

    // Preserve the rest of the original logic (push TraceKind::Act entry,
    // set stream_in_flight, etc.) but use `title` as the tool display string
    // instead of the former `tc.title` fallback.
    // ...
}
```

**Adaptation note:** the original arm body likely builds a `TraceEntry` with `TraceKind::Act { tool: ..., family: ..., input: ... }`. Replace only the `tool` argument with `title` (computed above). Do NOT change `family` or `input` — those are pre-existing logic paths.

- [ ] **Step 6: Verify `agent_cfg.kind` is accessible in the handler scope**

Run: `rg -n "agent_cfg|agent_kind" crates/spur-tui/src/views/session_detail.rs | head -20`
If `agent_cfg` is not already in scope, the handler must receive it. Look for how other adapter calls get the kind — `extract_observe` is likely called somewhere in the same method. Mirror that pattern.

- [ ] **Step 7: Build to check for compile errors**

Run: `cargo check --package spur-tui`
Expected: builds cleanly. If `ToolCallId.0` is not a tuple struct, use `tc.tool_call_id.to_string()` instead.

- [ ] **Step 8: Add a unit test for the nesting depth logic**

Append to the existing `#[cfg(test)] mod tests` block in `crates/spur-tui/src/views/session_detail.rs` (find it with `rg -n "#\[cfg\(test\)\]" crates/spur-tui/src/views/session_detail.rs`):

```rust
#[test]
fn tool_depth_nested_two_levels() {
    use std::collections::HashMap;
    let mut tool_depth: HashMap<String, u8> = HashMap::new();

    // Level 0: root Task call.
    tool_depth.insert("tc-root".into(), 0);

    // Level 1: child references root.
    let depth_1 = Some("tc-root")
        .and_then(|pid| tool_depth.get(pid).copied())
        .map(|d| d.saturating_add(1).min(8))
        .unwrap_or(0);
    tool_depth.insert("tc-child".into(), depth_1);
    assert_eq!(depth_1, 1);

    // Level 2: grandchild references child.
    let depth_2 = Some("tc-child")
        .and_then(|pid| tool_depth.get(pid).copied())
        .map(|d| d.saturating_add(1).min(8))
        .unwrap_or(0);
    assert_eq!(depth_2, 2);
}

#[test]
fn tool_depth_unknown_parent_defaults_zero() {
    use std::collections::HashMap;
    let tool_depth: HashMap<String, u8> = HashMap::new();
    let depth = Some("tc-ghost")
        .and_then(|pid| tool_depth.get(pid).copied())
        .map(|d| d.saturating_add(1).min(8))
        .unwrap_or(0);
    assert_eq!(depth, 0);
}

#[test]
fn tool_depth_caps_at_eight() {
    use std::collections::HashMap;
    let mut tool_depth: HashMap<String, u8> = HashMap::new();
    tool_depth.insert("tc-deep".into(), 8);
    let depth = Some("tc-deep")
        .and_then(|pid| tool_depth.get(pid).copied())
        .map(|d| d.saturating_add(1).min(8))
        .unwrap_or(0);
    assert_eq!(depth, 8);
}
```

- [ ] **Step 9: Run the new tests**

Run: `cargo test --package spur-tui --lib tool_depth`
Expected: 3 tests PASS.

- [ ] **Step 10: Run the existing spur-tui snapshot suite for regressions**

Run: `cargo test --package spur-tui`
Expected: all tests pass. If pre-existing snapshot tests are affected by the new indent prefix, review the diff: if the snapshot shows an unexpected indent, the root-depth logic is wrong; if the indent is correct, update the snapshot with `cargo insta review` or equivalent.

- [ ] **Step 11: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): normalize tool identity + subagent nesting via SpurToolMeta

Closes gaps #4 (parent_tool_use_id nesting) and #5 (vendor tool_name).
session_detail.rs now reads only through spur_acp::adapter::extract_tool_meta;
no _meta or vendor tokens in spur-tui.

Refs: docs/superpowers/specs/2026-04-17-acp-vendor-meta-unification-design.md"
```

---

## Task 3: `ReactTrace::append_user_message` + close Gap #1

**Files:**
- Create: `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/user_message_chunk_replay.json`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Create the `user_message_chunk_replay.json` fixture**

Write this file:

```json
{
  "sessionId": "sess-cc-replay-001",
  "update": {
    "sessionUpdate": "user_message_chunk",
    "content": {
      "type": "text",
      "text": "list the files in src/"
    }
  }
}
```

- [ ] **Step 2: Write the failing ReactTrace test**

In `crates/spur-tui/src/components/react_trace/mod.rs`, find the `#[cfg(test)] mod tests` block (search for `fn append_message_merges_consecutive_chunks_from_same_agent`). Add these two new tests right after it:

```rust
#[test]
fn append_user_message_creates_new_entry_when_empty() {
    let mut trace = ReactTrace::new();
    trace.append_user_message("hello", "10:00:01".to_string());
    let entries = trace.entries_for_test();
    let user_count = entries
        .iter()
        .filter(|e| matches!(&e.kind, TraceKind::UserMessage))
        .count();
    assert_eq!(user_count, 1);
    assert_eq!(entries.last().unwrap().text, "hello");
}

#[test]
fn append_user_message_coalesces_into_tail_user_entry() {
    let mut trace = ReactTrace::new();
    // Simulate `push_user_message` from the UI side (existing behavior).
    trace.push(TraceEntry {
        kind: TraceKind::UserMessage,
        text: "hello ".to_string(),
        timestamp: "10:00:01".to_string(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
    // Streamed chunk arrives AFTER push — must merge, not duplicate.
    trace.append_user_message("world", "10:00:02".to_string());

    let entries = trace.entries_for_test();
    let user_entries: Vec<_> = entries
        .iter()
        .filter(|e| matches!(&e.kind, TraceKind::UserMessage))
        .collect();
    assert_eq!(user_entries.len(), 1, "must coalesce, not duplicate");
    assert_eq!(user_entries[0].text, "hello world");
}
```

- [ ] **Step 3: Run the tests — verify they fail**

Run: `cargo test --package spur-tui --lib append_user_message`
Expected: both tests FAIL with "no method named `append_user_message`".

- [ ] **Step 4: Implement `append_user_message` on ReactTrace**

In `crates/spur-tui/src/components/react_trace/mod.rs`, find the existing `pub fn append_message(&mut self, text: &str, agent: &str, timestamp: String)` around line 187. Add this new method directly after it:

```rust
/// Append a streamed user-message chunk, coalescing into the tail entry
/// iff that entry is `TraceKind::UserMessage`. Otherwise push a new
/// `TraceKind::UserMessage` entry. Symmetric to `append_message` for
/// agent chunks.
pub fn append_user_message(&mut self, text: &str, timestamp: String) {
    let target_idx = match self.entries.last() {
        Some(entry) => match &entry.kind {
            TraceKind::UserMessage => Some(self.entries.len() - 1),
            _ => None,
        },
        None => None,
    };

    match target_idx {
        Some(idx) => {
            self.entries[idx].text.push_str(text);
            self.mark_dirty_from(idx);
        }
        None => {
            self.entries.push(TraceEntry {
                kind: TraceKind::UserMessage,
                text: text.to_string(),
                timestamp,
                #[cfg(feature = "markdown")]
                markdown: None,
            });
            self.mark_dirty_from(self.entries.len() - 1);
        }
    }
}
```

- [ ] **Step 5: Run the tests — verify they pass**

Run: `cargo test --package spur-tui --lib append_user_message`
Expected: both tests PASS.

- [ ] **Step 6: Wire `UserMessageChunk` in session_detail.rs**

In `crates/spur-tui/src/views/session_detail.rs`, find the match arm list inside the `AgentNotification` handler (search: `rg -n "SessionUpdate::AgentMessageChunk" crates/spur-tui/src/views/session_detail.rs`). Read the AgentMessageChunk arm carefully — it extracts `&str` text from `chunk.content` (a `ContentBlock`) via inline pattern matching on `ContentBlock::Text(tc)`. Replicate that extraction for user chunks.

Add a new arm immediately after the `AgentMessageChunk` arm:

```rust
SessionUpdate::UserMessageChunk(chunk) => {
    if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
        self.react_trace
            .append_user_message(&tc.text, Self::now_stamp());
    }
    self.stream_in_flight = true;
}
```

**Adaptation note:** the AgentMessageChunk arm may extract text through a local closure, a helper method, or inline pattern match — whichever it uses, mirror it. The only requirements are: (a) text is the `ContentBlock::Text` body, (b) non-Text variants are silently skipped, (c) `append_user_message` is called with that text plus `Self::now_stamp()`.

- [ ] **Step 7: Build to verify**

Run: `cargo check --package spur-tui`
Expected: clean build.

- [ ] **Step 8: Run full spur-tui test suite**

Run: `cargo test --package spur-tui`
Expected: all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-acp/tests/fixtures/notifications/claude-code-acp/user_message_chunk_replay.json
git commit -m "feat(spur-tui): coalescing append_user_message + UserMessageChunk wiring

Closes gap #1. Streamed user_message_chunk during loadSession replay
now renders correctly and idempotently merges into any pre-existing
TraceKind::UserMessage tail entry (no double-render)."
```

---

## Task 4: Extend `extract_text` for Diff + Terminal content — close Gaps #2 and #3

**Files:**
- Create: `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_edit_with_diff.json`
- Create: `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_terminal.json`
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Create the `tool_call_edit_with_diff.json` fixture**

```json
{
  "sessionId": "sess-cc-diff-001",
  "update": {
    "sessionUpdate": "tool_call",
    "toolCallId": "tc-edit-001",
    "title": "Edit",
    "kind": "edit",
    "content": [
      {
        "type": "diff",
        "path": "src/foo.rs",
        "oldText": "fn old() {}\n",
        "newText": "fn new_name() {}\n"
      }
    ],
    "_meta": { "claudeCode": { "toolName": "Edit" } }
  }
}
```

- [ ] **Step 2: Create the `tool_call_terminal.json` fixture**

```json
{
  "sessionId": "sess-cc-term-001",
  "update": {
    "sessionUpdate": "tool_call",
    "toolCallId": "tc-bash-term-001",
    "title": "Bash",
    "kind": "execute",
    "content": [
      { "type": "terminal", "terminalId": "term-abc-123" }
    ],
    "_meta": { "claudeCode": { "toolName": "Bash" } }
  }
}
```

- [ ] **Step 3: Locate the current `extract_text` function in session_detail.rs**

Run: `rg -n "fn extract_text" crates/spur-tui/src/views/session_detail.rs`
Read ±20 lines of context to understand its exact signature (likely takes `&[ToolCallContent]` and returns `Option<String>`).

- [ ] **Step 4: Write failing unit tests for Diff and Terminal extraction**

Append to the `#[cfg(test)] mod tests` block in `crates/spur-tui/src/views/session_detail.rs`:

```rust
#[test]
fn extract_text_renders_diff_content() {
    use spur_acp::ToolCallContent;
    let content = vec![ToolCallContent::Diff {
        path: "src/foo.rs".into(),
        old_text: Some("fn old() {}\n".into()),
        new_text: "fn new_name() {}\n".into(),
    }];
    let out = extract_text(&content).expect("should return Some");
    assert!(out.contains("src/foo.rs"), "diff must include path");
    assert!(out.contains("-fn old"), "diff must include old line prefix");
    assert!(out.contains("+fn new_name"), "diff must include new line prefix");
}

#[test]
fn extract_text_renders_terminal_placeholder() {
    use spur_acp::ToolCallContent;
    use agent_client_protocol::TerminalId;
    let content = vec![ToolCallContent::Terminal {
        terminal_id: TerminalId("term-abc-123".into()),
    }];
    let out = extract_text(&content).expect("should return Some");
    assert!(out.contains("term-abc-123"), "placeholder must include id");
    assert!(
        out.starts_with("[terminal:"),
        "placeholder must be labeled"
    );
}

#[test]
fn extract_text_truncates_long_diffs() {
    use spur_acp::ToolCallContent;
    let big_new = "line\n".repeat(200);
    let content = vec![ToolCallContent::Diff {
        path: "big.txt".into(),
        old_text: Some("".into()),
        new_text: big_new,
    }];
    let out = extract_text(&content).expect("should return Some");
    let line_count = out.lines().count();
    assert!(line_count <= 60, "expected truncation around 40 body lines + header, got {}", line_count);
    assert!(out.contains("more lines"), "must indicate truncation");
}
```

- [ ] **Step 5: Run tests — verify they fail**

Run: `cargo test --package spur-tui --lib extract_text`
Expected: 3 new tests FAIL (existing extract_text tests, if any, should still pass).

- [ ] **Step 6: Extend `extract_text` to handle Diff and Terminal**

Replace the body of `extract_text` in session_detail.rs with:

```rust
fn extract_text(content: &[spur_acp::ToolCallContent]) -> Option<String> {
    use spur_acp::ToolCallContent;
    let mut out = String::new();
    for c in content {
        match c {
            ToolCallContent::Content { content: cb } => {
                if let spur_acp::ContentBlock::Text(tc) = cb {
                    out.push_str(&tc.text);
                }
                // Other ContentBlock variants (Image, Audio, Resource) silently
                // skipped — tracked separately, see design doc.
            }
            ToolCallContent::Diff { path, old_text, new_text } => {
                out.push_str(&format_diff_truncated(
                    path,
                    old_text.as_deref(),
                    new_text,
                ));
            }
            ToolCallContent::Terminal { terminal_id } => {
                out.push_str(&format!("[terminal: {}]", terminal_id.0));
            }
            _ => {
                // ToolCallContent is #[non_exhaustive] — ignore unknown
                // variants rather than crashing on protocol upgrades.
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

const DIFF_MAX_LINES: usize = 40;

fn format_diff_truncated(path: &str, old: Option<&str>, new_: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n", path));
    out.push_str(&format!("+++ b/{}\n", path));

    let mut body_lines = 0usize;
    let mut truncated_old = 0usize;
    let mut truncated_new = 0usize;

    if let Some(old_text) = old {
        for line in old_text.lines() {
            if body_lines >= DIFF_MAX_LINES {
                truncated_old += 1;
                continue;
            }
            out.push_str(&format!("-{}\n", line));
            body_lines += 1;
        }
    }
    for line in new_.lines() {
        if body_lines >= DIFF_MAX_LINES {
            truncated_new += 1;
            continue;
        }
        out.push_str(&format!("+{}\n", line));
        body_lines += 1;
    }
    let total_truncated = truncated_old + truncated_new;
    if total_truncated > 0 {
        out.push_str(&format!("... ({} more lines)\n", total_truncated));
    }
    out
}
```

**Notes:**
- `ToolCallContent` is `#[non_exhaustive]` in the ACP crate — the `_ =>` arm is required.
- `TerminalId.0` accesses the inner string (newtype tuple struct). If it's not a tuple struct, use `terminal_id.to_string()` instead and adjust the test assertion.
- `format_diff_truncated` does NOT compute a real LCS diff; it renders "old lines as deletions, new lines as additions." Matches the pattern `claude.rs::make_unified` already uses.

- [ ] **Step 7: Run the new tests — verify they pass**

Run: `cargo test --package spur-tui --lib extract_text`
Expected: 3 new tests PASS; existing `extract_text_*` tests (if any) still PASS.

- [ ] **Step 8: Run full spur-tui suite**

Run: `cargo test --package spur-tui`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_edit_with_diff.json \
        crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_terminal.json
git commit -m "feat(spur-tui): render ToolCallContent::Diff and ::Terminal

Closes gaps #2 (inline diff extraction; 40-line truncation with 'N more lines'
suffix) and #3 (terminal placeholder '[terminal: <id>]'). Live terminal
output subscription deferred to a follow-up spec."
```

---

## Task 5: CI guardrail — grep-based vendor-leak check

**Files:**
- Create: `scripts/check-no-vendor-meta-leak.sh`
- Create: `.github/workflows/vendor-leak-check.yml`

- [ ] **Step 1: Write the guardrail script**

Create `scripts/check-no-vendor-meta-leak.sh`:

```bash
#!/usr/bin/env bash
# Fails if vendor-specific tokens appear in spur-tui.
# Normalization happens in spur-acp adapters; spur-tui must consume only
# normalized types. See docs/spur/acp-meta-conventions.md.
set -euo pipefail

VENDOR_TOKENS='"_meta"|claudeCode|parentToolUseId|toolResponse|terminal_info'
TARGET='crates/spur-tui/src/'
ALLOWLIST_MARKER='allow-vendor-read'

# Find matches, then drop lines carrying the allowlist marker.
MATCHES=$(grep -rnE "$VENDOR_TOKENS" "$TARGET" || true)
MATCHES=$(printf '%s\n' "$MATCHES" | grep -v "$ALLOWLIST_MARKER" || true)

if [ -n "$MATCHES" ]; then
    echo "ERROR: vendor-specific tokens found in $TARGET:" >&2
    echo "$MATCHES" >&2
    echo "" >&2
    echo "Vendor _meta access must go through spur_acp::adapter." >&2
    echo "If this read is intentional, add '// allow-vendor-read' on the line." >&2
    echo "See docs/spur/acp-meta-conventions.md." >&2
    exit 1
fi

echo "OK: no vendor-specific tokens in $TARGET"
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/check-no-vendor-meta-leak.sh`

- [ ] **Step 3: Run the script — verify it passes on the current tree**

Run: `./scripts/check-no-vendor-meta-leak.sh`
Expected output: `OK: no vendor-specific tokens in crates/spur-tui/src/`
Exit code: 0.

- [ ] **Step 4: Verify the script CAN fail**

Temporarily add this line somewhere in `crates/spur-tui/src/views/session_detail.rs` (e.g. in a comment):
```rust
// claudeCode test marker — REMOVE ME
```

Run: `./scripts/check-no-vendor-meta-leak.sh`
Expected: exits 1, prints the match.

REMOVE the test marker and re-run to confirm exit 0. Do NOT commit the marker.

- [ ] **Step 5: Create the GitHub Actions workflow**

Create `.github/workflows/vendor-leak-check.yml`:

```yaml
name: vendor-leak-check

on:
  push:
    branches: [main]
  pull_request:

jobs:
  check:
    name: No vendor tokens in spur-tui
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run guardrail
        run: ./scripts/check-no-vendor-meta-leak.sh
```

- [ ] **Step 6: Commit**

```bash
git add scripts/check-no-vendor-meta-leak.sh .github/workflows/vendor-leak-check.yml
git commit -m "ci: grep guardrail forbidding vendor _meta tokens in spur-tui

Enforces architectural invariant from
docs/spur/acp-meta-conventions.md — all vendor-specific JSON paths
are accessed only through spur_acp::adapter. Escape hatch:
// allow-vendor-read comment on the line."
```

---

## Task 6: Convention document

**Files:**
- Create: `docs/spur/acp-meta-conventions.md`

- [ ] **Step 1: Write the convention document**

Create `docs/spur/acp-meta-conventions.md` with exactly this content:

```markdown
# ACP Vendor Meta Conventions

**Status:** Reference. Single source of truth for how spur handles vendor-specific ACP `_meta` extensions.

## 1. Why This Exists

The Agent Client Protocol defines `_meta` as an extension channel for fields the core spec does not cover. Every vendor (claude-agent-acp, codex, kiro, gemini, opencode) uses it differently. Spur normalizes these into one struct — `SpurToolMeta` — in `crates/spur-acp/src/adapter/` so that downstream crates (especially spur-tui) never depend on vendor-specific JSON paths.

## 2. Namespace Rule

All vendor extensions go under:

```
_meta.<vendor>.<key>
```

`<vendor>` is the camelCase form of the `AgentKind` variant:

| AgentKind            | Vendor prefix       |
|----------------------|---------------------|
| `ClaudeCodeAcp`      | `claudeCode`        |
| `ClaudeStreamJson`   | `claudeCode`        |
| `CodexAcp`           | `codex`             |
| `Kiro`               | `kiro`              |
| `Generic` (gemini)   | (no standard prefix)|

Gemini currently falls under `Generic`. If a dedicated `AgentKind::Gemini` is introduced later, its prefix will be `gemini`.

## 3. Known Normalized Keys

`SpurToolMeta` (`crates/spur-acp/src/adapter/mod.rs`) exposes these fields today:

| Field                | Claude path                              | Meaning                              |
|----------------------|------------------------------------------|--------------------------------------|
| `tool_name`          | `_meta.claudeCode.toolName`              | Vendor-specific tool identity        |
| `parent_tool_use_id` | `_meta.claudeCode.parentToolUseId`       | Subagent/Task nesting reference      |

## 4. What Does NOT Go in `SpurToolMeta`

- **Fields already expressed by ACP spec.** `terminal_id` belongs on `ToolCallContent::Terminal`; `raw_output` belongs on `ToolCallUpdate.fields.raw_output`. Adding them to `SpurToolMeta` would duplicate the spec.
- **Vendor-only concepts not yet needed cross-vendor.** Claude-specific `toolResponse` payloads, Kiro's spec IDs, etc. stay inside each vendor's adapter module and are mapped to existing normalized types (`ObservePayload`, `ToolInputDisplay`) where possible.

## 5. Non-ACP Translator Obligation

If an agent does NOT speak ACP natively (claude CLI stream-json, opencode, future wrappers), the translator (e.g. `crates/spur-acp/src/protocol/claude_events.rs`) MUST emit `SessionNotification`s with `_meta.<vendor>.*` synthesized from the source event. This keeps the adapter extraction path uniform across transports.

## 6. Vendor Onboarding Checklist

To add a new agent:

1. Add an `AgentKind::<Name>` variant in `crates/spur-acp/src/types.rs`.
2. Create `crates/spur-acp/src/adapter/<vendor>.rs` with:
   - `pub fn extract_tool_meta(tc: &ToolCall) -> super::SpurToolMeta`
   - `pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload>`
   - `pub fn mode_badge(id: &str) -> Option<ModeBadge>`
   - `pub fn refine(title: &str, base: ToolFamily) -> ToolFamily`
3. Wire each function into the `match kind` dispatcher in `adapter/mod.rs`.
4. Add a TOML descriptor entry in `crates/spur-acp/src/agents/defaults.toml`.
5. Capture a live-session fixture to `crates/spur-acp/tests/fixtures/notifications/<agent>/`.
6. If the agent is non-ACP, add a translator module emitting `_meta.<vendor>.*` and test its ACP output.
7. If the vendor introduces a cross-vendor concept not yet in `SpurToolMeta`, propose a new field via a design doc and update Section 3 of this file.

## 7. Governance

Adding a field to `SpurToolMeta` requires:

- Written justification in a design doc under `docs/superpowers/specs/`
- Sign-off from spur-acp and spur-tui owners
- Update to Section 3 in the same commit

## 8. Enforcement

A CI guardrail (`scripts/check-no-vendor-meta-leak.sh`) forbids the tokens `"_meta"`, `claudeCode`, `parentToolUseId`, `toolResponse`, `terminal_info` in `crates/spur-tui/src/`. Escape hatch: add `// allow-vendor-read` on a specific line to whitelist it.
```

- [ ] **Step 2: Verify the doc renders cleanly**

Run: `cat docs/spur/acp-meta-conventions.md | head -30`
Expected: clean markdown, no unresolved placeholders.

- [ ] **Step 3: Commit**

```bash
git add docs/spur/acp-meta-conventions.md
git commit -m "docs(spur): ACP vendor meta conventions reference

Namespace rule, known normalized keys, non-ACP translator obligation,
vendor-onboarding checklist, governance rules, CI-enforcement reference.
Replaces vendor-specific tribal knowledge."
```

---

## Task 7: Final integration check & PR preparation

**Files:**
- No code changes; verification only.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace`
Expected: all tests pass. If any fail, diagnose and fix before proceeding.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings promoted to errors. Fix any that appeared from the new code.

- [ ] **Step 3: Run the vendor-leak guardrail**

Run: `./scripts/check-no-vendor-meta-leak.sh`
Expected: `OK: no vendor-specific tokens in crates/spur-tui/src/`, exit 0.

- [ ] **Step 4: Manual smoke test with a real Claude session**

Run spur-tui against a claude-code-acp session and execute at least:
- One `Bash` tool call — confirm the tool title shows "Bash" (from `_meta.claudeCode.toolName`), not the heuristic guess.
- One `Edit` tool call — confirm the diff renders inline under the tool title, truncated if large.
- One `Task` subagent dispatch — confirm child tool calls render with 2-space indentation under the parent.
- Load a past session — confirm user turns replay correctly (no blanks, no duplicates).

Record observed behavior in a notes file (not committed).

- [ ] **Step 5: Confirm commits are clean**

Run: `git log --oneline feat/acp-vendor-meta-unification ^main`
Expected: 6 commits in order — infrastructure, session_detail meta, user message chunk, diff+terminal, CI guardrail, convention doc.

- [ ] **Step 6: Push and open PR**

```bash
git push -u origin feat/acp-vendor-meta-unification
gh pr create --title "feat: ACP vendor meta unification + session-detail rendering fixes" --body "$(cat <<'EOF'
## Summary

- Introduces `spur_acp::adapter::SpurToolMeta` + `extract_tool_meta` — per-vendor extractor for `_meta.<vendor>.*` extensions.
- Closes five session-detail rendering gaps (user_message_chunk, diff content, terminal placeholder, subagent nesting, vendor tool name) without any vendor token in `crates/spur-tui/`.
- Adds a CI grep guardrail enforcing the invariant.
- Publishes `docs/spur/acp-meta-conventions.md` as the vendor-onboarding reference.

## Test plan

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `./scripts/check-no-vendor-meta-leak.sh` exits 0
- [ ] Manual smoke: Bash tool renders "Bash" name, Edit renders diff, Task subagent renders indented, past-session load shows user turns
- [ ] CI `vendor-leak-check` job green

Refs: docs/superpowers/specs/2026-04-17-acp-vendor-meta-unification-design.md

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed. Task complete.

---

## Spec Coverage Check

| Spec section                                   | Task  |
|------------------------------------------------|-------|
| §5.1 `SpurToolMeta` struct                     | 1     |
| §5.2 claude extractor + stubs                  | 1     |
| §5.3 session_detail consumer pattern           | 2     |
| §5.4 `tool_depth` state                        | 2     |
| §6.1 Gap #1 UserMessageChunk                   | 3     |
| §6.2 Gap #2 Diff content                       | 4     |
| §6.3 Gap #3 Terminal placeholder               | 4     |
| §6.4 Gap #4 parent_tool_use_id                 | 2     |
| §6.5 Gap #5 tool_name                          | 2     |
| §7 Convention document                         | 6     |
| §8 Golden fixtures                             | 1, 3, 4 |
| §9 CI guardrail                                | 5     |
| §10 Sequencing (6 commits)                     | 1-6   |
| §11 Risks (truncation, depth cap, coalescing)  | 2, 3, 4 |
| §12 Follow-ups (F1-F4)                         | out of scope — spec explicit |

No gaps.
