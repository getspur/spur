# ACP SDK upgrade + codex `/model` & `/effort` pickers — paired implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land two coupled changes: (Phase 1) bump `agent-client-protocol` from `0.10.4` to `0.11.1` with the new `agent_client_protocol::schema::*` namespace; (Phase 2) ship the v1 codex `/model` and `/effort` slash-command pickers using v2-aligned type shapes so v2 PRs add only new variants on top.

**Architecture:** Phase 1 is mechanical — workspace dep bump plus `use agent_client_protocol::{X}` → `use agent_client_protocol::schema::{X}` rewrites across ~30 files. Phase 2 introduces a vendor-neutral synthesizer in `spur-acp` that turns cached `Vec<SessionConfigOption>` into `CommandEntry`s tagged `CommandSource::Advertised`, plus a generic `ArgPickerSpec` lookup driving a fuzzy-search `ConfigOptionQuerySource` modeled on the existing `@`-mention picker. The detector state machine gains one new `TriggerKind::SlashArg { command_name }` variant; `InputCompletionPort` dispatches on the cached `ArgPickerSpec.typed_hint` to instantiate the right `QuerySource`.

**Tech Stack:** Rust 2021. Workspace cargo. `agent-client-protocol` crate (Zed). `nucleo_matcher` for fuzzy filtering (already in `crates/spur-tui`). `tokio` async + broadcast channels. `serde` + `serde_json`.

**Specs:**
- v1: `docs/superpowers/specs/2026-04-27-codex-model-effort-slash-pickers-design.md`
- v2 (informs v1 type shapes): `docs/superpowers/specs/2026-04-27-acp-first-arg-pickers-v2-design.md`
- A2A reconciliation (informational, no code impact): `docs/superpowers/specs/2026-04-27-a2a-acp-reconciliation-design.md`

**Out of scope** (explicitly):
- v2 features (`/review`, `/review-branch`, `/review-commit`, `ConfigOptionUpdate` arm) — separate plan after this one lands.
- Surfacing `mode` (Approval Preset) selector — collides with spur-local `/mode`; deferred per v1 §2 non-goals.
- Any of the `Op::OverrideTurnContext` knobs codex-acp doesn't expose (`personality`, `service_tier`, `summary`, etc.) — upstream-gated per v1+v2 §11.

---

## Phase 1 — SDK upgrade `0.10.4 → 0.11.1` (PR-0)

**Why first:** Phase 2 needs `unstable_session_model` feature flag, which doesn't exist on `0.10.4`. The 0.11.x reorganization (schema types moved into `agent_client_protocol::schema::*`) must be absorbed across the codebase before any new ACP-touching code lands.

### Task 1.1: Capture green-tree baseline

**Files:** none modified

- [ ] **Step 1: Confirm clean working tree**

Run: `git status --short`
Expected: only the spec commit `3def8557` ahead of any baseline; no uncommitted changes.

- [ ] **Step 2: Run baseline test suite to capture current pass set**

Run: `cargo test --workspace --no-fail-fast 2>&1 | tee /tmp/spur-baseline-tests.log; tail -20 /tmp/spur-baseline-tests.log`
Expected: a passing baseline (or a known set of pre-existing failures). Save the tail showing "test result: …" lines from each crate.

- [ ] **Step 3: Sanity-check workspace builds clean**

Run: `cargo check --workspace 2>&1 | tail -5`
Expected: `Finished` with no errors.

If baseline is already failing, stop and surface the failures before continuing.

### Task 1.2: Bump `Cargo.toml` and add `unstable_session_model` feature

**Files:**
- Modify: `Cargo.toml:82`

- [ ] **Step 1: Edit the workspace dep**

Change `Cargo.toml:82` from:
```toml
agent-client-protocol = { version = "0.10", features = ["unstable_session_usage"] }
```
to:
```toml
agent-client-protocol = { version = "0.11", features = ["unstable_session_usage", "unstable_session_model"] }
```

- [ ] **Step 2: Run cargo check to surface the import errors**

Run: `cargo check --workspace 2>&1 | grep -c "^error\[E0432\]"`
Expected: a positive integer (typically 13). Save the full output:
```sh
cargo check --workspace 2>&1 > /tmp/spur-upgrade-errors.log
```

- [ ] **Step 3: Confirm errors are import-only (no behavioural drift)**

Run: `grep -c "^error\[" /tmp/spur-upgrade-errors.log; grep -c "^error\[E0432\]" /tmp/spur-upgrade-errors.log`
Both numbers should match. If they don't, an unexpected non-import error exists — investigate before proceeding.

### Task 1.3: Inventory the affected files

**Files:** none modified — produces a working list.

- [ ] **Step 1: Extract the unique file list from cargo errors**

Run:
```sh
grep "^  --> " /tmp/spur-upgrade-errors.log | sed 's/^  --> //' | cut -d: -f1 | sort -u | tee /tmp/spur-upgrade-files.txt
```
Expected: ~30 file paths under `crates/`.

- [ ] **Step 2: Confirm count matches the spec estimate**

Run: `wc -l /tmp/spur-upgrade-files.txt`
Expected: between 25 and 40. If far outside that range, the spec estimate was wrong — note the actual count for the commit message.

### Task 1.4: Symbol-mapping cheatsheet (reference document for the rewrites)

**Files:** none modified — creates a reference at `/tmp/spur-symbol-map.md`.

- [ ] **Step 1: Build the symbol → namespace map**

Read the cargo error output and build this exact reference file:

```sh
cat > /tmp/spur-symbol-map.md <<'EOF'
# agent-client-protocol 0.10 → 0.11 symbol-namespace map

## Symbols that MOVED to `agent_client_protocol::schema::`
(Update `use agent_client_protocol::{X}` → `use agent_client_protocol::schema::{X}`)

- AuthMethodId, AuthenticateRequest, AuthenticateResponse
- AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate
- ContentBlock, ContentChunk, CurrentModeUpdate
- ExtNotification, ExtRequest, ExtResponse
- ListSessionsRequest, ListSessionsResponse
- LoadSessionRequest
- PermissionOption, PermissionOptionId, PermissionOptionKind
- Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus
- RequestPermissionOutcome, RequestPermissionRequest
- ResourceLink
- SelectedPermissionOutcome
- SessionId, SessionInfo, SessionModeId, SessionNotification, SessionUpdate
- SetSessionModeRequest, SetSessionModeResponse
- TextContent
- ToolCall, ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus
- ToolCallUpdate, ToolCallUpdateFields
- ToolKind
- UnstructuredCommandInput
- UsageUpdate
- (any other symbol whose error message says "no `<X>` in the root" with hint pointing to `schema::`)

## Symbols that STAY at the crate root
(Do NOT change these — they are still re-exported at `agent_client_protocol::`)

- Agent, Client (traits)
- AgentNotification, ClientNotification (top-level RPC enums)
- ClientRequest, AgentResponse
- ConnectTo, ConnectionTo
- Error (the protocol's Error type)
- ByteStreams
- (anything not in the "moved" list above)

## How to apply per file
1. Find the line `use agent_client_protocol::{...}`.
2. Split it into TWO use statements: one for symbols that moved, one for symbols that stay.
3. Verify with `cargo check -p <crate>` that the file compiles.
EOF
cat /tmp/spur-symbol-map.md
```

- [ ] **Step 2: Sanity-check the cheatsheet against actual errors**

Run:
```sh
grep "no \`[A-Z][A-Za-z]*\` in the root" /tmp/spur-upgrade-errors.log | sed 's/.*no `\([A-Z][A-Za-z]*\)`.*/\1/' | sort -u | tee /tmp/spur-actually-moved.txt
```
Expected: every symbol in this list should appear in the "moved" section of the cheatsheet. If any are missing, append them.

### Task 1.5: Rewrite imports in `crates/spur-acp/src/lib.rs`

**Files:**
- Modify: `crates/spur-acp/src/lib.rs:46-54` (the giant `use` block)

This file has 40+ symbols on one `use`; it's the largest single file in the rewrite.

- [ ] **Step 1: Read the current `use` block**

Run: `sed -n '44,60p' crates/spur-acp/src/lib.rs`
Note the exact line numbers and which symbols are imported.

- [ ] **Step 2: Split the `use` into two statements**

Replace the existing `use agent_client_protocol::{...}` block with two statements: one importing schema-moved symbols from `agent_client_protocol::schema::{...}`, and one importing crate-root-stay symbols from `agent_client_protocol::{...}`. Use the cheatsheet at `/tmp/spur-symbol-map.md` as the reference.

If the original was alphabetically sorted, keep both new lists alphabetically sorted (matches existing style).

- [ ] **Step 3: Run cargo check on this crate alone**

Run: `cargo check -p spur-acp 2>&1 | tail -30`
Expected: errors confined to OTHER files in the crate (or zero errors if this is the only spur-acp file affected). The `lib.rs` line should no longer appear.

- [ ] **Step 4: Commit this single-file change**

Run:
```sh
git add crates/spur-acp/src/lib.rs
git commit -m "refactor(spur-acp): use schema:: namespace for ACP types in lib.rs

Part of agent-client-protocol 0.10→0.11 upgrade. Schema types moved
to a submodule in 0.11; runtime items (Agent/Client/ConnectTo) stay
at root.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.6: Rewrite imports in `crates/spur-acp/src/protocol/claude_events.rs`

**Files:**
- Modify: `crates/spur-acp/src/protocol/claude_events.rs:8-9`

- [ ] **Step 1: Read the current `use` block**

Run: `sed -n '6,12p' crates/spur-acp/src/protocol/claude_events.rs`

- [ ] **Step 2: Split the `use` per cheatsheet**

Apply the same two-statement split: schema-moved symbols → `agent_client_protocol::schema::`, root-stay symbols → `agent_client_protocol::`.

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p spur-acp 2>&1 | grep -c "^error\["`
Expected: count drops by however many errors this file held (typically 1-2).

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-acp/src/protocol/claude_events.rs
git commit -m "refactor(spur-acp): use schema:: namespace in protocol/claude_events.rs

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.7: Rewrite imports in remaining `crates/spur-acp` files

**Files (per `/tmp/spur-upgrade-files.txt`, the spur-acp portion):**
- Modify each spur-acp file flagged by cargo, one at a time:
  - `crates/spur-acp/src/connection/stream_json_adapter.rs`
  - `crates/spur-acp/src/connection/stdio_adapter.rs`
  - `crates/spur-acp/src/connection/native.rs`
  - `crates/spur-acp/src/connection/cli_wrap_adapter.rs`
  - `crates/spur-acp/src/domain/events.rs`
  - `crates/spur-acp/src/adapter/mod.rs`
  - `crates/spur-acp/src/adapter/claude.rs`
  - `crates/spur-acp/src/adapter/kiro.rs`
  - `crates/spur-acp/src/adapter/codex.rs`

For each file:

- [ ] **Step 1: Read its `use agent_client_protocol::` block**

Run: `grep -n "use agent_client_protocol::" <file>`

- [ ] **Step 2: Apply the two-statement split per cheatsheet**

- [ ] **Step 3: Run `cargo check -p spur-acp` and confirm this file no longer appears in errors**

- [ ] **Step 4: After all spur-acp files are updated, also run `cargo check -p spur-acp --tests` to catch test-only imports**

Run: `cargo check -p spur-acp --tests 2>&1 | tail -20`
If test files (`crates/spur-acp/tests/*.rs`) appear, repeat steps 1-3 for each.

- [ ] **Step 5: Commit the spur-acp rewrites as one commit**

Run:
```sh
git add crates/spur-acp/
git commit -m "refactor(spur-acp): use schema:: namespace across remaining modules + tests

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.8: Rewrite imports in `crates/spur-core`

**Files:** the spur-core portion of `/tmp/spur-upgrade-files.txt`. Per the inventory:
- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-core/src/continuation_bridge.rs`
- `crates/spur-core/src/notification_drain.rs`
- `crates/spur-core/tests/continuation_integration.rs`
- `crates/spur-core/tests/continuation_properties.rs`
- `crates/spur-core/tests/skip_perm_helper.rs`
- `crates/spur-core/tests/notification_pump_integration.rs`

For each file:

- [ ] **Step 1: Locate the `use agent_client_protocol::` line**

Run: `grep -n "use agent_client_protocol::" <file>`

- [ ] **Step 2: Apply the two-statement split per cheatsheet**

- [ ] **Step 3: Run `cargo check -p spur-core` after each file (or batch the file edits and run once at the end)**

- [ ] **Step 4: Run `cargo check -p spur-core --tests` to catch test-file imports**

- [ ] **Step 5: Commit**

Run:
```sh
git add crates/spur-core/
git commit -m "refactor(spur-core): use schema:: namespace across orchestrator + continuation + tests

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.9: Rewrite imports in `crates/spur-tui`

**Files:** the spur-tui portion of `/tmp/spur-upgrade-files.txt`. Per the inventory:
- `crates/spur-tui/src/views/session_detail.rs`
- `crates/spur-tui/tests/session_update_handling.rs`
- `crates/spur-tui/tests/stream_tab_parity.rs`

Plus any others surfaced when running `cargo check -p spur-tui --tests`.

For each file:

- [ ] **Step 1: Locate the `use agent_client_protocol::` line**

Run: `grep -n "use agent_client_protocol::" <file>`

- [ ] **Step 2: Apply the two-statement split per cheatsheet**

- [ ] **Step 3: Run `cargo check -p spur-tui --tests`**

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-tui/
git commit -m "refactor(spur-tui): use schema:: namespace across views + tests

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.10: Rewrite imports in `crates/spur-bot`

**Files:**
- `crates/spur-bot/tests/runtime_flow.rs`

- [ ] **Step 1: Locate the `use agent_client_protocol::` line**

Run: `grep -n "use agent_client_protocol::" crates/spur-bot/tests/runtime_flow.rs`

- [ ] **Step 2: Apply the two-statement split per cheatsheet**

- [ ] **Step 3: Run `cargo check -p spur-bot --tests`**

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-bot/
git commit -m "refactor(spur-bot): use schema:: namespace in runtime_flow test

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.11: Sweep — confirm no other files reference the old paths

**Files:** none modified.

- [ ] **Step 1: Search for any residual root-level imports of moved symbols**

Run:
```sh
for sym in $(cat /tmp/spur-actually-moved.txt); do
  results=$(grep -rn "use agent_client_protocol::[^s].*$sym\b" crates/ 2>/dev/null | grep -v "::schema::")
  if [ -n "$results" ]; then
    echo "=== $sym ==="
    echo "$results"
  fi
done
```
Expected: empty output. Any matches indicate residual imports needing fix.

- [ ] **Step 2: Final cargo check on the workspace**

Run: `cargo check --workspace 2>&1 | tail -5`
Expected: `Finished` with zero errors.

If errors remain, locate them and apply per cheatsheet. Commit each fix with the same message style (`refactor(<crate>): ...`).

### Task 1.12: Verify test parity with baseline

**Files:** none modified.

- [ ] **Step 1: Run the full test suite**

Run:
```sh
cargo test --workspace --no-fail-fast 2>&1 | tee /tmp/spur-postupgrade-tests.log
tail -20 /tmp/spur-postupgrade-tests.log
```

- [ ] **Step 2: Compare pass/fail counts to baseline**

Run:
```sh
diff <(grep "^test result:" /tmp/spur-baseline-tests.log) \
     <(grep "^test result:" /tmp/spur-postupgrade-tests.log)
```
Expected: empty diff. Any difference (even +/- 1 test) requires investigation before proceeding to Phase 2.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30`
Expected: clean. If any new lints appeared due to the namespace change (e.g. `clippy::module_name_repetitions` from `schema::` types), address per existing patterns in the file.

- [ ] **Step 4: Verify formatting**

Run: `cargo fmt --check 2>&1 | tail -10`
Expected: clean.

If any of these fail, do not proceed to Phase 2 — fix and re-verify.

### Task 1.13: Phase 1 complete — final commit if any sweep fixes

**Files:** any not-yet-committed.

- [ ] **Step 1: Confirm clean tree**

Run: `git status --short`
Expected: empty output (everything committed in Tasks 1.5-1.10).

- [ ] **Step 2: Show Phase 1 commit summary**

Run: `git log --oneline 3def8557..HEAD`
Expected: 5-7 commits forming Phase 1.

---

## Phase 2 — v1 codex `/model` and `/effort` pickers (PR-1)

**Why second:** depends on Phase 1's `unstable_session_model` feature flag. Implements the feature set in `docs/superpowers/specs/2026-04-27-codex-model-effort-slash-pickers-design.md` using the v2-aligned type shapes (`ArgPickerSpec`, `SlashArg{command_name}`) so v2 PRs become purely additive.

### Task 2.1: Add `ArgPickerSpec` / `ArgPickerHint` types in `spur-acp`

**Files:**
- Create: `crates/spur-acp/src/adapter/arg_picker_hint.rs`
- Modify: `crates/spur-acp/src/adapter/mod.rs` (add `pub mod arg_picker_hint;`)
- Test: `crates/spur-acp/src/adapter/arg_picker_hint.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write failing tests inline in the new module**

Create `crates/spur-acp/src/adapter/arg_picker_hint.rs` with:

```rust
//! Vendor-neutral arg-picker descriptors derived from agent-advertised data
//! (config_options for v1 synthetic commands; AvailableCommand.input + _meta
//! for v2 advertised commands). Consumed by spur-tui without ACP-schema
//! imports — spur-tui sees only the types defined here.

#[derive(Debug, Clone, PartialEq)]
pub struct ArgPickerSpec {
    /// Hint string for picker placeholder. Empty when the source is a typed
    /// select with no free-text fallback (e.g. v1 ConfigOption commands).
    pub free_text_hint: String,
    /// If Some, the picker uses a typed query source. None means free-text.
    pub typed_hint: Option<ArgPickerHint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgPickerHint {
    /// v1: picker reads choices from the agent's cached SessionConfigOption
    /// select for the given config_id.
    ConfigOption { config_id: String },
    // v2 will add: GitRef { kind: GitRefKind }, etc.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_option_spec_has_no_free_text_fallback() {
        let spec = ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::ConfigOption {
                config_id: "model".into(),
            }),
        };
        assert!(spec.free_text_hint.is_empty());
        assert!(matches!(
            spec.typed_hint,
            Some(ArgPickerHint::ConfigOption { ref config_id }) if config_id == "model"
        ));
    }

    #[test]
    fn arg_picker_hint_equality() {
        let a = ArgPickerHint::ConfigOption { config_id: "model".into() };
        let b = ArgPickerHint::ConfigOption { config_id: "model".into() };
        let c = ArgPickerHint::ConfigOption { config_id: "reasoning_effort".into() };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
```

Then add to `crates/spur-acp/src/adapter/mod.rs`:
```rust
pub mod arg_picker_hint;
```
(Find the existing `pub mod` lines and add this one in alphabetical order.)

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p spur-acp adapter::arg_picker_hint 2>&1 | tail -10`
Expected: tests don't compile yet because the module file didn't exist; after creating it they should compile and pass.

If they do compile and pass on first run (because the test code matches the type definitions in the same file), that's expected and fine — this task is a single atomic addition.

- [ ] **Step 3: Run all spur-acp tests to confirm no regression**

Run: `cargo test -p spur-acp 2>&1 | tail -5`
Expected: all green; new tests visible.

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-acp/src/adapter/arg_picker_hint.rs crates/spur-acp/src/adapter/mod.rs
git commit -m "feat(spur-acp): add ArgPickerSpec/ArgPickerHint vendor-neutral types

Foundation for v1 (/model, /effort) and v2 (/review-style) arg pickers.
Lives in adapter/ alongside other vendor-neutral parsers.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.2: Add `synthesize` function in `spur-acp/adapter/config_options.rs`

**Files:**
- Create: `crates/spur-acp/src/adapter/config_options.rs`
- Modify: `crates/spur-acp/src/adapter/mod.rs` (add `pub mod config_options;`)

- [ ] **Step 1: Write failing tests for the synthesizer (covering S1-S8 from spec §10.3)**

Create `crates/spur-acp/src/adapter/config_options.rs` with the test module first (TDD: red):

```rust
//! Synthesizes interactive `AdvertisedCommand` rows from the agent's
//! `Vec<SessionConfigOption>`. Vendor-neutral by `config_id` allow-list.

use agent_client_protocol::schema::{SessionConfigOption, SessionConfigOptionKind};

use super::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};

/// Vendor-neutral description of an interactive slash command synthesized from
/// the agent's advertised config options. spur-tui consumes this without
/// needing ACP schema imports.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedCommand {
    /// Slash name (no leading `/`). E.g. "model", "effort".
    pub name: String,
    /// Short label for the slash popup.
    pub description: String,
    /// Optional hint, e.g. the current value.
    pub hint: Option<String>,
    /// ACP `config_id` to send back in `set_config_option`. May differ from
    /// `name` (we rename `reasoning_effort` → `effort` at the slash surface).
    pub config_id: String,
    /// The currently-active choice id, if known.
    pub current_value: Option<String>,
    /// Available choices for the arg picker.
    pub choices: Vec<AdvertisedChoice>,
    /// The arg picker spec to register against this command in the registry.
    pub arg_picker_spec: ArgPickerSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedChoice {
    /// Wire value (sent as the `value` of `set_config_option`).
    pub value: String,
    /// Human-readable label shown in the picker.
    pub label: String,
    /// Optional one-line description.
    pub description: Option<String>,
}

/// Allow-list of (acp_config_id, slash_name, slash_description). Drives the
/// vendor-neutral generation: any agent emitting these `config_id`s in
/// NewSessionResponse.config_options gets the matching slash command.
const ALLOW_LIST: &[(&str, &str, &str)] = &[
    ("model",            "model",  "Switch model for this session"),
    ("reasoning_effort", "effort", "Switch reasoning / thinking effort"),
];

/// Synthesize advertised commands from the agent's cached config options.
/// Filters by ALLOW_LIST and ignores non-Select shapes and empty-choice options.
pub fn synthesize(options: &[SessionConfigOption]) -> Vec<AdvertisedCommand> {
    // Implementation comes in step 3.
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        SessionConfigId, SessionConfigOption, SessionConfigSelectOption,
    };

    fn make_select(config_id: &str, current: &str, choices: &[(&str, &str)]) -> SessionConfigOption {
        let select_choices: Vec<SessionConfigSelectOption> = choices
            .iter()
            .map(|(id, name)| SessionConfigSelectOption::new((*id).to_string(), (*name).to_string()))
            .collect();
        SessionConfigOption::select(
            SessionConfigId::new(config_id.to_string()),
            "label".to_string(),
            current.to_string(),
            select_choices,
        )
    }

    // S1
    #[test]
    fn empty_input_returns_empty() {
        assert!(synthesize(&[]).is_empty());
    }

    // S2
    #[test]
    fn single_allowlisted_select_emits_one_command_with_ordered_choices() {
        let opt = make_select(
            "model",
            "gpt-5-codex",
            &[("gpt-5-codex", "GPT-5 Codex"), ("gpt-5", "GPT-5"), ("o4-mini", "o4-mini")],
        );
        let out = synthesize(&[opt]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "model");
        assert_eq!(out[0].config_id, "model");
        assert_eq!(out[0].choices.len(), 3);
        assert_eq!(out[0].choices[0].value, "gpt-5-codex");
        assert_eq!(out[0].choices[1].value, "gpt-5");
        assert_eq!(out[0].choices[2].value, "o4-mini");
    }

    // S3 — boolean type filtered out (when SessionConfigOption gets non-Select variants)
    // Skip: as of agent-client-protocol-schema 0.11.4, only Select shape exists.
    // When boolean lands, add the test mirroring S2 but constructing a boolean option
    // and asserting `synthesize` returns empty for it.

    // S4
    #[test]
    fn empty_choices_omits_command() {
        let opt = make_select("model", "", &[]);
        assert!(synthesize(&[opt]).is_empty());
    }

    // S5
    #[test]
    fn non_allowlisted_config_id_omitted() {
        let opt = make_select("mode", "auto", &[("auto", "Auto"), ("manual", "Manual")]);
        assert!(synthesize(&[opt]).is_empty());
    }

    // S6
    #[test]
    fn multiple_allowlisted_returned_in_allowlist_order() {
        let effort = make_select(
            "reasoning_effort",
            "high",
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        );
        let model = make_select(
            "model",
            "gpt-5",
            &[("gpt-5", "GPT-5")],
        );
        // Pass in opposite order from ALLOW_LIST to verify ordering is by ALLOW_LIST not input
        let out = synthesize(&[effort, model]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "model");      // ALLOW_LIST[0]
        assert_eq!(out[1].name, "effort");     // ALLOW_LIST[1]
    }

    // S7
    #[test]
    fn current_value_populated() {
        let opt = make_select("model", "gpt-5-codex", &[("gpt-5-codex", "GPT-5 Codex")]);
        let out = synthesize(&[opt]);
        assert_eq!(out[0].current_value, Some("gpt-5-codex".to_string()));
    }

    // S8
    #[test]
    fn hint_format_when_current_value_some() {
        let opt = make_select("model", "gpt-5-codex", &[("gpt-5-codex", "GPT-5 Codex")]);
        let out = synthesize(&[opt]);
        assert_eq!(out[0].hint, Some("current: gpt-5-codex".to_string()));
    }

    #[test]
    fn renames_reasoning_effort_to_effort_at_slash_surface() {
        let opt = make_select(
            "reasoning_effort",
            "high",
            &[("low", "Low"), ("high", "High")],
        );
        let out = synthesize(&[opt]);
        assert_eq!(out[0].name, "effort");
        assert_eq!(out[0].config_id, "reasoning_effort");
    }

    #[test]
    fn arg_picker_spec_is_config_option_typed() {
        let opt = make_select("model", "gpt-5", &[("gpt-5", "GPT-5")]);
        let out = synthesize(&[opt]);
        assert_eq!(out[0].arg_picker_spec.free_text_hint, "");
        assert_eq!(
            out[0].arg_picker_spec.typed_hint,
            Some(ArgPickerHint::ConfigOption { config_id: "model".into() })
        );
    }
}
```

Then add to `crates/spur-acp/src/adapter/mod.rs`:
```rust
pub mod config_options;
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p spur-acp adapter::config_options 2>&1 | tail -20`
Expected: tests fail at `unimplemented!()` panic (the synthesize function body).

- [ ] **Step 3: Implement `synthesize`**

Replace the `unimplemented!()` body in `crates/spur-acp/src/adapter/config_options.rs` with:

```rust
pub fn synthesize(options: &[SessionConfigOption]) -> Vec<AdvertisedCommand> {
    let mut out = Vec::new();
    for (acp_config_id, slash_name, slash_desc) in ALLOW_LIST {
        // Find the matching option (Select kind) in the input.
        let opt = options.iter().find(|o| o.id.0.as_ref() == *acp_config_id);
        let Some(opt) = opt else { continue };

        // Extract the Select variant. Non-Select shapes are silently ignored.
        let (current, choices_acp) = match &opt.kind {
            SessionConfigOptionKind::Select { current_value, choices } => {
                (current_value.clone(), choices.clone())
            }
            // Future variants (Boolean, etc.) — skip.
        };

        // Skip degenerate empty-choice selects.
        if choices_acp.is_empty() {
            continue;
        }

        let choices: Vec<AdvertisedChoice> = choices_acp
            .into_iter()
            .map(|c| AdvertisedChoice {
                value: c.value.0.to_string(),
                label: c.label,
                description: c.description,
            })
            .collect();

        let current_value = if current.is_empty() { None } else { Some(current.clone()) };
        let hint = current_value.as_ref().map(|v| format!("current: {v}"));

        out.push(AdvertisedCommand {
            name: (*slash_name).to_string(),
            description: (*slash_desc).to_string(),
            hint,
            config_id: (*acp_config_id).to_string(),
            current_value,
            choices,
            arg_picker_spec: ArgPickerSpec {
                free_text_hint: String::new(),
                typed_hint: Some(ArgPickerHint::ConfigOption {
                    config_id: (*acp_config_id).to_string(),
                }),
            },
        });
    }
    out
}
```

Note: the field shapes (`opt.id.0`, `opt.kind`, `current_value`, `choices`) reflect the `SessionConfigOption` types in agent-client-protocol-schema 0.11.4. If the actual struct shape differs (e.g. fields are wrapped in `Option`, or the enum variant is named differently), the executor must read the type definitions in `~/.cargo/registry/src/index.crates.io-*/agent-client-protocol-schema-0.11.4/src/agent.rs` (or similar) and adjust. The compiler will guide.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p spur-acp adapter::config_options 2>&1 | tail -20`
Expected: all 8 tests (`empty_input_returns_empty`, `single_allowlisted_select_emits_one_command_with_ordered_choices`, `empty_choices_omits_command`, `non_allowlisted_config_id_omitted`, `multiple_allowlisted_returned_in_allowlist_order`, `current_value_populated`, `hint_format_when_current_value_some`, `renames_reasoning_effort_to_effort_at_slash_surface`, `arg_picker_spec_is_config_option_typed`) pass.

- [ ] **Step 5: Commit**

Run:
```sh
git add crates/spur-acp/src/adapter/config_options.rs crates/spur-acp/src/adapter/mod.rs
git commit -m "feat(spur-acp): synthesize AdvertisedCommand from cached config_options

Vendor-neutral allow-list keyed by config_id. Renames reasoning_effort
to /effort at the slash surface; preserves wire name in config_id.
Tests cover S1-S8 from spec §10.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.3: Add `set_session_config_option` to `AgentConnection` trait

**Files:**
- Modify: `crates/spur-acp/src/connection/mod.rs` (add new method after `set_session_mode` at L158)

- [ ] **Step 1: Locate the `set_session_mode` declaration**

Run: `sed -n '155,170p' crates/spur-acp/src/connection/mod.rs`
Expected: see the trait method declaration around L158-166.

- [ ] **Step 2: Add the new trait method below it**

After `set_session_mode`'s declaration (identify by closing `}` of the default body), add:

```rust
/// Issue ACP `session/set_config_option`. Returns the agent's updated
/// `Vec<SessionConfigOption>` so callers can refresh their cache.
async fn set_session_config_option(
    &mut self,
    request: agent_client_protocol::schema::SetSessionConfigOptionRequest,
) -> anyhow::Result<agent_client_protocol::schema::SetSessionConfigOptionResponse> {
    Err(anyhow::anyhow!(
        "set_session_config_option not supported by this connection"
    ))
}
```

This default-error implementation mirrors how `set_session_mode` is structured (see L158-166 — also a default-impl method). Specific connections override.

- [ ] **Step 3: Run cargo check**

Run: `cargo check -p spur-acp 2>&1 | tail -10`
Expected: clean. If `SetSessionConfigOptionRequest`/`Response` aren't found, verify they exist in 0.11.x (they do, per spec §3.1; gated behind `unstable_session_model` feature which Phase 1 added).

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-acp/src/connection/mod.rs
git commit -m "feat(spur-acp): add set_session_config_option to AgentConnection trait

Default impl returns NotSupported error; NativeAcpConnection overrides
in next commit. Signature mirrors set_session_mode.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.4: Implement `set_session_config_option` on `NativeAcpConnection`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:82-124` (extend AcpCommand enum)
- Modify: `crates/spur-acp/src/connection/native.rs:544-568` (add new method after set_session_mode)
- Modify: `crates/spur-acp/src/connection/native.rs:1007-1013` (add handler after SetSessionMode in the ACP thread match arm)

- [ ] **Step 1: Add the AcpCommand variant**

In `crates/spur-acp/src/connection/native.rs`, locate the `enum AcpCommand` declaration (line 82). After the `SetSessionMode` variant (L112-115), add:

```rust
SetSessionConfigOption {
    request: agent_client_protocol::schema::SetSessionConfigOptionRequest,
    reply: oneshot::Sender<anyhow::Result<agent_client_protocol::schema::SetSessionConfigOptionResponse>>,
},
```

- [ ] **Step 2: Add the trait method impl**

Locate the existing `set_session_mode` impl (`native.rs:544-568`). After the closing `}`, add:

```rust
async fn set_session_config_option(
    &mut self,
    request: agent_client_protocol::schema::SetSessionConfigOptionRequest,
) -> anyhow::Result<agent_client_protocol::schema::SetSessionConfigOptionResponse> {
    let cmd_tx = self
        .cmd_tx
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!(
            "NativeAcpConnection '{}': set_session_config_option called before initialize",
            self.agent_name
        ))?;
    let (reply_tx, reply_rx) = oneshot::channel();
    cmd_tx
        .send(AcpCommand::SetSessionConfigOption {
            request,
            reply: reply_tx,
        })
        .map_err(|_| anyhow::anyhow!(
            "NativeAcpConnection '{}': ACP thread is gone",
            self.agent_name
        ))?;
    reply_rx
        .await
        .map_err(|_| anyhow::anyhow!(
            "NativeAcpConnection '{}': set_session_config_option reply dropped",
            self.agent_name
        ))?
}
```

- [ ] **Step 3: Add the handler in the ACP thread**

Locate the `SetSessionMode` handler (`native.rs:1007-1013`). After its closing `}`, add:

```rust
AcpCommand::SetSessionConfigOption { request, reply } => {
    let result = connection
        .set_session_config_option(request)
        .await
        .map_err(|e| anyhow::anyhow!(
            "NativeAcpConnection '{}': set_session_config_option failed: {e}",
            agent_name
        ));
    let _ = reply.send(result);
}
```

- [ ] **Step 4: Run cargo check**

Run: `cargo check -p spur-acp 2>&1 | tail -10`
Expected: clean. If the SDK's `Connection` type doesn't have `set_session_config_option` (it should — it's in the schema crate at `client.rs:158-168` per earlier verification), the executor verifies the actual method name on the SDK's `Connection` and adjusts.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-acp connection::native 2>&1 | tail -10`
Expected: existing tests pass; no new test added in this task (the wire-path test belongs to integration tier in Task 2.13).

- [ ] **Step 6: Commit**

Run:
```sh
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): implement set_session_config_option on NativeAcpConnection

Mirrors SetSessionMode plumbing: AcpCommand variant + thread handler.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.5: Extend `Dispatch` enum with `SetSessionConfigOption`

**Files:**
- Modify: `crates/spur-tui/src/commands/entry.rs:31-47`

- [ ] **Step 1: Read current Dispatch enum**

Run: `sed -n '31,47p' crates/spur-tui/src/commands/entry.rs`
Expected: 3 variants (SpurLocal, PromptText, VendorExec).

- [ ] **Step 2: Add new variant**

After the `VendorExec { … }` variant in `crates/spur-tui/src/commands/entry.rs`, add:

```rust
/// v1: dispatch to ACP `session/set_config_option`. Used by the synthetic
/// /model and /effort slash commands. The `value` is filled in by the
/// arg-picker selection (or by the user's typed arg).
SetSessionConfigOption { config_id: String },
```

- [ ] **Step 3: Cargo check (compiler-driven for any new match arms)**

Run: `cargo check -p spur-tui 2>&1 | tail -20`
Expected: any `match dispatch` sites that didn't have a wildcard arm now error. If so, add a `_ => {}` arm to silence them temporarily — they'll be filled out properly in Task 2.10 (submit_router) and elsewhere. List the sites flagged here for follow-up.

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-tui/src/commands/entry.rs
git commit -m "feat(spur-tui): add Dispatch::SetSessionConfigOption variant

For v1 /model and /effort slash commands. Routed in submit_router in
a later task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.6: Extend `CommandSource` enum with `Advertised`

**Files:**
- Modify: `crates/spur-tui/src/commands/entry.rs:20-28`

- [ ] **Step 1: Read current CommandSource enum**

Run: `sed -n '20,28p' crates/spur-tui/src/commands/entry.rs`
Expected: 2 variants (Spur, Agent { handle }).

- [ ] **Step 2: Add Advertised variant**

After the `Agent { handle: String },` variant, add:

```rust
/// Synthesized by spur from an agent's advertised data (e.g.
/// NewSessionResponse.config_options). Vendor-neutral by allow-list;
/// see crates/spur-acp/src/adapter/config_options.rs.
Advertised { handle: String },
```

The `handle` mirrors `Agent { handle }` so per-agent attribution is consistent.

- [ ] **Step 3: Cargo check**

Run: `cargo check -p spur-tui 2>&1 | tail -10`
Expected: any exhaustive matches on CommandSource (filtering UI etc.) error. Add a `_ => {}` arm and note the sites for follow-up — they may need explicit handling in later tasks.

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-tui/src/commands/entry.rs
git commit -m "feat(spur-tui): add CommandSource::Advertised variant

Tags entries synthesized by spur from agent-advertised config_options
(distinct from Agent which is from AvailableCommandsUpdate).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.7: Add `advertised_commands` slot to `CommandRegistry`

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs:15-22` (struct fields)
- Modify: `crates/spur-tui/src/commands/registry.rs:75-134` (ensure_cache merge logic)
- Modify: `crates/spur-tui/src/commands/registry.rs:66-73` (mirror set_agent_commands as set_advertised_commands)

- [ ] **Step 1: Read the current CommandRegistry struct**

Run: `sed -n '13,30p' crates/spur-tui/src/commands/registry.rs`

- [ ] **Step 2: Add the new field**

In `crates/spur-tui/src/commands/registry.rs`, modify the struct (L15-22) to add a third field after `dynamic_commands`:

```rust
pub struct CommandRegistry {
    static_commands: Vec<(String, Vec<CommandEntry>)>,
    dynamic_commands: Vec<(String, Vec<CommandEntry>)>,
    advertised_commands: Vec<(String, Vec<CommandEntry>)>,    // NEW
    cache: RefCell<Option<CacheSnapshot>>,
}
```

If the struct has a `Default` impl, find it and add `advertised_commands: Vec::new(),` to the construction.

If the struct has a `pub fn new()` constructor, find it and add the same.

- [ ] **Step 3: Add the `set_advertised_commands` method (mirror set_agent_commands)**

Read the existing `set_agent_commands` at L66-73:
```sh
sed -n '66,75p' crates/spur-tui/src/commands/registry.rs
```

After it, add:

```rust
pub fn set_advertised_commands(&mut self, handle: &str, entries: Vec<CommandEntry>) {
    if let Some(slot) = self.advertised_commands.iter_mut().find(|(h, _)| h == handle) {
        slot.1 = entries;
    } else {
        self.advertised_commands.push((handle.to_string(), entries));
    }
    *self.cache.borrow_mut() = None;
}
```

- [ ] **Step 4: Update `ensure_cache` to merge advertised entries alongside static and dynamic**

Read the existing merge logic:
```sh
sed -n '75,134p' crates/spur-tui/src/commands/registry.rs
```

Find the loop that consumes `dynamic_commands`. Add a parallel iteration over `advertised_commands` immediately after, appending each advertised entry to the merged list using the same shadowing/dedup rules as `dynamic_commands` (the existing rule: spur-local entries shadow agent entries with the same name; the new rule: spur-local entries also shadow advertised entries with the same name).

If the existing merge logic uses a helper or matches on `CommandSource`, extend the match arm to handle `Advertised { handle }` symmetrically with `Agent { handle }`.

- [ ] **Step 5: Add a unit test for the new bucket**

After the existing `#[cfg(test)] mod tests` block (or create one if absent), add:

```rust
#[test]
fn advertised_commands_appear_in_cache() {
    let mut reg = CommandRegistry::default();
    let entry = CommandEntry {
        name: "model".to_string(),
        description: "Switch model".to_string(),
        hint: None,
        source: CommandSource::Advertised { handle: "codex".to_string() },
        dispatch: Dispatch::SetSessionConfigOption { config_id: "model".to_string() },
    };
    reg.set_advertised_commands("codex", vec![entry]);
    let names = reg.iter_entries().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert!(names.contains(&"model".to_string()));
}

#[test]
fn spur_local_shadows_advertised_with_same_name() {
    let mut reg = CommandRegistry::default();
    // (Set up a static spur-local /model entry; call set_advertised_commands
    // with a competing /model; assert the spur-local one wins.)
    // Use whatever helper the existing tests use to construct spur-local entries.
}
```

The exact `iter_entries` method name should match what the existing tests use. If absent, use whatever public accessor the registry exposes (or add one).

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-tui registry 2>&1 | tail -15`
Expected: new tests pass; no regressions.

- [ ] **Step 7: Commit**

Run:
```sh
git add crates/spur-tui/src/commands/registry.rs
git commit -m "feat(spur-tui): add advertised_commands bucket to CommandRegistry

Third bucket alongside static + dynamic. Advertised entries follow the
same shadowing rule (spur-local wins on name collision).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.8: Extend `TriggerKind` with `SlashArg { command_name }`

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs:1-8` (add variant)
- Modify: `crates/spur-tui/src/components/completion_trigger.rs:50-58` (extend internal TriggerState if needed)
- Modify: `crates/spur-tui/src/components/completion_trigger.rs:110-116` (extend `step` to handle SlashArg)
- Test: `crates/spur-tui/src/components/completion_trigger.rs` (add tests T6-T15 from spec §10.2)

This is the highest-risk task per the α-mitigation in spec §4 (Q5). Tests T1-T15 must be written BEFORE implementation.

- [ ] **Step 1: Write the tests T1-T15 from spec §10.2**

Locate the existing `#[cfg(test)] mod tests` block in `crates/spur-tui/src/components/completion_trigger.rs`. Add tests for both T1-T5 (regression guards on existing Mention/Slash) and T6-T15 (new SlashArg behavior). Use whatever test helper pattern the existing tests use (likely a builder for `(text, cursor, event)` → expected `TriggerTransition`).

Pseudo-code template (adapt to existing style):

```rust
#[test]
fn t6_slash_model_space_at_end_opens_slash_arg() {
    let mut detector = TriggerDetector::default();
    // Type /model into a fresh buffer, then space.
    // Use a minimal CommandRegistry that reports Some(spec) for "model" and None for others.
    let registry = test_registry_with_arg_picker(&["model"]);
    let txn = step_with_text(&mut detector, "/model ", 7, &registry);
    assert!(matches!(
        txn,
        TriggerTransition::Open { trigger: Trigger { kind: TriggerKind::SlashArg { ref command_name }, .. } } if command_name == "model"
    ));
}

#[test]
fn t9_paste_query_into_slash_arg_filters() {
    let mut detector = TriggerDetector::default();
    let registry = test_registry_with_arg_picker(&["model"]);
    // Type /model then paste "gpt"
    let _ = step_with_text(&mut detector, "/model ", 7, &registry);
    let txn = step_with_text(&mut detector, "/model gpt", 10, &registry);
    assert!(matches!(
        txn,
        TriggerTransition::Update { ref query } if query == "gpt"
    ));
}

#[test]
fn t12_unknown_slash_command_does_not_open_slash_arg() {
    let mut detector = TriggerDetector::default();
    let registry = test_registry_with_arg_picker(&["model"]);  // /unknown not registered
    let txn = step_with_text(&mut detector, "/unknown foo", 12, &registry);
    assert!(matches!(txn, TriggerTransition::None));
}

#[test]
fn t15_command_lookup_is_case_sensitive() {
    let mut detector = TriggerDetector::default();
    let registry = test_registry_with_arg_picker(&["model"]);  // lowercase only
    let txn = step_with_text(&mut detector, "/Model ", 7, &registry);
    assert!(matches!(txn, TriggerTransition::None));
}
```

Add tests T1-T5 (regression: existing Mention/Slash behavior unchanged) using the same pattern. Add T7, T8, T10, T11, T13, T14 as variants of the above.

The helper `test_registry_with_arg_picker(names)` constructs a CommandRegistry where each named command has an `Option<ArgPickerSpec>` (Some for the listed names, None for others) — the executor implements this helper using whatever public surface CommandRegistry exposes for tests.

NOTE: `TriggerDetector::step` does not currently take a `&CommandRegistry` argument. The signature change (Step 4 below) adds it.

- [ ] **Step 2: Run tests to verify all 15 fail (compile error or assertion failure)**

Run: `cargo test -p spur-tui completion_trigger 2>&1 | tail -30`
Expected: T6-T15 fail (they reference `TriggerKind::SlashArg` and the new step signature). T1-T5 might pass since they describe existing behavior.

- [ ] **Step 3: Add the TriggerKind::SlashArg variant**

In `crates/spur-tui/src/components/completion_trigger.rs:1-8`:

```rust
pub enum TriggerKind {
    Slash,
    Mention,
    /// Cursor in the arg region of a slash command whose registry
    /// `arg_picker_spec(command_name)` returned Some. The picker kind
    /// (typed vs free-text) is resolved by InputCompletionPort, not here.
    SlashArg { command_name: String },
}
```

- [ ] **Step 4: Modify `step` signature to accept a registry handle**

Change `crates/spur-tui/src/components/completion_trigger.rs:110-116`:

```rust
pub fn step<R>(
    &mut self,
    event: IntentEvent,
    text: &str,
    cursor: usize,
    protected_ranges: &[crate::components::input_bar::ProtectedRange],
    registry_arg_picker: R,
) -> TriggerTransition
where
    R: Fn(&str) -> bool,        // takes command name; returns true if arg picker exists
{
```

The closure signature avoids importing CommandRegistry into completion_trigger. Callers pass `|name| registry.arg_picker_spec(name).is_some()`.

This signature change requires updating every existing call site of `step`. Find them:
```sh
grep -rn "trigger_detector.step\|\.step(" crates/spur-tui/src/components/ | grep -v test
```

For each call site, append the registry-arg-picker closure. If the surrounding code doesn't have a CommandRegistry handle, plumb one through (most likely it does, since that's how the slash popup gets its entries).

- [ ] **Step 5: Implement the SlashArg detection in `step`**

In the `step` body, after the existing Slash/Mention detection logic, add a branch:

```rust
// SlashArg detection: if we're not in an active Mention/Slash trigger
// and the text matches `^/<word>\s+`, look up the word and transition.
if matches!(self.state, TriggerState::Idle) {
    if let Some((cmd, prefix_start)) = parse_slash_arg_prefix(text, cursor) {
        if registry_arg_picker(cmd) {
            self.state = TriggerState::Composing {
                kind: TriggerKindInternal::SlashArg { command_name: cmd.to_string() },
                prefix_start,
            };
            return TriggerTransition::Open {
                trigger: Trigger {
                    kind: TriggerKind::SlashArg { command_name: cmd.to_string() },
                    /* fill remaining Trigger fields per existing pattern */
                },
            };
        }
    }
}

// SlashArg update: if we're in a SlashArg state, recompute the query.
if let TriggerState::Composing {
    kind: TriggerKindInternal::SlashArg { ref command_name },
    prefix_start,
} = &self.state.clone()
{
    let query = &text[*prefix_start..cursor];
    return TriggerTransition::Update { query: query.to_string() };
}
```

The helper `parse_slash_arg_prefix(text, cursor) -> Option<(&str, usize)>` parses `^/<command>\s+` and returns `(command_name, byte_offset_just_past_whitespace)`. Implement it as a private function:

```rust
fn parse_slash_arg_prefix(text: &str, cursor: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'/') { return None; }

    // Find end of command name (first whitespace after /).
    let mut end_of_cmd = 1;
    while end_of_cmd < bytes.len() && !bytes[end_of_cmd].is_ascii_whitespace() {
        end_of_cmd += 1;
    }
    if end_of_cmd == 1 { return None; }                  // /<empty>
    if end_of_cmd >= bytes.len() { return None; }        // no whitespace after cmd

    // Require single-space delimiter (not multi-word command names).
    let cmd = &text[1..end_of_cmd];
    let prefix_start = end_of_cmd + 1;
    if prefix_start > cursor { return None; }            // cursor not yet in arg region
    Some((cmd, prefix_start))
}
```

You may also need to extend the internal `TriggerState` enum (`completion_trigger.rs:50-58`) to carry `SlashArg` state. The internal kind enum needs a new variant; copy the existing pattern.

- [ ] **Step 6: Run all tests; iterate to green**

Run: `cargo test -p spur-tui completion_trigger 2>&1 | tail -30`
Expected: all 15 tests pass. Iterate fixes until green.

- [ ] **Step 7: Confirm no regression on other spur-tui tests**

Run: `cargo test -p spur-tui 2>&1 | tail -10`
Expected: all green.

- [ ] **Step 8: Commit**

Run:
```sh
git add crates/spur-tui/src/components/completion_trigger.rs
git commit -m "feat(spur-tui): add TriggerKind::SlashArg state + detection

State machine extends to detect ^/<cmd>\\s+ patterns where the registry
reports a known arg-picker for <cmd>. Tests T1-T15 from spec §10.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.9: Add `arg_picker_spec(name)` lookup to `CommandRegistry`

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs` (add method)
- Modify: `crates/spur-tui/src/commands/entry.rs:6-17` (add `arg_picker_spec` field on CommandEntry)

- [ ] **Step 1: Add the field to CommandEntry**

In `crates/spur-tui/src/commands/entry.rs`, modify the struct (L6-17) to add:

```rust
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
    pub source: CommandSource,
    pub dispatch: Dispatch,
    /// If Some, typing `/<name> <arg>` opens an arg picker.
    pub arg_picker_spec: Option<spur_acp::adapter::arg_picker_hint::ArgPickerSpec>,
}
```

This requires adding `spur-acp` to spur-tui's Cargo.toml dependencies (likely already present; verify).

- [ ] **Step 2: Update all CommandEntry constructions**

Run: `grep -rn "CommandEntry {" crates/spur-tui/src/`
Expected: every call site needs `arg_picker_spec: None,` added (or `Some(spec)` for the synthesized advertised commands).

For static spur-local commands (in `spur_local.rs`), add `arg_picker_spec: None,` to each entry.

For the future advertised-source synthesizer (Task 2.10), it will set `arg_picker_spec: Some(advertised_command.arg_picker_spec.clone())`.

- [ ] **Step 3: Add the lookup method to CommandRegistry**

In `crates/spur-tui/src/commands/registry.rs`, add a method on the `impl CommandRegistry` block:

```rust
/// Returns the parsed ArgPickerSpec for the named command, if it requires
/// an arg picker. Used by TriggerDetector to decide whether ^/<cmd>\s+
/// transitions to SlashArg, and by InputCompletionPort to instantiate the
/// matching QuerySource.
pub fn arg_picker_spec(&self, command_name: &str) -> Option<spur_acp::adapter::arg_picker_hint::ArgPickerSpec> {
    self.ensure_cache();
    let cache = self.cache.borrow();
    cache.as_ref()?.entries.iter()
        .find(|e| e.name == command_name)
        .and_then(|e| e.arg_picker_spec.clone())
}
```

Adjust `cache.entries` to whatever the actual cache structure exposes (verify by reading the existing `ensure_cache` body).

- [ ] **Step 4: Add a unit test**

```rust
#[test]
fn arg_picker_spec_returns_some_for_advertised_with_spec() {
    let mut reg = CommandRegistry::default();
    let entry = CommandEntry {
        name: "model".into(),
        description: "Switch".into(),
        hint: None,
        source: CommandSource::Advertised { handle: "codex".into() },
        dispatch: Dispatch::SetSessionConfigOption { config_id: "model".into() },
        arg_picker_spec: Some(spur_acp::adapter::arg_picker_hint::ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(spur_acp::adapter::arg_picker_hint::ArgPickerHint::ConfigOption {
                config_id: "model".into(),
            }),
        }),
    };
    reg.set_advertised_commands("codex", vec![entry]);
    assert!(reg.arg_picker_spec("model").is_some());
    assert!(reg.arg_picker_spec("nonexistent").is_none());
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui registry 2>&1 | tail -10`
Expected: green.

- [ ] **Step 6: Commit**

Run:
```sh
git add crates/spur-tui/src/commands/entry.rs crates/spur-tui/src/commands/registry.rs crates/spur-tui/src/commands/spur_local.rs
git commit -m "feat(spur-tui): CommandEntry.arg_picker_spec + Registry::arg_picker_spec lookup

All static spur-local entries get None; advertised entries carry the
synthesized spec. TriggerDetector consults this to decide SlashArg.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.10: Add `AdvertisedSource` to wire synthesizer → registry

**Files:**
- Create: `crates/spur-tui/src/commands/advertised.rs`
- Modify: `crates/spur-tui/src/commands/mod.rs` (add `pub mod advertised;`)

- [ ] **Step 1: Write the source module with tests inline**

Create `crates/spur-tui/src/commands/advertised.rs`:

```rust
//! Synthesizes CommandEntry rows from an agent's cached config_options.
//! Vendor-neutral; calls into spur-acp's synthesize() function.

use spur_acp::adapter::config_options::{synthesize, AdvertisedCommand};
use agent_client_protocol::schema::SessionConfigOption;

use super::entry::{CommandEntry, CommandSource, Dispatch};

pub struct AdvertisedSource;

impl AdvertisedSource {
    /// Build CommandEntry rows from cached config_options. Each entry's
    /// `arg_picker_spec` is set from the synthesizer output.
    pub fn entries(handle: &str, opts: &[SessionConfigOption]) -> Vec<CommandEntry> {
        synthesize(opts)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised { handle: handle.to_string() },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        SessionConfigId, SessionConfigOption, SessionConfigSelectOption,
    };

    #[test]
    fn empty_options_yield_empty_entries() {
        assert!(AdvertisedSource::entries("codex", &[]).is_empty());
    }

    #[test]
    fn allowlisted_option_yields_advertised_entry() {
        let opt = SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "label".to_string(),
            "gpt-5-codex".to_string(),
            vec![SessionConfigSelectOption::new("gpt-5-codex".to_string(), "GPT-5 Codex".to_string())],
        );
        let entries = AdvertisedSource::entries("codex", &[opt]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
        assert!(matches!(entries[0].source, CommandSource::Advertised { ref handle } if handle == "codex"));
        assert!(matches!(
            entries[0].dispatch,
            Dispatch::SetSessionConfigOption { ref config_id } if config_id == "model"
        ));
        assert!(entries[0].arg_picker_spec.is_some());
    }
}
```

Add to `crates/spur-tui/src/commands/mod.rs`:
```rust
pub mod advertised;
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-tui commands::advertised 2>&1 | tail -10`
Expected: both tests pass.

- [ ] **Step 3: Commit**

Run:
```sh
git add crates/spur-tui/src/commands/advertised.rs crates/spur-tui/src/commands/mod.rs
git commit -m "feat(spur-tui): AdvertisedSource bridges spur-acp synthesizer to registry

Builds CommandEntry rows from cached config_options, tagged
CommandSource::Advertised with arg_picker_spec set.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.11: Add `ConfigOptionQuerySource` (the picker data source)

**Files:**
- Create: `crates/spur-tui/src/components/config_option_query_source.rs`
- Modify: `crates/spur-tui/src/components/mod.rs` (add `pub mod config_option_query_source;`)

- [ ] **Step 1: Write the source with inline tests**

Create `crates/spur-tui/src/components/config_option_query_source.rs`:

```rust
//! QuerySource that pulls choices from cached SessionConfigOption select.
//! Used by v1 /model and /effort pickers. Static (snapshot at open).

use nucleo_matcher::{Matcher, Config, pattern::{Pattern, CaseMatching, Normalization}};
use nucleo_matcher::Utf32Str;

use spur_acp::adapter::config_options::AdvertisedChoice;
use super::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

pub struct ConfigOptionQuerySource {
    pub command: String,
    pub config_id: String,
    pub choices: Vec<AdvertisedChoice>,
}

impl ConfigOptionQuerySource {
    pub fn new(command: String, config_id: String, choices: Vec<AdvertisedChoice>) -> Self {
        Self { command, config_id, choices }
    }
}

impl QuerySource for ConfigOptionQuerySource {
    fn title(&self) -> &str {
        if self.command == "model" { "Model" }
        else if self.command == "effort" { "Effort" }
        else { &self.command }
    }

    fn query_mode(&self) -> QueryMode { QueryMode::OwnedByShell }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, &AdvertisedChoice)> = self.choices.iter()
            .filter_map(|c| {
                let mut buf = Vec::new();
                pattern.score(Utf32Str::new(&c.label, &mut buf), &mut matcher).map(|s| (s, c))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter()
            .map(|(_, c)| RetrievalRow {
                primary: c.label.clone(),
                secondary: c.description.clone().unwrap_or_default(),
                tag: c.value.clone(),
                atoms: Vec::new(),
            })
            .collect()
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        let value = self.choices.get(row_idx)?.value.clone();
        Some(RetrievalAccept::ReplaceTriggerToken {
            prefix_start: 0,    // Will be re-anchored by InputCompletionPort using the SlashArg state
            replacement: format!("/{} {}", self.command, value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ConfigOptionQuerySource {
        ConfigOptionQuerySource::new(
            "model".to_string(),
            "model".to_string(),
            vec![
                AdvertisedChoice { value: "gpt-5-codex".into(), label: "GPT-5 Codex".into(), description: None },
                AdvertisedChoice { value: "gpt-5".into(), label: "GPT-5".into(), description: None },
                AdvertisedChoice { value: "o4-mini".into(), label: "o4-mini".into(), description: None },
            ],
        )
    }

    #[test]
    fn refresh_empty_query_returns_all() {
        let mut src = fixture();
        let rows = src.refresh("");
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn refresh_filters_by_query() {
        let mut src = fixture();
        let rows = src.refresh("gpt");
        assert!(rows.iter().any(|r| r.primary.contains("GPT-5")));
        assert!(!rows.iter().any(|r| r.primary == "o4-mini"));
    }

    #[test]
    fn accept_returns_replace_trigger_token() {
        let src = fixture();
        let accept = src.accept(0).unwrap();
        match accept {
            RetrievalAccept::ReplaceTriggerToken { ref replacement, .. } => {
                assert_eq!(replacement, "/model gpt-5-codex");
            }
            _ => panic!("expected ReplaceTriggerToken"),
        }
    }

    #[test]
    fn accept_out_of_range_returns_none() {
        let src = fixture();
        assert!(src.accept(99).is_none());
    }
}
```

Add to `crates/spur-tui/src/components/mod.rs`:
```rust
pub mod config_option_query_source;
```

- [ ] **Step 2: Verify nucleo_matcher API matches**

Run: `cargo check -p spur-tui 2>&1 | tail -10`
If `Pattern::score` signature differs, consult the existing `MentionQuerySource` (per kimi research) at `crates/spur-tui/src/components/query_source.rs:416` and mirror its exact call shape.

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-tui config_option_query_source 2>&1 | tail -10`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-tui/src/components/config_option_query_source.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): ConfigOptionQuerySource for v1 /model and /effort pickers

Static QuerySource over cached choices; nucleo fuzzy filter. accept()
emits ReplaceTriggerToken with full /<cmd> <value> replacement; the
SlashArg state in InputCompletionPort re-anchors prefix_start.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.12: Wire `InputCompletionPort` to dispatch on `ArgPickerSpec`

**Files:**
- Modify: `crates/spur-tui/src/components/input_completion.rs:41-114` (extend dispatch to handle SlashArg trigger)
- Modify: `crates/spur-tui/src/components/input_completion.rs:177-201` (extend apply_accept if needed)

- [ ] **Step 1: Read the existing dispatch method**

Run: `sed -n '40,115p' crates/spur-tui/src/components/input_completion.rs`

- [ ] **Step 2: Add SlashArg handling**

In the `match` arm for `TriggerTransition::Open` (currently L78-108), add a new arm for `TriggerKind::SlashArg`:

```rust
TriggerKind::SlashArg { command_name } => {
    let Some(spec) = env.registry.arg_picker_spec(&command_name) else {
        return;  // shouldn't happen — TriggerDetector already verified
    };
    let source: Box<dyn QuerySource> = match spec.typed_hint {
        Some(ArgPickerHint::ConfigOption { config_id }) => {
            // Pull cached choices from the env's session-config-options cache
            let choices = env.session_config_options
                .iter()
                .find(|o| o.id.0.as_ref() == config_id.as_str())
                .map(|o| extract_choices(o))
                .unwrap_or_default();
            Box::new(ConfigOptionQuerySource::new(
                command_name.clone(),
                config_id.clone(),
                choices,
            ))
        }
        // v2 will add: Some(ArgPickerHint::GitRef { kind }) => Box::new(GitRefQuerySource::new(...)),
        None => {
            // v1: unreachable for /model, /effort. v2 uses for free-text.
            // Add a placeholder Box::new(...) that errors gracefully or
            // omit the arm — current v1 commands always have a typed_hint.
            return;
        }
    };
    self.picker_shell = Some(PickerShell::open_with_query(source, &trigger.query));
}
```

The `env.registry`, `env.session_config_options`, and `extract_choices` references reflect a CompletionEnv struct that needs the new `session_config_options` field. Read the existing `CompletionEnv` (likely `crates/spur-tui/src/components/input_completion.rs` or `mod.rs`):

```sh
grep -rn "struct CompletionEnv" crates/spur-tui/src/components/
```

Add the new field:
```rust
pub struct CompletionEnv<'a> {
    pub registry: &'a CommandRegistry,
    pub session_config_options: &'a [SessionConfigOption],     // NEW
    // ...existing fields...
}
```

The caller (in app.rs / dashboard.rs / wherever input handling is wired) constructs `CompletionEnv` per keystroke; add the cached config_options from the orchestrator there.

`extract_choices` is a small helper that pulls `Vec<AdvertisedChoice>` from a `SessionConfigOption::Select` variant. Define it as a private function in this file or in adapter::config_options as `pub fn extract_choices(opt: &SessionConfigOption) -> Vec<AdvertisedChoice>`.

- [ ] **Step 3: Verify accept handling**

Read the existing `apply_accept` (`input_completion.rs:177-201`). The `ReplaceTriggerToken` branch already calls `replace_trigger_token(input_bar, prefix_start, &replacement)`. ConfigOptionQuerySource sets `prefix_start: 0` as a placeholder; before applying, the port should override it with the actual SlashArg `prefix_start` (from the trigger state).

Modify the `ReplaceTriggerToken` arm in `apply_accept`:

```rust
RetrievalAccept::ReplaceTriggerToken { prefix_start: _, replacement } => {
    let real_prefix_start = self.trigger_detector.current_prefix_start().unwrap_or(0);
    replace_trigger_token(input_bar, real_prefix_start, &replacement);
}
```

Add a method on TriggerDetector if absent:
```rust
pub fn current_prefix_start(&self) -> Option<usize> {
    match &self.state {
        TriggerState::Composing { prefix_start, .. } => Some(*prefix_start),
        _ => None,
    }
}
```

Note: ConfigOptionQuerySource's accept computes the FULL replacement (from `/`); the prefix_start should therefore be 0 (replace from beginning of `/` token, not from arg start). The detector's `prefix_start` for SlashArg points to the arg-start. Two prefixes mean two strategies:

Option A: `accept` returns `replacement = "<value>"` only; port replaces from arg-start → buffer becomes `"/model gpt-5-codex"`.
Option B: `accept` returns `replacement = "/model <value>"`; port replaces from beginning of slash → buffer becomes `"/model gpt-5-codex"` (same outcome).

Pick Option A — simpler. Modify `ConfigOptionQuerySource::accept`:

```rust
fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
    let value = self.choices.get(row_idx)?.value.clone();
    Some(RetrievalAccept::ReplaceTriggerToken {
        prefix_start: 0,        // Re-set by InputCompletionPort to arg-start
        replacement: value,     // Just the value; port appends to existing /<cmd>
    })
}
```

Then in `apply_accept`:
```rust
RetrievalAccept::ReplaceTriggerToken { prefix_start: _, replacement } => {
    let real_prefix_start = self.trigger_detector.current_prefix_start().unwrap_or(0);
    replace_trigger_token(input_bar, real_prefix_start, &replacement);
}
```

`replace_trigger_token` then replaces `[prefix_start..cursor]` with `<value>`, leaving the `/<cmd> ` prefix intact.

- [ ] **Step 4: Run tests on input_completion**

Run: `cargo test -p spur-tui input_completion 2>&1 | tail -15`
Expected: existing tests pass; iterate any failures.

Add an integration test if absent:
```rust
#[test]
fn slash_arg_open_instantiates_config_option_query_source() {
    // Set up a test registry with /model registered;
    // dispatch a step that opens SlashArg{command_name:"model"};
    // assert self.picker_shell is Some;
    // assert picker_shell.title() returns "Model".
}
```

- [ ] **Step 5: Commit**

Run:
```sh
git add crates/spur-tui/src/components/input_completion.rs crates/spur-tui/src/components/completion_trigger.rs crates/spur-tui/src/components/config_option_query_source.rs
git commit -m "feat(spur-tui): InputCompletionPort dispatches ArgPickerSpec to QuerySource

When TriggerDetector emits SlashArg, port looks up arg_picker_spec on
the registry, switches on typed_hint, instantiates the right QuerySource.
ConfigOption hint → ConfigOptionQuerySource. Prefix anchoring via
trigger_detector.current_prefix_start().

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.13: Capture `NewSessionResponse.config_options` in orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:2867-3075` (create_brain_session — capture config_options)
- Modify: `crates/spur-core/src/orchestrator.rs` (BrainSession struct — add field)

- [ ] **Step 1: Locate the BrainSession struct**

Run: `grep -n "^pub struct BrainSession\|^struct BrainSession" crates/spur-core/src/orchestrator.rs`

Read the struct definition (~30 lines). Note all current fields.

- [ ] **Step 2: Add the cache field to BrainSession**

```rust
pub struct BrainSession {
    // ...existing fields...
    /// Latest config_options from the agent. Populated from
    /// NewSessionResponse on session creation; refreshed by
    /// SetSessionConfigOption responses (Task 2.14) and by
    /// session/update.ConfigOptionUpdate notifications (v2 separate plan).
    pub config_options: Vec<agent_client_protocol::schema::SessionConfigOption>,
}
```

- [ ] **Step 3: Populate the cache in `create_brain_session`**

Locate the `BrainSession {` literal at the end of `create_brain_session` (~L3064). Add:

```rust
Ok(BrainSession {
    // ...existing fields...
    config_options: session_response.config_options.clone(),    // NEW
})
```

If `session_response.config_options` is `Option<...>` or wrapped, adjust accordingly.

- [ ] **Step 4: Add a getter and setter on the Orchestrator (or BrainSession)**

Find where Orchestrator holds the active BrainSession. Add:

```rust
pub fn session_config_options(&self, session_id: &SessionId) -> Vec<agent_client_protocol::schema::SessionConfigOption> {
    self.brains.get(session_id)
        .map(|b| b.config_options.clone())
        .unwrap_or_default()
}

pub fn replace_session_config_options(
    &mut self,
    session_id: &SessionId,
    opts: Vec<agent_client_protocol::schema::SessionConfigOption>,
) {
    if let Some(b) = self.brains.get_mut(session_id) {
        b.config_options = opts;
        // Emit registry-dirty signal so spur-tui rebuilds the registry on next ensure_cache.
        self.emit(SpurEvent::now(SpurEventBody::CommandRegistryDirty {
            session: session_id.clone(),
        }));
    }
}
```

This requires adding `CommandRegistryDirty` variant to `SpurEventBody` (read the enum, find its location, add variant). If a similar event already exists (`SessionConfigOptionsChanged`?), reuse it.

- [ ] **Step 5: Write a test that exercises the cache**

In `crates/spur-core/tests/`, add `orchestrator_session_config_options.rs`:

```rust
// Use existing test harness pattern (find by greping tests/ for similar Orchestrator setup).
// Construct a mock connection that returns NewSessionResponse with non-empty config_options.
// Call create_brain_session.
// Assert orchestrator.session_config_options(...) returns the same options.
// Then call orchestrator.replace_session_config_options(...) with new options.
// Assert getter returns the new set.
```

Adapt to the existing orchestrator-test patterns (kimi confirmed `crates/spur-core/tests/notification_pump_integration.rs` has the right shape).

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-core orchestrator 2>&1 | tail -10`
Expected: green.

- [ ] **Step 7: Commit**

Run:
```sh
git add crates/spur-core/src/orchestrator.rs crates/spur-core/tests/orchestrator_session_config_options.rs
git commit -m "feat(spur-core): orchestrator caches NewSessionResponse.config_options

Stored on BrainSession; getter + setter expose it. Setter emits
CommandRegistryDirty so spur-tui rebuilds on next ensure_cache.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.14: Add `InteractiveInput::SetSessionConfigOption` variant + handler

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:550-621` (add InteractiveInput variant)
- Modify: `crates/spur-core/src/orchestrator.rs:1972-1996` (add handler after SetSessionMode)

- [ ] **Step 1: Add the InteractiveInput variant**

In the `InteractiveInput` enum (`orchestrator.rs:550-621`), after `SetSessionMode { mode_id }`, add:

```rust
SetSessionConfigOption { config_id: String, value: String },
```

- [ ] **Step 2: Add the handler**

After the `SetSessionMode` handler (`orchestrator.rs:1972-1996`), add:

```rust
InteractiveInput::SetSessionConfigOption { config_id, value } => {
    if let Some(b) = brain.as_mut() {
        let req = agent_client_protocol::schema::SetSessionConfigOptionRequest::new(
            agent_client_protocol::schema::SessionId::new(b.acp_session_id.clone()),
            agent_client_protocol::schema::SessionConfigId::new(config_id.clone()),
            agent_client_protocol::schema::SessionConfigOptionValue::value_id(value.clone()),
        );
        match b.connection.set_session_config_option(req).await {
            Ok(resp) => {
                let session_id = b.spur_session_id.clone();
                self.replace_session_config_options(&session_id, resp.config_options);
            }
            Err(e) => {
                warn!(
                    error = %e,
                    config_id = %config_id,
                    value = %value,
                    "set_session_config_option failed"
                );
                self.emit(SpurEvent::now(SpurEventBody::Toast {
                    severity: ToastSeverity::Error,
                    message: format!("Failed to set {config_id}: {e}"),
                }));
            }
        }
    } else {
        warn!(
            config_id = %config_id,
            value = %value,
            "SetSessionConfigOption received but no active brain session"
        );
    }
}
```

The `SessionConfigOptionValue::value_id(value)` constructor name reflects the schema's enum variant (likely `ValueId(String)`); verify by reading `SessionConfigOptionValue` definition. If different, adjust.

`Toast`/`ToastSeverity` likely already exist in spur-core — search for them:
```sh
grep -rn "SpurEventBody::Toast\|ToastSeverity" crates/spur-core/
```

If they don't exist (less likely), use whatever existing error-surface mechanism orchestrator already uses (search for `warn!` followed by an `emit` call in the SetSessionMode path).

- [ ] **Step 3: Add a test**

In `crates/spur-core/tests/`, add or extend a test exercising SetSessionConfigOption end-to-end (mock connection echoes the request). Verify cache is updated.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core 2>&1 | tail -10`
Expected: green.

- [ ] **Step 5: Commit**

Run:
```sh
git add crates/spur-core/
git commit -m "feat(spur-core): InteractiveInput::SetSessionConfigOption variant + handler

Sends ACP set_config_option, refreshes orchestrator cache from response,
toasts on error. Mirrors SetSessionMode plumbing pattern.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.15: Route `Dispatch::SetSessionConfigOption` in submit_router

**Files:**
- Modify: `crates/spur-tui/src/commands/submit_router.rs:26-42` (add SubmitDecision variant)
- Modify: `crates/spur-tui/src/commands/submit_router.rs:45-N` (route function)
- Modify: spur-cli or app.rs (whichever wires SubmitDecision → InteractiveInput)

- [ ] **Step 1: Add the SubmitDecision variant**

In `crates/spur-tui/src/commands/submit_router.rs:26-42`, add:

```rust
SetSessionConfigOption {
    config_id: String,
    value: String,
},
```

- [ ] **Step 2: Add a route arm**

In the `route` function, find where Dispatch::PromptText is routed. Add an arm for `Dispatch::SetSessionConfigOption { config_id }`:

```rust
Dispatch::SetSessionConfigOption { config_id } => {
    // The arg is whatever the user typed after `/<cmd> `. Parse it.
    let cmd_name = entry.name.as_str();
    let Some(arg) = parse_slash_arg(text, cmd_name) else {
        return SubmitDecision::Empty;  // No arg yet; the picker handles this
    };
    if arg.is_empty() {
        return SubmitDecision::Empty;
    }
    SubmitDecision::SetSessionConfigOption {
        config_id: config_id.clone(),
        value: arg.to_string(),
    }
}
```

The helper `parse_slash_arg(text, cmd_name) -> Option<&str>` extracts the substring after `/<cmd_name> `. Implement inline or in a util module.

Validate the value against the registered choices (E3 from spec §3.3): if `value` isn't in the cached choices, return `SubmitDecision::Error(...)` with a message listing valid options. Look up choices via the registry/cache.

- [ ] **Step 3: Wire the SubmitDecision through to InteractiveInput**

Find the existing routing (likely in `crates/spur-cli/src/main.rs` or wherever SubmitDecision::VendorExec is mapped to UserInput::VendorExec). Add a parallel mapping for SubmitDecision::SetSessionConfigOption → UserInput::SetSessionConfigOption → InteractiveInput::SetSessionConfigOption.

The `UserInput` enum lives in spur-tui/src/action.rs (probably). Add the variant. Then in spur-cli, map UserInput → InteractiveInput.

- [ ] **Step 4: Add tests**

Test that valid arg routes correctly; invalid arg returns error variant.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui submit_router && cargo test -p spur-cli 2>&1 | tail -10`
Expected: green.

- [ ] **Step 6: Commit**

Run:
```sh
git add crates/spur-tui/src/commands/submit_router.rs crates/spur-tui/src/action.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-tui+cli): route Dispatch::SetSessionConfigOption end-to-end

Submit-router parses the slash arg, validates against cached choices,
emits SubmitDecision::SetSessionConfigOption. spur-cli forwards as
InteractiveInput::SetSessionConfigOption to orchestrator.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.16: Wire orchestrator → TUI — make AdvertisedSource entries visible

**Files:**
- Modify: wherever the TUI pulls per-session command lists. Find via:
  `grep -rn "set_agent_commands\|CommandRegistry" crates/spur-tui/src/views/`

- [ ] **Step 1: Locate where set_agent_commands is called from**

Per kimi: `crates/spur-tui/src/views/session_detail.rs:619` (apply_available_commands).

Read 30 lines around it to see the context.

- [ ] **Step 2: Add a parallel call to set_advertised_commands**

Wherever the orchestrator surfaces fresh per-session state to the TUI (e.g. when AgentSessionReady fires), build the advertised entries from the cached config_options and call `registry.set_advertised_commands(handle, entries)`.

Likely sites:
- `app.rs` handling of `SpurEventBody::AgentSessionReady` — fetch `orchestrator.session_config_options(session_id)`, call `AdvertisedSource::entries(handle, &opts)`, then `registry.set_advertised_commands(handle, entries)`.
- Same handler for `SpurEventBody::CommandRegistryDirty` (the new event from Task 2.13).

Read the existing `apply_session_update::AvailableCommandsUpdate` arm (per kimi: `app.rs:2626-2637`) for the exact pattern to mirror.

- [ ] **Step 3: Add an integration test**

In `crates/spur-tui/tests/`, add a test that:
- Creates a mock orchestrator with cached config_options for `model` and `reasoning_effort`.
- Triggers AgentSessionReady.
- Asserts the registry's `iter_entries` contains `/model` and `/effort` entries with `CommandSource::Advertised`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui 2>&1 | tail -10`
Expected: green.

- [ ] **Step 5: Commit**

Run:
```sh
git add crates/spur-tui/src/
git commit -m "feat(spur-tui): /model and /effort appear in slash popup on session ready

apply_session_update handler now also calls AdvertisedSource::entries
on cached config_options and feeds them via set_advertised_commands.
Refreshes on CommandRegistryDirty event.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.17: End-to-end smoke test (mock-codex)

**Files:**
- Create or extend: `crates/spur-tui/tests/codex_model_picker_smoke.rs`

- [ ] **Step 1: Find the existing mock-codex test pattern**

Run: `find crates -name "*.rs" | xargs grep -l "mock-codex\|mock_codex\|MockAgentConnection" 2>/dev/null`
Note the file. Read for the harness pattern.

- [ ] **Step 2: Write the smoke test**

```rust
#[tokio::test]
async fn codex_model_picker_end_to_end() {
    // Construct an orchestrator with a mock connection that returns
    // NewSessionResponse.config_options including a Select for "model"
    // with 3 choices and a Select for "reasoning_effort" with 3 choices.

    // Trigger session creation; assert orchestrator.session_config_options
    // returns the 2 options.

    // Assert AdvertisedSource::entries produces /model and /effort
    // CommandEntry rows with Some(arg_picker_spec).

    // Simulate user typing "/model gpt-5-codex" and submitting.
    // Assert mock connection's set_session_config_option was called
    // with config_id="model", value="gpt-5-codex".

    // Assert the orchestrator cache was updated from the mock response.
}
```

The harness boilerplate depends on existing patterns. Use the smallest possible orchestrator setup that exposes the relevant surface.

- [ ] **Step 3: Run the test**

Run: `cargo test -p spur-tui codex_model_picker_smoke 2>&1 | tail -15`
Expected: green.

- [ ] **Step 4: Commit**

Run:
```sh
git add crates/spur-tui/tests/codex_model_picker_smoke.rs
git commit -m "test(spur-tui): end-to-end smoke for /model picker via mock-codex

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.18: Final verification

**Files:** none modified.

- [ ] **Step 1: Full workspace test**

Run: `cargo test --workspace --no-fail-fast 2>&1 | tee /tmp/spur-final-tests.log; tail -20 /tmp/spur-final-tests.log`
Expected: all green; new tests visible in the count.

- [ ] **Step 2: Compare against Phase 1 baseline**

Run: `diff <(grep "^test result:" /tmp/spur-postupgrade-tests.log) <(grep "^test result:" /tmp/spur-final-tests.log)`
Expected: only counts increased (more tests, same passes); no failures.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: clean.

- [ ] **Step 4: Format check**

Run: `cargo fmt --check 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 5: Show full Phase 1 + 2 commit list**

Run: `git log --oneline 3def8557..HEAD`
Expected: ~7 commits for Phase 1 + ~17 commits for Phase 2 = ~24 commits total.

- [ ] **Step 6: Manual smoke (impossible to automate without a real codex-acp install)**

Document only — no automation:

If a codex-acp binary is available locally and the user wants to run an end-to-end manual check:
1. Start spur connected to codex-acp.
2. Type `/m` in the input bar; expect `/model` (and `/effort`) to appear in the popup.
3. Pick `/model`; type a fuzzy query; pick a value.
4. Confirm the buffer reads `/model <value>` and submit.
5. Confirm the codex agent acknowledges the model change in its next message.

If no codex-acp install is available, this step is skipped and the smoke test from Task 2.17 is the only verification.

---

## Done

After all tasks pass:

```sh
git log --oneline 3def8557..HEAD | wc -l
```

Expected output: ~24. The plan is complete when the workspace is green and all tests added in Phase 2 pass.

If any step blocked or surfaced unexpected complexity (worker signal: scope_drift / risk per spurpower-worker-signals), pause and surface to the brain before continuing.
