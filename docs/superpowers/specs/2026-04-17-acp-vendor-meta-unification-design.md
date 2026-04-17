# ACP Vendor Meta Unification & Session-Detail Rendering Design

**Status:** Design approved, awaiting written-spec review.
**Date:** 2026-04-17
**Author:** kevin.truong.ds@gmail.com
**Related:** `docs/superpowers/specs/2026-04-12-acp-session-event-passthrough.md`, `docs/superpowers/specs/2026-04-12-stream-json-adapter-design.md`

## 1. Context & Problem

Spur ingests `session/update` notifications from multiple ACP-speaking agents (claude-agent-acp, kiro-cli, codex-cli, gemini) and from non-ACP agents via translators (Claude CLI stream-json today; opencode planned). Each vendor uses the ACP `_meta` channel to carry extensions the core spec does not define:

- claude-agent-acp emits `_meta.claudeCode.{toolName, parentToolUseId, toolResponse}` and `_meta.terminal_info.terminal_id`
- other vendors emit their own `_meta.<vendor>.*` payloads

Two problems compound:

**P1 — Rendering gaps in `crates/spur-tui/src/views/session_detail.rs`.** An audit against upstream `@agentclientprotocol/claude-agent-acp` identified five gaps (full gap list in Section 4).

**P2 — Vendor-specific field reads leaking into spur-tui.** A naive fix for two of those gaps would have spur-tui read `_meta.claudeCode.*` directly, coupling the UI crate to every vendor's private schema. With five+ agents on the roadmap this scales badly.

This spec solves both together: the unification layer that keeps spur-tui vendor-blind is what enables the rendering fixes to be written once and work for every agent.

## 2. Goals & Non-Goals

### Goals

1. Define a normalized `SpurToolMeta` struct in `spur-acp` that extracts vendor `_meta` into a stable shape.
2. Establish `_meta.<vendor>.<key>` as the only convention by which vendors add non-spec fields.
3. Close the five rendering gaps in session_detail.rs using only normalized types.
4. Guarantee (via CI) that no vendor-specific token appears in `crates/spur-tui/src/`.
5. Publish a written convention document so new-agent onboarding is mechanical.

### Non-Goals (explicit v1 exclusions)

1. Live terminal output subscription — deferred to a follow-up spec.
2. Routing `ToolCallContent::Diff` through `ObservePayload::EditResult` via the adapter — separate refactor.
3. `opencode` translator implementation — tracked but out of scope here.
4. A plugin-loadable third-party adapter registry (trait + dynamic dispatch) — premature; revisit when vendor count exceeds ten.
5. Declarative (TOML/JSONPath) meta extraction — premature; revisit when `SpurToolMeta` grows past ten fields.

## 3. Architecture Overview

The design has four layers, each with a single responsibility:

```
  Wire                  Adapter                     Consumer
  ────                  ───────                     ────────
  ACP SessionUpdate     spur-acp::adapter           spur-tui session_detail
     { _meta: {         ::extract_tool_meta(tc,       let meta = extract_tool_meta(...);
         <vendor>: {       kind) -> SpurToolMeta      use meta.tool_name
           key: ..       ────────────────────         use meta.parent_tool_use_id
         } } }          per-vendor functions;        // no _meta reads anywhere
                        match on AgentKind
```

**Invariants:**

- I1. Vendor-specific JSON paths are referenced ONLY in `crates/spur-acp/src/adapter/<vendor>.rs`.
- I2. `crates/spur-tui/` contains zero occurrences of `_meta`, `claudeCode`, `parentToolUseId`, or any other vendor token.
- I3. `SpurToolMeta` contains ONLY fields that represent cross-vendor concepts the ACP spec does not already cover.
- I4. Non-ACP agents emit `SessionNotification`s with `_meta.<vendor>.*` synthesized from their native wire format.

## 4. The Five Rendering Gaps & How They Map to the Architecture

| # | Gap | Root cause | Fix location |
|---|-----|------------|--------------|
| 1 | `UserMessageChunk` not rendered in AgentNotification handler | unmatched SessionUpdate variant | session_detail.rs match arm + new `append_user_message` on `ReactTrace` |
| 2 | `ToolCallContent::Diff` dropped by `extract_text` | extraction only handles Text | session_detail.rs `extract_text` |
| 3 | `ToolCallContent::Terminal` dropped by `extract_text` | same | session_detail.rs `extract_text` (placeholder only; live subscription deferred) |
| 4 | Subagent nesting lost | `parent_tool_use_id` never read | **`SpurToolMeta.parent_tool_use_id`** — normalized through adapter |
| 5 | Tool identity weak (title-heuristic only) | `tool_name` never read | **`SpurToolMeta.tool_name`** — normalized through adapter |

Gaps #1-#3 are pure render fixes (spec-typed fields). Gaps #4-#5 are the ones that would have leaked vendor knowledge without the adapter; they are the justification for this spec.

## 5. Design — `SpurToolMeta` and Per-Vendor Extractors

### 5.1 Normalized type

```rust
// crates/spur-acp/src/adapter/mod.rs

/// Normalized view of vendor-specific extensions on a ToolCall.
///
/// Fields are added to this struct ONLY when a concept is genuinely
/// cross-vendor and NOT already expressed by an ACP spec field.
/// Adding a field is a design change — see
/// docs/spur/acp-meta-conventions.md.
#[derive(Debug, Default, Clone)]
pub struct SpurToolMeta {
    /// Vendor-specific tool identity (e.g. "Bash", "Edit", "/spec-init").
    /// Prefer this over `tc.title` for routing decisions (diff renderer, terminal renderer).
    pub tool_name: Option<String>,

    /// ID of the parent ToolCall when this call was spawned by a
    /// subagent/Task mechanism. Used for render indentation.
    pub parent_tool_use_id: Option<String>,
}

pub fn extract_tool_meta(tc: &ToolCall, kind: AgentKind) -> SpurToolMeta {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson =>
            claude::extract_tool_meta(tc),
        AgentKind::CodexAcp => codex::extract_tool_meta(tc),
        AgentKind::Kiro     => kiro::extract_tool_meta(tc),
        AgentKind::Generic  => SpurToolMeta::default(),
    }
}
```

### 5.2 Per-vendor modules

#### `adapter/claude.rs` (real extractor)

```rust
pub(super) fn extract_tool_meta(tc: &ToolCall) -> SpurToolMeta {
    let cc = tc.meta.as_ref().and_then(|m| m.get("claudeCode"));
    SpurToolMeta {
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

#### `adapter/{codex,kiro,gemini}.rs` (stubs initially)

Return `SpurToolMeta::default()`. Each file carries a `TODO(vendor-onboarding)` comment pointing to `docs/spur/acp-meta-conventions.md` and a tracking issue. When a vendor actually starts emitting recognizable `_meta` fields, the stub is replaced with a real extractor. Stub vs real is decided per-vendor by a real capture of a live session; see Section 8 for the fixture strategy.

This matches decision Q1=c: claude full, others stub, issues tracking.

### 5.3 Consumer access pattern

```rust
// crates/spur-tui/src/views/session_detail.rs

SessionUpdate::ToolCall(tc) => {
    let meta = spur_acp::adapter::extract_tool_meta(tc, self.agent_cfg.kind);
    let display_name = meta.tool_name.as_deref().unwrap_or(&tc.title);
    let depth = meta.parent_tool_use_id.as_ref()
        .and_then(|pid| self.tool_depth.get(pid).copied())
        .map(|d| d + 1)
        .unwrap_or(0);
    self.tool_depth.insert(tc.id.0.to_string(), depth);
    // ... push TraceEntry with display_name and depth-indented title
}
```

### 5.4 Session-state additions

`SessionDetailView` gains one field:

```rust
/// Maps ToolCall id -> render depth for subagent nesting.
/// Populated on each ToolCall; read on each ToolCall to resolve parent depth.
tool_depth: std::collections::HashMap<String, u8>,
```

Bounded by session lifetime; cleared on session close. Depth capped at 8 to prevent runaway indentation from malformed parent chains.

## 6. Design — Session-Detail Rendering Fixes

### 6.1 Gap #1 — `UserMessageChunk`

New method on `ReactTrace`:

```rust
// crates/spur-tui/src/components/react_trace/mod.rs
pub fn append_user_message(&mut self, text: &str, timestamp: String) {
    // Coalesce into tail entry iff it is TraceKind::UserMessage.
    // Otherwise push new entry with TraceKind::UserMessage.
    // Symmetric to existing append_message for TraceKind::AgentMessage.
}
```

New match arm in session_detail.rs:

```rust
SessionUpdate::UserMessageChunk(c) => {
    if let Some(text) = extract_text(&c.content) {
        react_trace.append_user_message(&text, timestamp());
    }
}
```

This coalescing prevents double-render when `loadSession` replays history (the HistoryEntry path already pushed a UserMessage entry, and `append_user_message` merges into it rather than pushing a duplicate).

### 6.2 Gap #2 — `ToolCallContent::Diff`

Extend `extract_text` at session_detail.rs:1698:

```rust
fn extract_text(content: &[ToolCallContent]) -> Option<String> {
    let mut out = String::new();
    for c in content {
        match c {
            ToolCallContent::Content { content: ContentBlock::Text(tc) } => {
                out.push_str(&tc.text);
            }
            ToolCallContent::Diff { path, old_text, new_text } => {
                out.push_str(&format_diff(path, old_text.as_deref(), new_text));
            }
            ToolCallContent::Terminal { terminal_id } => {
                out.push_str(&format!("[terminal: {}]", terminal_id));
            }
            _ => {}  // non-text ContentBlock variants (image etc.) still dropped; tracked separately
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn format_diff(path: &str, old: Option<&str>, new_: &str) -> String {
    // Unified-diff style. Truncate at DIFF_MAX_LINES = 40 with a "... N more lines" suffix.
    // Mirrors the truncation pattern used for raw_input at session_detail.rs:1725.
}
```

### 6.3 Gap #3 — `ToolCallContent::Terminal` (placeholder only)

Covered by the `Terminal` arm above. Renders `[terminal: <id>]`. Live output subscription is a separate spec (follow-up F1).

### 6.4 Gap #4 — Subagent nesting

Covered by `SpurToolMeta.parent_tool_use_id` in Section 5.3. Render applies indent prefix based on depth:

```rust
let indent = "  ".repeat(depth as usize);
let title = format!("{}{}", indent, display_name);
```

### 6.5 Gap #5 — Tool identity

Covered by `SpurToolMeta.tool_name` in Section 5.3. When None, falls back to `tc.title` (existing behavior for non-claude agents, unchanged).

## 7. Design — Convention Document

New file: `docs/spur/acp-meta-conventions.md`. Outline:

1. **Purpose** — one paragraph: why `_meta` exists, why we normalize.
2. **Namespace rule** — `_meta.<vendor>.<key>`. `<vendor>` equals the `AgentKind` variant name in camelCase: `claudeCode`, `codex`, `kiro`, `gemini`, `opencode`.
3. **Known normalized keys** — table of `SpurToolMeta` fields with the canonical vendor paths that populate each.
4. **Non-ACP translator obligation** — if an agent does not speak ACP natively (stream-json, opencode, future agents), the translator MUST emit `SessionNotification`s that carry `_meta.<vendor>.*` synthesized from the source event.
5. **Vendor-onboarding checklist** — seven steps:
   1. Add `AgentKind::<Name>` variant
   2. Create `crates/spur-acp/src/adapter/<vendor>.rs` with `extract_tool_meta`, `try_extract_observe`, `mode_badge`
   3. Wire up in `adapter/mod.rs` dispatch match
   4. Add TOML descriptor in `crates/spur-acp/src/agents/defaults.toml`
   5. Capture a golden fixture to `crates/spur-acp/tests/adapter_fixtures/<vendor>/`
   6. For non-ACP transports: add a translator module and test its ACP output
   7. Update `docs/spur/acp-meta-conventions.md` Section 3 with any new normalized keys the vendor introduces
6. **Governance** — adding a field to `SpurToolMeta` requires: (a) justification in a design doc, (b) sign-off from spur-acp and spur-tui owners, (c) update to this document.

The document is **reference**, not a spec — it lives at `docs/spur/acp-meta-conventions.md` (decision Q4=a).

## 8. Testing Strategy

### 8.1 Golden fixtures

`crates/spur-acp/tests/adapter_fixtures/<vendor>/*.json` — each file contains a real ACP `session/update` JSON payload captured from a live session. Sibling `.expected.toml` files declare expected `SpurToolMeta` output.

Minimum v1 coverage:
- `claude/tool_call_bash.json` → `tool_name: "Bash", parent: None`
- `claude/tool_call_subagent_task.json` → `tool_name: "Task", parent: Some("toolu_xxx")`
- `claude/tool_call_edit_with_diff.json` → diff content extraction (render path, not meta)
- `claude/user_message_chunk_replay.json` → Gap #1 coalescing

### 8.2 Unit tests

- `spur-acp/src/adapter/` — one test per vendor module asserting fixture → expected.
- `spur-tui/src/components/react_trace/` — test coalescing of `append_user_message`, including the idempotent merge case (push_user_message followed by append_user_message of the same turn).
- `spur-tui/src/views/session_detail.rs` — snapshot tests for each of the five gap scenarios (existing snapshot infrastructure).

### 8.3 Invariant enforcement

See Section 9.

## 9. CI Guardrail

New pre-commit hook and GitHub Actions step (decision Q3=a, grep-based):

```sh
# scripts/check-no-vendor-meta-leak.sh
VENDOR_TOKENS='"_meta"|claudeCode|parentToolUseId|toolResponse|terminal_info'
if grep -rn -E "$VENDOR_TOKENS" crates/spur-tui/src/; then
    echo "ERROR: vendor-specific tokens found in spur-tui."
    echo "See docs/spur/acp-meta-conventions.md — reads must go through spur_acp::adapter."
    exit 1
fi
```

Wired into:
- `.git/hooks/pre-commit` via project setup script
- `.github/workflows/ci.yml` as a new `check-vendor-leak` job that runs in parallel with `cargo check`

False-positive rate is low enough to accept (the token `"_meta"` only appears where someone is reading the field). If a legitimate need arises (e.g. a debug print), a `// allow-vendor-read` comment next to the match makes it explicit; the grep is updated to ignore lines with that marker.

## 10. Sequencing & Delivery Plan

Single PR scope for v1 (all changes under one feature branch):

1. **Commit 1 — Infrastructure**: `SpurToolMeta` + `extract_tool_meta` + claude extractor + stubs for codex/kiro/gemini. Fixture files. Unit tests.
2. **Commit 2 — Rendering fixes**: session_detail.rs gaps #1-#5 wired through `SpurToolMeta`. New `append_user_message` on ReactTrace. `format_diff` helper. `tool_depth` state. Snapshot tests.
3. **Commit 3 — CI guardrail**: pre-commit script + GHA job. Verify it fails cleanly on a deliberately-introduced `_meta` read, then remove the test violation.
4. **Commit 4 — Convention doc**: `docs/spur/acp-meta-conventions.md` + CONTRIBUTING.md pointer.

Estimated size: ~180 LOC production code + ~120 LOC test code + ~250 LOC docs. One reviewer-day.

## 11. Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| History-replay double-render for user turns | `append_user_message` coalesces into tail UserMessage entry; test proves idempotency |
| Long diff payloads blow scrollback | Truncate at 40 lines with "N more" suffix; matches existing pattern |
| `parent_tool_use_id` references unknown parent (race) | Default depth 0 on unknown; cap depth at 8 |
| Vendor changes `_meta` path without warning | Golden fixture test fails; extractor update required |
| CI guardrail false positives | `// allow-vendor-read` escape hatch; tokens list is tight |
| A future vendor emits an extension the ACP spec later adopts | When the spec field appears, remove the `_meta` fallback from the extractor; callers unchanged |

## 12. Follow-ups (Tracked, Not Scoped)

- **F1 — Live terminal subscription.** spur-acp exposes `subscribe_terminal_output(TerminalId) -> broadcast::Receiver<TerminalChunk>`; spur-tui renders live output in a TerminalView panel. Separate design doc.
- **F2 — Adapter-layer diff consolidation.** Route `ToolCallContent::Diff` through `ObservePayload::EditResult` so both inline and `raw_output`-borne diffs share one renderer. Separate refactor.
- **F3 — opencode translator.** Non-ACP agent; requires a translator in `crates/spur-acp/src/translators/opencode.rs` that converts opencode's JSON event stream to `SessionNotification` with synthesized `_meta.opencode.*`. Tracked as separate effort.
- **F4 — Plugin adapter registry.** Migrate `extract_tool_meta`'s match-arm to a trait + registry when vendor count exceeds ten or when third-party plugin agents become a supported surface.

## 13. Decisions Recorded

- Q1=c: Claude full extractor; codex/kiro/gemini stubs with tracking issues.
- Q2=a: opencode out of scope; documented as translator obligation only.
- Q3=a: Grep-based CI guardrail.
- Q4=a: Convention doc at `docs/spur/acp-meta-conventions.md` (reference, not spec).

## 14. Open Questions

None. Design is complete. Ready for implementation planning.
