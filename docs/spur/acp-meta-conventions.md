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
| `Kimi`               | `kimi`              |
| `Gemini`             | `gemini`            |
| `Generic`            | (no standard prefix)|

Gemini's native `invoke_agent` ACP frames do not currently expose child
tool/output metadata through `_meta`; the Gemini adapter only derives ordinary
ACP input/content from the frame title so renderers have display text.

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
4. Add a seed entry in `crates/spur-acp/src/seed_agents.toml`, and a delegation descriptor in `crates/spur-acp/src/agents/defaults.toml` if the agent is worker-capable.
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
