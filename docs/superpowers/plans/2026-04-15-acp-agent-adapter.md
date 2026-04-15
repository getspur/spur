# ACP Agent Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a per-agent adapter layer so the TUI can render ReAct traces uniformly across the ACP brain agents Spur ships (`claude-code`, `claude-code-acp`, `codex`, `codex-acp`, `kiro`, `gemini`), without collapsing their semantic differences (tool-name vocabularies, `raw_output` shapes, mode-id tokens) into generic string prose.

**Architecture:** A pure-data `spur_acp::adapter` module exposes four functions — `classify_tool(&ToolCall, AgentKind)`, `format_input(&Value, AgentKind)`, `extract_observe(&Value, AgentKind)`, `mode_badge(&str, AgentKind)` — keyed on a new `AgentKind` enum declared on `AgentConfig`. Primary classification rides on ACP's own `ToolKind` enum (Read/Edit/Delete/Move/Search/Execute/Think/Fetch/SwitchMode/Other); per-kind modules only `refine(title, base_family)` where the protocol is ambiguous (e.g. Claude's `TodoWrite` arrives as `ToolKind::Other` → `ToolFamily::Plan`). The TUI (`session_detail.rs`) calls these at event-mapping time; no changes to `SpurEventBody`, `NativeAcpConnection`, or the orchestrator pump. Shared logic (MCP-envelope unwrap, generic fallback heuristics) lives in `adapter/{mcp,generic}.rs`. Unknown agents degrade gracefully through `AgentKind::Generic`. `TraceKind::Act` gains `family: ToolFamily` + `input: ToolInputDisplay`; `TraceKind::Observe` gains `payload: Option<ObservePayload>` (the existing `TraceEntry.text` remains the raw fallback when `input` is `Empty` or `payload` is `None` — single source of truth, no shadow strings). `react_trace.rs` renders the new fields with tool-family glyphs, structured observe payloads, and brain-signature tint derived from `AgentKind`.

**Tech Stack:** Rust 2021 workspace · `agent-client-protocol` crate v0.10 (schema crate 0.11) · `ratatui` TUI · `serde_json` for `raw_output` parsing (existing). No new workspace dependencies — per-kind tables are plain `match` blocks, not phf maps.

**Design rationale:** 12-round sequential-thinking brainstorm + a plan-drift audit established (a) ACP's `ToolKind` already canonicalizes tool categories, so the adapter is a thin refinement layer rather than a substitute taxonomy, (b) enrichment is a rendering decision, not canonical truth, so adapter functions belong in spur-acp but are *called* from the TUI (not broadcast on `SpurEvent`), (c) MCP-envelope unwrap is cross-agent and belongs to a shared helper that handles multi-item content blocks, (d) explicit `kind` in TOML beats command/args inference, (e) dual-source-of-truth fields on `TraceKind` (holding both raw text AND structured view) were retracted in favor of one structured-or-fallback invariant.

---

## Out of scope (explicit deferrals)

These are known gaps; do **not** address them in this plan:

- Kiro's `call_ext("_kiro.dev/commands/*", ...)` interception — stateful, requires `NativeAcpConnection` hooks.
- Merged `TraceKind::Action { outcome }` variant (TK-B from the brainstorm) — requires pending-state streaming across ACT→OBSERVE.
- Capability-probe-based `AgentKind` promotion at session-ready.
- User-extensible adapter definitions in `.spur/config.toml` for private agents.
- Auth / session-resume / permission-prompt per-kind hooks.
- Worker-side rendering changes (workers are out of scope per user rescoping).
- Pane-level UX redesign items (turn cards, plan strip, collapse/expand chord) — a **separate** follow-up plan consumes this adapter and delivers the UX. This plan ships only the machinery and minimal render hookups.

---

## File Structure

**Files created:**
- `crates/spur-acp/src/adapter/mod.rs` — public API: `ToolFamily`, `ObservePayload`, `ToolInputDisplay`, `ModeBadge`, `BadgeColor`, and the four adapter functions.
- `crates/spur-acp/src/adapter/mcp.rs` — `unwrap_mcp_envelope()` shared helper; handles multi-item content blocks (concat Text, pick first Json, flag truncation).
- `crates/spur-acp/src/adapter/generic.rs` — fallback heuristics used when per-kind `refine` returns the base family unchanged.
- `crates/spur-acp/src/adapter/claude.rs` — `refine(title, base)` + observe/input extractors for `ClaudeCodeAcp` and `ClaudeStreamJson`.
- `crates/spur-acp/src/adapter/codex.rs` — `refine(title, base)` + extractors for `CodexAcp`.
- `crates/spur-acp/src/adapter/kiro.rs` — `refine(title, base)` + extractors for `Kiro`.
- `crates/spur-acp/tests/adapter_fixtures.rs` — fixture-replay tests.
- `crates/spur-acp/tests/fixtures/notifications/{claude-code-acp,codex-acp,kiro}/*.json` — hand-authored `SessionNotification` payloads (see Task 4 for authoring rubric and optional record-mode for ongoing capture).
- `crates/spur-tui/tests/render_golden.rs` — golden-file rendering tests for the TUI pipeline (Task 7.6).
- `crates/spur-tui/tests/golden/*.txt` — committed expected-output files for the golden tests.

**Files modified:**
- `crates/spur-acp/src/lib.rs` — `pub mod adapter;` + re-export `AgentKind`.
- `crates/spur-acp/src/config/mod.rs` — add `pub kind: AgentKind` field to `AgentConfig` with `#[serde(default)]`; update `with_defaults` to set `kind: AgentKind::Generic`.
- `crates/spur-acp/src/seed_agents.toml` — declare `kind = "..."` for all six shipped agents.
- `crates/spur-acp/src/types.rs` — declare `AgentKind` enum.
- `crates/spur-acp/src/protocol/claude_events.rs` — **no-op for this plan** (passthrough still works; v2 can re-route through adapter).
- `crates/spur-tui/src/components/react_trace.rs` — extend `TraceKind::Act`/`Observe`; render new fields (glyph by family, structured payload).
- `crates/spur-tui/src/views/session_detail.rs` — call adapter functions when constructing trace entries from `ToolCall`/`ToolCallUpdate`/`CurrentModeUpdate`; derive `AgentKind` once from `agent_cfg.kind`.

**Files modified to update exhaustive `AgentConfig` struct literals (Task 1 cascade):**
- `crates/spur-acp/tests/skip_permissions_config.rs` (4 sites: around L42, L71, L93, L120) — add `kind: spur_acp::AgentKind::Generic,` (or the specific kind for the named agent).
- `crates/spur-core/tests/skip_perm_helper.rs` (around L95) — same.
- `crates/spur-tui/tests/session_update_handling.rs` (2 sites: around L107 and L180) — same.

**Files untouched (invariant):**
- `crates/spur-core/src/orchestrator.rs` — no new enrichment site.
- `crates/spur-acp/src/domain/events.rs` — `SpurEventBody` unchanged.
- `crates/spur-acp/src/connection/native.rs` — transport unchanged **except** an optional `--features record-fixtures` tee (Task 4.1b) gated behind a cargo feature. The default build path is untouched.
- `crates/spur-acp/src/connection/*_adapter.rs` — transport unchanged.
- `crates/spur-acp/Cargo.toml` — no new deps; only an optional `record-fixtures` feature flag if Task 4.1b is adopted.

**Files untouched (invariant):**
- `crates/spur-core/src/orchestrator.rs` — no new enrichment site.
- `crates/spur-acp/src/domain/events.rs` — `SpurEventBody` unchanged.
- `crates/spur-acp/src/connection/native.rs` — transport unchanged.
- `crates/spur-acp/src/connection/*_adapter.rs` — transport unchanged.

---

## Task 1: AgentKind + config plumbing (spur-acp)

**Files:**
- Create: nothing new yet.
- Modify: `crates/spur-acp/src/config/mod.rs`, `crates/spur-acp/src/lib.rs`, `crates/spur-acp/src/types.rs` (or chosen home for the enum).

Goal: land the `AgentKind` enum and the TOML field without any behavior change. Every existing test must still pass with `kind` defaulting to `Generic`.

- [ ] **Step 1.1: Define `AgentKind`**

  Add to `crates/spur-acp/src/types.rs` (co-located with `TransportKind`):

  ```rust
  /// Identifies the agent's wire-level idiom for the adapter layer.
  /// Orthogonal to `TransportKind`: multiple kinds share the same transport.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
  #[serde(rename_all = "kebab-case")]
  pub enum AgentKind {
      /// Claude Code via `claude -p --output-format stream-json`.
      ClaudeStreamJson,
      /// Claude Code via `@agentclientprotocol/claude-agent-acp`.
      ClaudeCodeAcp,
      /// Codex via `@zed-industries/codex-acp` (npx or native binary).
      CodexAcp,
      /// Kiro CLI via `kiro-cli acp`.
      Kiro,
      /// Any ACP-speaking agent not otherwise recognized.
      #[default]
      Generic,
  }
  ```

- [ ] **Step 1.2: Add `kind` field to `AgentConfig`**

  In `crates/spur-acp/src/config/mod.rs`:

  ```rust
  /// Per-agent wire idiom; picks the adapter used for TUI rendering.
  /// Defaults to `Generic` — safe fallback for unknown agents.
  #[serde(default)]
  pub kind: AgentKind,
  ```

  Update `AgentConfig::with_defaults` to set `kind: AgentKind::Generic`.

- [ ] **Step 1.2b: Update exhaustive `AgentConfig` struct literals (cascade)**

  None of the existing test fixtures use `..Default::default()`; adding a new field breaks compilation at every exhaustive literal. Verified sites (confirm with `rg 'AgentConfig\s*\{' --type rust` before editing):

  - `crates/spur-acp/tests/skip_permissions_config.rs` — 4 sites (around lines 42, 71, 93, 120).
  - `crates/spur-core/tests/skip_perm_helper.rs` — 1 site (around line 95).
  - `crates/spur-tui/tests/session_update_handling.rs` — 2 sites (around lines 107 and 180).

  At each site, insert a `kind: spur_acp::AgentKind::Generic,` line (or the agent-specific kind if the test asserts on it — today none do, so `Generic` is correct everywhere).

  Do **not** touch `AgentConfig::with_defaults` callers (`app.rs:226`, `commands/registry.rs:181`) — those use the helper and pick up the default automatically.

- [ ] **Step 1.3: Declare `kind` for every seed agent**

  In `crates/spur-acp/src/seed_agents.toml`:

  | Agent name | `kind` value |
  |---|---|
  | `claude-code` | `"claude-stream-json"` |
  | `kiro` | `"kiro"` |
  | `claude-code-acp` | `"claude-code-acp"` |
  | `codex` | `"codex-acp"` |
  | `codex-acp` | `"codex-acp"` |
  | `gemini` | `"generic"` |

- [ ] **Step 1.4: Re-export from `lib.rs`**

  ```rust
  pub use types::AgentKind;
  ```

- [ ] **Step 1.5: Verify**

  ```
  cargo test -p spur-acp
  cargo check -p spur-core -p spur-tui
  ```

  No behavior change expected; all tests must still pass. The seed-agent test at `config/mod.rs:483` still asserts the six names are present.

---

## Task 2: Adapter module skeleton (spur-acp)

**Files:**
- Create: `crates/spur-acp/src/adapter/mod.rs`, `adapter/mcp.rs`, `adapter/generic.rs`, plus per-kind stubs `adapter/{claude,codex,kiro}.rs` (bodies stay empty until Task 3).
- Modify: `crates/spur-acp/src/lib.rs`.

Goal: stand up the adapter API with all functions falling to generic behavior. Per-kind stub modules compile but every function returns `None`/passthrough. Task 3 replaces stub bodies. **No new workspace dependency** — per-kind overrides are plain `match` blocks.

- [ ] **Step 2.1: Define public types in `adapter/mod.rs`**

  Primary classification rides on ACP's own `ToolKind` enum; `ToolFamily` is a 1:1 superset that adds TUI-specific refinements (`Plan`, `Mcp`). Note: ACP `ToolCall` has `title: String` + `kind: ToolKind` but **no `name` field**, so `classify_tool` takes `&ToolCall`, not a name string.

  ```rust
  pub mod generic;
  pub mod mcp;
  pub mod claude;
  pub mod codex;
  pub mod kiro;

  use crate::types::AgentKind;
  use agent_client_protocol::{ToolCall, ToolKind};
  use serde_json::Value;

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ToolFamily {
      // 1:1 mirror of ACP ToolKind variants:
      Read, Edit, Delete, Move, Search, Execute, Think, Fetch, SwitchMode,
      // TUI-specific refinements produced by per-kind `refine(title, base)`:
      Plan,     // e.g. Claude TodoWrite, Codex plan_update
      Mcp,      // title starts with "mcp__" (MCP tool passthrough)
      Unknown,  // maps from ACP `Other` with no per-kind refinement
  }

  impl From<ToolKind> for ToolFamily {
      fn from(k: ToolKind) -> Self {
          match k {
              ToolKind::Read => ToolFamily::Read,
              ToolKind::Edit => ToolFamily::Edit,
              ToolKind::Delete => ToolFamily::Delete,
              ToolKind::Move => ToolFamily::Move,
              ToolKind::Search => ToolFamily::Search,
              ToolKind::Execute => ToolFamily::Execute,
              ToolKind::Think => ToolFamily::Think,
              ToolKind::Fetch => ToolFamily::Fetch,
              ToolKind::SwitchMode => ToolFamily::SwitchMode,
              ToolKind::Other => ToolFamily::Unknown,
          }
      }
  }

  #[derive(Debug, Clone)]
  pub enum ToolInputDisplay {
      Path(String),
      Diff { path: String, diff: String },
      Command { cmd: String, cwd: Option<String> },
      Query(String),
      Json(String),              // pretty-printed, truncated to 8 lines
      Text(String),
      /// Nothing meaningful to show — callers fall back to `TraceEntry.text`.
      Empty,
  }

  #[derive(Debug, Clone)]
  pub enum ObservePayload {
      CommandOutput { exit_code: Option<i32>, stdout: String, stderr: String },
      FileRead { path: Option<String>, content: String, truncated: bool },
      EditResult { path: Option<String>, replacements: Option<usize>, diff: Option<String> },
      Json { pretty: String },
      Text { body: String },
      Error { message: String },
  }

  #[derive(Debug, Clone)]
  pub struct ModeBadge {
      pub short: &'static str,   // "PLAN", "AUTO", "RO"
      pub color: BadgeColor,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum BadgeColor { Amber, Green, Red, Neutral }

  /// Classify a tool call. Takes `&ToolCall` because ACP tool identity is
  /// `(title, kind)` — there is no `name` field on the ACP `ToolCall` struct.
  /// Pipeline: `ToolKind → ToolFamily` (via `From`), then per-kind
  /// `refine(title, base)` which may upgrade `Unknown` → `Plan`/`Mcp`.
  /// Never panics.
  pub fn classify_tool(tc: &ToolCall, kind: AgentKind) -> ToolFamily {
      let base = ToolFamily::from(tc.kind);
      match kind {
          AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::refine(&tc.title, base),
          AgentKind::CodexAcp => codex::refine(&tc.title, base),
          AgentKind::Kiro     => kiro::refine(&tc.title, base),
          AgentKind::Generic  => generic::refine(&tc.title, base),
      }
  }

  /// Convert a ToolCall's `raw_input` JSON into a display-friendly form.
  /// Per-kind first; generic fallback.
  pub fn format_input(raw_input: &Value, kind: AgentKind) -> ToolInputDisplay {
      let per_kind = match kind {
          AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::try_format_input(raw_input),
          AgentKind::CodexAcp => codex::try_format_input(raw_input),
          AgentKind::Kiro     => kiro::try_format_input(raw_input),
          AgentKind::Generic  => None,
      };
      per_kind.unwrap_or_else(|| generic::format_input(raw_input))
  }

  /// Pipeline: MCP-envelope unwrap (shared) → per-kind extraction → generic fallback.
  pub fn extract_observe(raw_output: &Value, kind: AgentKind) -> ObservePayload {
      let unwrapped = mcp::unwrap_envelope(raw_output);
      let per_kind = match kind {
          AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::try_extract_observe(&unwrapped),
          AgentKind::CodexAcp => codex::try_extract_observe(&unwrapped),
          AgentKind::Kiro     => kiro::try_extract_observe(&unwrapped),
          AgentKind::Generic  => None,
      };
      per_kind.unwrap_or_else(|| generic::extract_observe(&unwrapped))
  }

  /// Translate `CurrentModeUpdate::current_mode_id` into a short badge.
  /// `None` = the kind has no known modes (callers hide the badge).
  pub fn mode_badge(mode_id: &str, kind: AgentKind) -> Option<ModeBadge> {
      match kind {
          AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::mode_badge(mode_id),
          AgentKind::CodexAcp => codex::mode_badge(mode_id),
          AgentKind::Kiro     => kiro::mode_badge(mode_id),
          AgentKind::Generic  => None,
      }
  }
  ```

- [ ] **Step 2.2: Implement `adapter/mcp.rs`** — multi-item aware

  ```rust
  use std::borrow::Cow;
  use serde_json::Value;

  /// Unwrap the standard MCP content-block envelope
  /// `{"items": [{"Json": J}, {"Text": T}, ...]}` into a single Value.
  ///
  /// Strategy (deterministic, documented):
  /// - Non-envelope (no `items` array) → passthrough.
  /// - 0 items → passthrough.
  /// - Single `Json` → the inner Json value.
  /// - Single `Text` → `Value::String(text)`.
  /// - Multi-item, all Text → concatenated `Value::String` joined by `\n`.
  /// - Multi-item, any Json → FIRST `Json` value with a `__truncated__: true`
  ///   sentinel merged into its object (if object) so downstream extractors
  ///   can flag partial data. If first Json is non-object, wrap in
  ///   `{"value": <original>, "__truncated__": true}`.
  /// - Unrecursive: only the outer envelope is unwrapped, never nested.
  pub fn unwrap_envelope(v: &Value) -> Cow<'_, Value> { /* implement + tests */ }
  ```

  Unit-test cases (each → its own `#[test]`):
  - 0-item envelope → passthrough, exactly `v` (Eq check).
  - single-Json envelope → inner Json.
  - single-Text envelope → `Value::String`.
  - multi-Text envelope → concatenated `Value::String` with `\n` joiner.
  - Text + Json mixed → first Json + `__truncated__: true`.
  - Json + Json mixed → first Json + `__truncated__: true`.
  - non-envelope `{}`, `[]`, `"hi"`, `null` → passthrough.
  - nested envelope (envelope whose Json item is itself an envelope) → only top-level unwrapped.

- [ ] **Step 2.3: Implement `adapter/generic.rs`** — shared signature with per-kind modules

  All per-kind modules and `generic` share identical function shapes so the Step 2.1 dispatch is uniform.

  ```rust
  use serde_json::Value;
  use super::{ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

  /// Generic `refine` — catches common `Other`-kinded tools the protocol
  /// didn't classify, via case-insensitive substring match on `title`.
  pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
      let low = title.to_ascii_lowercase();
      if low.starts_with("mcp__") { return ToolFamily::Mcp; }
      if matches!(base, ToolFamily::Unknown) {
          if low.contains("todo") || low.contains("plan") { return ToolFamily::Plan; }
      }
      base
  }

  /// Generic input formatter. Invoked only when the per-kind module returned None.
  pub fn format_input(raw: &Value) -> ToolInputDisplay {
      // Value::Null or {} → Empty
      // Object with "path"|"file_path"|"target"|"filename" → Path
      // Object with "command"|"cmd"                        → Command { cmd, cwd? }
      // Object with "pattern"|"query"                      → Query
      // String                                             → Text
      // else                                               → Json(pretty, truncate 8 lines)
      unimplemented!()
  }

  /// Generic observe extractor. Receives the MCP-unwrapped Value.
  pub fn extract_observe(raw: &Value) -> ObservePayload {
      // { exit_code|exitCode|status: number, stdout?, stderr? }  → CommandOutput
      // { content: string, path? }                              → FileRead
      // { diff | replacements | replaced }                      → EditResult
      // { error: true|"...", message: "..." }                   → Error
      // Value::String                                           → Text
      // Value::Null                                             → Text { body: "" }
      // else                                                    → Json { pretty }
      unimplemented!()
  }
  ```

- [ ] **Step 2.4: Create per-kind stub modules**

  Create `adapter/claude.rs`, `adapter/codex.rs`, `adapter/kiro.rs`, each with the same exported shape (bodies fleshed out in Task 3):

  ```rust
  use serde_json::Value;
  use super::{ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

  pub fn refine(_title: &str, base: ToolFamily) -> ToolFamily { base }
  pub fn try_format_input(_raw: &Value) -> Option<ToolInputDisplay> { None }
  pub fn try_extract_observe(_raw: &Value) -> Option<ObservePayload> { None }
  pub fn mode_badge(_mode_id: &str) -> Option<ModeBadge> { None }
  ```

  Dispatch in Step 2.1 now compiles and exercises the generic fallback for every kind.

- [ ] **Step 2.5: Expose module from lib and add smoke tests**

  `crates/spur-acp/src/lib.rs`: `pub mod adapter;`

  Add `crates/spur-acp/tests/adapter_smoke.rs`:
  - Build `ToolCall { kind: ToolKind::Read, title: "read_file".into(), .. }`; `classify_tool(&tc, Generic)` → `ToolFamily::Read`.
  - `ToolCall { kind: ToolKind::Other, title: "mcp__srv__foo".into(), .. }`; → `ToolFamily::Mcp`.
  - `ToolCall { kind: ToolKind::Other, title: "TodoWrite".into(), .. }`; → `ToolFamily::Plan`.
  - `extract_observe(&json!("hello"), Generic)` → `ObservePayload::Text { body: "hello" }`.
  - `extract_observe(&json!({"exit_code": 0, "stdout": "ok", "stderr": ""}), Generic)` → `CommandOutput { exit_code: Some(0), .. }`.
  - `unwrap_envelope` cases from Step 2.2 bullet list.

- [ ] **Step 2.6: Verify**

  ```
  cargo test -p spur-acp
  cargo check --workspace
  ```

---

## Task 3: Per-kind modules (spur-acp)

**Files:**
- Modify: `crates/spur-acp/src/adapter/{claude,codex,kiro}.rs` (stubs created in Task 2.4).

Goal: replace stub bodies with real `refine` / `try_format_input` / `try_extract_observe` / `mode_badge` implementations. **No phf tables** — primary classification rides on ACP `ToolKind`; per-kind logic is override-only.

- [ ] **Step 3.1: `adapter/claude.rs`** (covers `ClaudeStreamJson` + `ClaudeCodeAcp`)

  ```rust
  use serde_json::Value;
  use super::{BadgeColor, ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

  /// Upgrade `Unknown` (ACP `Other`) to a TUI-specific family when the Claude
  /// title is recognizable. Don't downgrade a protocol-given kind.
  pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
      if title.starts_with("mcp__") { return ToolFamily::Mcp; }
      if matches!(base, ToolFamily::Unknown) {
          return match title {
              "TodoWrite" => ToolFamily::Plan,
              "Task"      => ToolFamily::Unknown, // subagent dispatch — keep
              _           => base,
          };
      }
      base
  }

  /// `Edit` / `MultiEdit` / `Write` → Diff; `Bash` → Command; `Read` → Path.
  /// Inspect raw_input by shape; title is only a tie-breaker.
  pub fn try_format_input(raw: &Value) -> Option<ToolInputDisplay> {
      let obj = raw.as_object()?;
      if let (Some(p), Some(old), Some(new)) = (
          obj.get("file_path").and_then(|v| v.as_str()),
          obj.get("old_string").and_then(|v| v.as_str()),
          obj.get("new_string").and_then(|v| v.as_str()),
      ) {
          return Some(ToolInputDisplay::Diff { path: p.into(), diff: make_unified(p, old, new) });
      }
      if let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) {
          return Some(ToolInputDisplay::Command { cmd: cmd.into(), cwd: None });
      }
      if let Some(p) = obj.get("file_path").and_then(|v| v.as_str()) {
          return Some(ToolInputDisplay::Path(p.into()));
      }
      if let Some(pat) = obj.get("pattern").and_then(|v| v.as_str()) {
          return Some(ToolInputDisplay::Query(pat.into()));
      }
      None
  }

  /// Bash tool output: `{stdout, stderr, exit_code}` (when Claude echoes structured).
  /// Otherwise: plain string body or fall back to generic.
  pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload> { /* ... */ }

  pub fn mode_badge(mode_id: &str) -> Option<ModeBadge> {
      Some(match mode_id {
          "plan"              => ModeBadge { short: "PLAN", color: BadgeColor::Amber },
          "acceptEdits"       => ModeBadge { short: "AUTO", color: BadgeColor::Green },
          "bypassPermissions" => ModeBadge { short: "BYPASS", color: BadgeColor::Red },
          "default"           => return None,
          _                   => return None,
      })
  }

  fn make_unified(path: &str, old: &str, new: &str) -> String { /* minimal diff */ }
  ```

- [ ] **Step 3.2: `adapter/codex.rs`**

  ```rust
  pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
      if title.starts_with("mcp__") { return ToolFamily::Mcp; }
      if matches!(base, ToolFamily::Unknown) {
          return match title {
              "plan_update" => ToolFamily::Plan,
              _             => base,
          };
      }
      base
  }

  pub fn try_format_input(raw: &Value) -> Option<ToolInputDisplay> {
      let obj = raw.as_object()?;
      if let Some(cmd) = obj.get("cmd").and_then(|v| v.as_str()) {
          let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
          return Some(ToolInputDisplay::Command { cmd: cmd.into(), cwd });
      }
      if let Some(patch) = obj.get("patch").and_then(|v| v.as_str()) {
          return Some(ToolInputDisplay::Diff { path: "<patch>".into(), diff: patch.into() });
      }
      None
  }

  pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload> {
      let obj = raw.as_object()?;
      let exit = obj.get("exit_code").and_then(|v| v.as_i64()).map(|v| v as i32);
      let stdout = obj.get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string();
      let stderr = obj.get("stderr").and_then(|v| v.as_str()).unwrap_or("").to_string();
      if exit.is_some() || !stdout.is_empty() || !stderr.is_empty() {
          return Some(ObservePayload::CommandOutput { exit_code: exit, stdout, stderr });
      }
      None
  }

  pub fn mode_badge(mode_id: &str) -> Option<ModeBadge> {
      Some(match mode_id {
          "full-auto"   => ModeBadge { short: "AUTO", color: BadgeColor::Green },
          "read-only"   => ModeBadge { short: "RO",   color: BadgeColor::Neutral },
          "on-failure"  => ModeBadge { short: "ONFAIL", color: BadgeColor::Amber },
          "on-request"  => ModeBadge { short: "ASK",  color: BadgeColor::Amber },
          _             => return None,
      })
  }
  ```

- [ ] **Step 3.3: `adapter/kiro.rs`**

  Kiro's tool vocabulary is less documented publicly and no session fixtures exist yet. Start minimal: `refine` returns `ToolFamily::Mcp` for `mcp__` titles (covered by generic anyway; left explicit for symmetry), no input/observe extraction beyond generic, no `mode_badge` (kiro uses `--trust-all-tools`, not session modes). Expand as fixtures are authored in Task 4.

- [ ] **Step 3.4: Idempotence compliance test**

  Add one test per kind asserting `refine(title, refine(title, base)) == refine(title, base)` for a small title corpus. Cheap regression guard.

- [ ] **Step 3.5: Verify**

  ```
  cargo test -p spur-acp
  ```

---

## Task 4: Fixture corpus + replay tests (spur-acp)

**Files:**
- Create: `crates/spur-acp/tests/fixtures/notifications/{claude-code-acp,codex-acp,kiro}/*.json`
- Create: `crates/spur-acp/tests/adapter_fixtures.rs`
- Modify (optional, Step 4.1b only): `crates/spur-acp/Cargo.toml` (add `record-fixtures` feature), `crates/spur-acp/src/connection/native.rs` (~15 LOC feature-gated tee).

Goal: lock in expected adapter output per known-good `SessionNotification`, so upstream agent drift is caught by CI rather than in user traces.

**Note on prior approach:** an earlier draft proposed mining `.spur/logs/` for raw notifications. Ground-check confirmed the per-session `*-acp.log` files are **empty** (stderr taps only); the 200 MB structured-trace files at `.spur/logs/spur.log.YYYY-MM-DD` are not a practical source either. We take the hand-author path below, with an optional record-mode escape hatch for ongoing capture.

- [ ] **Step 4.1: Hand-author the initial fixtures**

  Author ~3 fixtures per kind from the ACP schema docs + the seed agent's public tool surface. Minimum coverage target is 9 files:

  ```
  tests/fixtures/notifications/
    claude-code-acp/
      tool_call_bash.json         # SessionUpdate::ToolCall with ToolKind::Execute
      tool_update_bash_exit0.json # SessionUpdate::ToolCallUpdate with command output
      tool_call_edit.json         # SessionUpdate::ToolCall with ToolKind::Edit + diff input
    codex-acp/
      tool_call_exec.json         # exec_command
      tool_update_exec_exit0.json # {exit_code, stdout, stderr}
      tool_call_apply_patch.json
    kiro/
      tool_call_mcp.json          # mcp__server__foo title (covers Mcp refinement)
      tool_update_mcp_envelope.json   # {"items":[{"Json":{...}}]} shape
      tool_call_generic.json      # ToolKind::Other with no recognizable title
  ```

  Each file is a complete `SessionNotification` JSON (session_id + update + optional _meta). Use ACP schema docs to confirm field names. Do **not** embed real user paths — use `/tmp/foo.rs`, `repo/src/bar.rs`, etc.

- [ ] **Step 4.1b: Optional record-mode for ongoing capture**

  Add a cargo feature `record-fixtures = []` on `spur-acp` and, gated on it, a ~15-LOC tee in `NativeAcpConnection`'s receive loop that writes each `SessionNotification` to `$SPUR_RECORD_FIXTURES/<uuid>.json` when the env var is set. Default builds are untouched (no new dependency, no runtime cost when feature is off).

  Recommended only if the hand-authored corpus proves insufficient; not required for v1.

- [ ] **Step 4.2: Write replay tests**

  `crates/spur-acp/tests/adapter_fixtures.rs`:

  ```rust
  use spur_acp::adapter::{self, ObservePayload, ToolFamily};
  use spur_acp::AgentKind;
  use agent_client_protocol::{SessionNotification, SessionUpdate};

  fn load(rel: &str) -> SessionNotification {
      let path = format!("{}/tests/fixtures/notifications/{}", env!("CARGO_MANIFEST_DIR"), rel);
      let json = std::fs::read_to_string(&path).expect("read fixture");
      serde_json::from_str(&json).expect("parse fixture")
  }

  #[test]
  fn claude_bash_tool_classifies_as_execute() {
      let n = load("claude-code-acp/tool_call_bash.json");
      let SessionUpdate::ToolCall(tc) = &n.update else { panic!("expected ToolCall") };
      assert_eq!(adapter::classify_tool(tc, AgentKind::ClaudeCodeAcp), ToolFamily::Execute);
  }

  #[test]
  fn codex_exec_output_extracts_as_command_output() {
      let n = load("codex-acp/tool_update_exec_exit0.json");
      let SessionUpdate::ToolCallUpdate(tcu) = &n.update else { panic!("expected ToolCallUpdate") };
      let raw = tcu.fields.raw_output.as_ref().expect("raw_output present");
      let p = adapter::extract_observe(raw, AgentKind::CodexAcp);
      match p {
          ObservePayload::CommandOutput { exit_code: Some(0), .. } => {}
          other => panic!("expected CommandOutput exit 0, got {other:?}"),
      }
  }

  // …one test per fixture per adapter axis.
  ```

  Coverage target: each fixture has ≥1 assertion on `classify_tool` OR `extract_observe` (whichever applies to its `SessionUpdate` variant).

- [ ] **Step 4.3: Privacy scrub**

  Grep for `/Users/`, `/home/`, or the committer's username across `tests/fixtures/notifications/` before committing; replace with synthetic paths. One-time check per PR.

- [ ] **Step 4.4: Verify**

  ```
  cargo test -p spur-acp --test adapter_fixtures
  ```

---

## Task 5: TraceKind evolution (spur-tui)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs`

Goal: evolve `TraceKind::Act` and `TraceKind::Observe` to single-source-of-truth shapes. **Current state:** `TraceKind::Observe` is a *unit* variant (no fields); body text lives on `TraceEntry.text`. `TraceKind::Act { tool: String, args: String }` has its own string duplication. After this task, both variants carry only the structured view; `TraceEntry.text` remains the raw fallback when the structured view is absent — no shadow strings.

- [ ] **Step 5.1: Change variants (drop dual-truth fields)**

  ```rust
  use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};

  pub enum TraceKind {
      Think,
      AgentMessage { agent: String },
      Act {
          tool: String,               // the ACP ToolCall.title (human-readable)
          family: ToolFamily,         // NEW, default ToolFamily::Unknown
          input: ToolInputDisplay,    // NEW, default ToolInputDisplay::Empty
          // `args: String` DROPPED — when input is Empty, renderer falls back
          // to TraceEntry.text for the raw-ish description.
      },
      Observe {
          payload: Option<ObservePayload>, // NEW; None → renderer uses TraceEntry.text
          // no `text: String` here — Observe was a unit variant before this task.
      },
      Delegate { .. },    // unchanged (worker scope is out)
      UserMessage,
      Permission { .. },
  }
  ```

  Rationale: in the prior draft we kept `args: String` and `text: String` as "legacy fallback" fields. Ground-check flagged this as dual source of truth — a bug farm (structured view says A, raw string says B; logs read one, renderer reads the other). Single source of truth: the structured field is authoritative when present; `TraceEntry.text` is the single raw fallback.

- [ ] **Step 5.2: Update all `TraceKind::Act` / `TraceKind::Observe` construction sites**

  Verify sites with `rg 'TraceKind::(Act|Observe)' crates/spur-tui`. Current call sites: `views/session_detail.rs` and `components/react_trace.rs` (construction inside `append_think`, `append_message`, etc. uses other variants — double-check).

  At each `TraceKind::Act` construction site:
  - Move whatever was stored in `args: String` into `TraceEntry.text` **if and only if** no `input: ToolInputDisplay` is being provided. Task 6 will populate `input` from the adapter, so most sites set `input: ToolInputDisplay::Empty` temporarily and let Task 6 fill it.

  At each `TraceKind::Observe` construction site:
  - Keep storing the raw body in `TraceEntry.text` as today. Add `payload: None`. Task 6 will populate `payload` from the adapter.

- [ ] **Step 5.3: Update `build_display_lines` / `build_virtual_rows` fallbacks**

  These functions pattern-match `TraceKind::Act { tool, args, .. }` and `TraceKind::Observe` today. Change them to:
  - For `Act`: read `input` when non-`Empty`, else fall back to `entry.text`.
  - For `Observe`: read `payload` when `Some`, else fall back to `entry.text`.

  Rendering details (glyphs, colors) stay unchanged in this task — Task 7 changes those. This task only swaps the field being read.

- [ ] **Step 5.4: Verify**

  ```
  cargo test -p spur-tui
  cargo check --workspace
  ```

  Output should look identical to before (`text` is still being rendered — just sourced from one field instead of two).

---

## Task 6: session_detail.rs adapter integration (spur-tui)

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

Goal: populate the new `TraceKind` fields at event-mapping time using the adapter. The render still looks identical after this task — the machinery is plumbed but the display is unchanged (Task 7 renders the new fields).

- [ ] **Step 6.1: Derive `AgentKind` once**

  Add an accessor on `SessionDetailView`:
  ```rust
  fn agent_kind(&self) -> AgentKind { self.agent_cfg.kind }
  ```

- [ ] **Step 6.2: Populate `Act` on `ToolCall`** (around `session_detail.rs:914-927`)

  Note: ACP's `ToolCall` has `title` + `kind` but **no `name` field**. `classify_tool` takes `&ToolCall`, not a name string.

  ```rust
  spur_acp::SessionUpdate::ToolCall(tc) => {
      use spur_acp::adapter::{self, ToolInputDisplay};
      let kind = self.agent_kind();
      let family = adapter::classify_tool(tc, kind);
      let input = tc.raw_input.as_ref()
          .map(|v| adapter::format_input(v, kind))
          .unwrap_or(ToolInputDisplay::Empty);
      // Fallback text for when `input` is Empty. Keep using the existing
      // `format_tool_args` helper for now — Task 8 decides whether to delete it.
      let fallback_text = tc.raw_input.as_ref()
          .map(|v| format_tool_args(v))
          .unwrap_or_default();
      self.react_trace.push(TraceEntry {
          kind: TraceKind::Act { tool: tc.title.clone(), family, input },
          text: fallback_text,
          timestamp: Self::now_stamp(),
          #[cfg(feature = "markdown")]
          markdown: None,
      });
  }
  ```

- [ ] **Step 6.3: Populate `Observe` on `ToolCallUpdate`** (around `session_detail.rs:928-938`)

  ```rust
  spur_acp::SessionUpdate::ToolCallUpdate(tcu) => {
      use spur_acp::adapter;
      let kind = self.agent_kind();
      let payload = tcu.fields.raw_output.as_ref()
          .map(|v| adapter::extract_observe(v, kind));
      let fallback_text = tcu.fields.raw_output.as_ref()
          .map(|v| format_observe_output(v))
          .unwrap_or_default();
      self.react_trace.push(TraceEntry {
          kind: TraceKind::Observe { payload },
          text: fallback_text,
          timestamp: Self::now_stamp(),
          #[cfg(feature = "markdown")]
          markdown: None,
      });
  }
  ```

- [ ] **Step 6.4: Stash mode badge on `CurrentModeUpdate`**

  Already stored as `self.current_mode: Option<String>`. Leave the string as-is; Task 7 calls `adapter::mode_badge(&mode, self.agent_kind())` at render time to translate.

- [ ] **Step 6.5: Verify**

  ```
  cargo test -p spur-tui
  ```

  Existing snapshot/render tests should still pass because Task 7 hasn't changed the rendering yet.

---

## Task 7: react_trace.rs render updates (spur-tui)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs`

Goal: consume the new fields. This is the first task that changes visible output.

- [ ] **Step 7.1: Replace `🔧 ACT  {tool}` header with family-aware glyph**

  Helper — variants match the post-Task-2 `ToolFamily` (ACP `ToolKind` 1:1 + `Plan`/`Mcp`/`Unknown`):
  ```rust
  fn family_glyph(f: ToolFamily) -> (&'static str, Color) {
      match f {
          ToolFamily::Read       => ("⚙ reads",   Color::Cyan),
          ToolFamily::Edit       => ("✎ edits",   Color::Yellow),
          ToolFamily::Delete     => ("✗ deletes", Color::Red),
          ToolFamily::Move       => ("→ moves",   Color::Yellow),
          ToolFamily::Search     => ("🔎 search", Color::Blue),
          ToolFamily::Execute    => ("$ runs",    Color::Magenta),
          ToolFamily::Think      => ("◈ thinks",  Color::DarkGray),
          ToolFamily::Fetch      => ("↯ fetch",   Color::Blue),
          ToolFamily::SwitchMode => ("⇄ mode",    Color::Cyan),
          ToolFamily::Plan       => ("▸ plan",    Color::Cyan),
          ToolFamily::Mcp        => ("⧉ mcp",     Color::DarkGray),
          ToolFamily::Unknown    => ("🔧 ACT",    Color::Yellow),
      }
  }
  ```

  Render in both `build_display_lines` (~L648 at plan-writing time) and `build_virtual_rows` (~L1000). Consider extracting to a single shared helper to avoid drift (already flagged as tech debt in the prior `react_trace.rs` review); at minimum, put `family_glyph` in one place and call it from both.

- [ ] **Step 7.2: Render `ToolInputDisplay` instead of raw `args` when non-Empty**

  - `Path(p)` → single indented line, dim
  - `Diff { path, diff }` → path header + first ~6 diff lines, red/green tinted; `[… N more]` if truncated
  - `Command { cmd, cwd }` → `$ cmd` line with optional `(cwd: …)`
  - `Query(q)` → `q` in italic
  - `Json(p)` → pretty-printed, truncated at 8 lines
  - `Text(t)` → plain indented
  - `Empty` → nothing (just the header row)

- [ ] **Step 7.3: Render `ObservePayload` with outcome glyph**

  Helper for outcome glyph:
  ```rust
  fn outcome_glyph(p: &ObservePayload) -> (&'static str, Color) {
      match p {
          ObservePayload::CommandOutput { exit_code: Some(0), .. } => ("✓", Color::Green),
          ObservePayload::CommandOutput { exit_code: Some(_), .. } => ("✗", Color::Red),
          ObservePayload::Error { .. } => ("✗", Color::Red),
          _ => ("✓", Color::Green),
      }
  }
  ```

  Header becomes `{timestamp} {glyph} {family-verb-past-tense or fallback "observed"}`.

  Body per variant:
  - `CommandOutput` → `$ exit N` + stdout (truncated to 8 lines) + stderr (red, truncated)
  - `FileRead { path, content, truncated }` → `path · N lines` header + content (truncated)
  - `EditResult { replacements, diff }` → "N replacements" or diff (first 6 lines)
  - `Json { pretty }` → pretty-printed (truncated at 8 lines; fold-hint `[… expand]`)
  - `Text { body }` → indented text
  - `Error { message }` → red message

  When `payload` is `None`, render `text` as today (backwards-compatible fallback).

- [ ] **Step 7.4: Pane title + brain signature tint**

  `render` / `render_with_ctx`:

  ```rust
  let title = match self.agent_kind {
      AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => " Session · claude ",
      AgentKind::CodexAcp      => " Session · codex ",
      AgentKind::Kiro          => " Session · kiro ",
      AgentKind::Generic       => " Session ",
  };
  let title_color = match self.agent_kind { /* palette */ };
  block = block.title(title).title_style(Style::default().fg(title_color));
  ```

  `ReactTrace` gains an `agent_kind: AgentKind` field set by a new constructor `ReactTrace::with_kind(kind: AgentKind)`. `SessionDetailView::new` passes `agent_cfg.kind` in.

- [ ] **Step 7.5: Mode badge in pane title when present**

  ```rust
  if let Some(mode_id) = &self.current_mode {
      if let Some(badge) = adapter::mode_badge(mode_id, self.agent_kind) {
          title = format!("{}· {} ", title, badge.short);
          // apply badge.color to a specific span if the title supports it
      }
  }
  ```

  Keep minimal: just appending to the title string is fine for v1.

- [ ] **Step 7.6: Golden-file rendering tests (CI-verifiable)**

  Manual smoke against two real agents is great but requires both installed locally. Add a CI-verifiable golden-file test that feeds a fixture notification through the session_detail → react_trace pipeline and string-compares the rendered lines against a committed `.golden` file. Plain `std::fs::read_to_string` + `assert_eq!` — no new dev-dep.

  (If a later PR wants prettier snapshot UX, `insta` can be added as a dev-dep on `spur-tui` then. It is **not** currently a workspace dep — verified against `/Volumes/Projects/spur/Cargo.toml` during ground-check — so don't rely on it here.)

  ```rust
  // crates/spur-tui/tests/render_golden.rs
  #[test]
  fn codex_exec_output_renders_with_outcome_glyph() {
      let notif = load_acp_fixture("codex-acp/tool_update_exec_exit0.json");
      let view = build_session_view_for_test(AgentKind::CodexAcp);
      view.apply_notification(&notif);
      let actual = view.render_trace_to_strings(80 /* width */).join("\n");
      let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/codex_exec.txt");
      if std::env::var("UPDATE_GOLDEN").is_ok() {
          std::fs::write(golden_path, &actual).unwrap();
      }
      let expected = std::fs::read_to_string(golden_path).expect("golden file");
      assert_eq!(actual, expected, "re-record with UPDATE_GOLDEN=1");
  }
  ```

  New files: `crates/spur-tui/tests/golden/*.txt` (one per test case). No new dep. `UPDATE_GOLDEN=1 cargo test -p spur-tui` rewrites goldens; commit the result.

  Cover at least: claude-code-acp tool-call-edit render; codex-acp exec-exit0 render; generic mcp-envelope render (the `{"items":[{"Json":...}]}` case from the user-pasted sample) — must become a two-line `✓ exit 0` / `done` block.

- [ ] **Step 7.7: Manual smoke (when agents are reachable)**

  ```
  cargo run -p spur-cli   # against claude-code-acp, then codex-acp
  ```

  Inspection checklist:
  - Pane title shows brain name + mode badge when applicable.
  - Tool calls render with family verb + glyph (not `🔧 ACT`).
  - Shell output shows `✓ exit 0` + stdout, no `{"items":[{"Json":...}]}` noise.
  - Edit calls show a short diff instead of raw JSON args.
  - Switching brain (quit, run with different `kind`) changes pane signature color.

- [ ] **Step 7.8: Verify**

  ```
  cargo test -p spur-tui
  cargo test -p spur-tui --test render_golden
  ```

---

## Task 8: Cleanup + documentation

**Files:**
- Modify: `docs/spur/agent-onboarding-cookbook.md` — document the `kind` field and how to add a new `AgentKind`. Verified this file exists.
- Delete (optional): `session_detail.rs::format_tool_args` and `format_observe_output` once every caller routes through `TraceEntry.text` fallback and no test grep matches them.

- [ ] **Step 8.1: Update cookbook**

  Add section "Choosing an AgentKind" with the table from Task 1.3 and guidance for custom agents ("use `generic`; file an issue to upstream a per-kind module if your agent's tools are widely used").

- [ ] **Step 8.2: Remove legacy formatters if fully replaced**

  Only do this when neither the render path in `react_trace.rs` nor any test reads the helpers' output directly. Concretely: `rg 'format_tool_args|format_observe_output' crates/` returns only the helpers themselves and their Task-6 call sites. If Task 6's fallback_text path is still alive, delete these helpers and inline `serde_json::to_string_pretty` (or similar) at the remaining site.

- [ ] **Step 8.3: Final verification**

  ```
  cargo fmt --all
  cargo clippy --workspace --all-features -- -D warnings
  cargo test --workspace
  ```

---

## Review checkpoints

After each task, before marking `completed`:

1. `cargo test -p <touched-crate>` passes.
2. `cargo clippy -p <touched-crate> -- -D warnings` is clean.
3. The "Files untouched (invariant)" list is still intact. Grep to confirm no edits leaked into `orchestrator.rs`, `domain/events.rs`, or the default-build path of `connection/native.rs` (a `#[cfg(feature = "record-fixtures")]` tee added in Task 4.1b is the only exception).
4. No `AgentKind` inference heuristics sneaked in — explicit TOML only.
5. No new workspace dependency was added (`phf` was considered and rejected during ground-check).
6. `TraceKind::Act` and `TraceKind::Observe` have no dual-source-of-truth fields: `args: String` and `Observe.text: String` must not reappear.
7. Per-kind `refine` functions pass the idempotence test from Step 3.4.

---

## Success criteria

- All six seed agents load with their declared `kind`; unknown kinds fall back to `Generic` with no panics.
- Running `spur` against `claude-code-acp`, `codex-acp`, and (if reachable) `kiro` produces visibly distinct pane signatures (title color) and correctly classifies their tool calls per the fixture tests.
- The pasted sample's OBSERVE noise (`{"items":[{"Json":{"exit_status":"exit status: 0","stdout":"done\n"}}]}`) renders as a two-line `✓ exit 0` / `done` block — asserted by a golden-file test (Step 7.6), not just by eye.
- `cargo test --workspace` passes.
- `SpurEventBody`, `orchestrator.rs`, and the default-build path of `NativeAcpConnection` are untouched (grep-verifiable invariant).
- No new workspace dependency added (verified against workspace `Cargo.toml`).
