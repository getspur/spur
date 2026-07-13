# Grok Interactive Model and Effort Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add working Grok `/model` and `/effort` pickers backed by the live-proven `session/set_model` wire method while leaving standard ACP `configOptions` behavior unchanged.

**Architecture:** Extend the existing Grok-only metadata snapshot into a catalog of real model IDs and per-model reasoning efforts. TUI commands synthesized from that catalog use dedicated dispatch variants; native ACP sends an untyped `session/set_model` request, with `_meta.reasoningEffort` only for effort changes. `_x.ai/session_notification` `model_changed` updates a native current-model cache and the TUI's cloned capability snapshot.

**Tech Stack:** Rust 2021, `agent-client-protocol` 1.0 generic request envelope, Tokio channels, Serde JSON, ratatui command registry.

---

### Task 1: Grok catalog and live state

**Files:**
- Modify: `crates/spur-acp/src/adapter/grok_session_display.rs`
- Modify: `crates/spur-acp/src/spur_agent_caps.rs`

- [ ] **Step 1: Write failing catalog tests**

Add fixtures proving that sessionConfig model options become real model choices, `initialize._meta.modelState.availableModels[*]._meta.reasoningEfforts` enriches each model, the composer model has no effort choices, and `model_changed` replaces the selected model/effort labels.

- [ ] **Step 2: Verify the tests fail**

Run: `scripts/spur-cargo test -p spur-acp grok_session_display -- --nocapture`

Expected: FAIL because catalog choices and `model_changed` mutation APIs do not exist.

- [ ] **Step 3: Implement the catalog**

Extend the Grok snapshot with serializable model/effort choice types and methods equivalent to:

```rust
pub fn model_choices(&self) -> impl Iterator<Item = (&str, &str)>;
pub fn current_effort_choices(&self) -> impl Iterator<Item = (&str, &str)>;
pub fn apply_model_changed(&mut self, params: &Value) -> bool;
```

Only accept `high`, `medium`, and `low` as effort IDs. Merge sessionConfig labels with modelState per-model effort lists, and do not populate standard `config_options`.

- [ ] **Step 4: Verify green**

Run: `scripts/spur-cargo test -p spur-acp grok_session_display -- --nocapture`

Expected: PASS.

### Task 2: Native proven wire path

**Files:**
- Modify: `crates/spur-acp/src/connection/mod.rs`
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Write failing dispatch and JSON tests**

Add tests requiring Grok catalog caps to choose a `DirectSetModel` decision, standard model config options to retain `FallbackConfigOption`, and effort parameters to serialize exactly as:

```json
{"sessionId":"sid","modelId":"grok-4.5","_meta":{"reasoningEffort":"low"}}
```

- [ ] **Step 2: Verify red**

Run: `scripts/spur-cargo test -p spur-acp set_session_model_dispatch_tests -- --nocapture`

Expected: FAIL because the direct decision and effort payload builder are absent.

- [ ] **Step 3: Implement native commands**

Add a default `AgentConnection::set_session_effort` capability error. In `NativeAcpConnection`, send `ClientRequest::ExtMethodRequest(ExtRequest)` whose method is exactly `session/set_model`; model requests carry only `sessionId` and `modelId`, while effort requests re-send the native cache's current model and add `_meta.reasoningEffort`. Update the cache from successful model calls and `_x.ai/session_notification` `model_changed` payloads.

- [ ] **Step 4: Verify green**

Run: `scripts/spur-cargo test -p spur-acp set_session_model_dispatch_tests -- --nocapture`

Expected: PASS, with existing Codex/config-option tests unchanged.

### Task 3: Dedicated Grok slash commands

**Files:**
- Modify: `crates/spur-tui/src/commands/entry.rs`
- Modify: `crates/spur-tui/src/commands/advertised.rs`
- Modify: `crates/spur-tui/src/commands/registry.rs`
- Modify: `crates/spur-tui/src/commands/submit_router.rs`

- [ ] **Step 1: Write failing synthesis and gate tests**

Require Grok catalog caps with empty config options to synthesize `/model` with `Dispatch::SetSessionModel`, synthesize `/effort` with `Dispatch::SetSessionEffort` only for the selected model's non-empty effort list, and survive registry capability filtering without claiming `supports_set_config_option`.

- [ ] **Step 2: Verify red**

Run: `scripts/spur-cargo test -p spur-tui commands:: -- --nocapture`

Expected: FAIL because dedicated dispatch variants and Grok synthesis are absent.

- [ ] **Step 3: Implement synthesis and routing**

Keep `adapter/config_options.rs` unchanged. Append Grok-only entries in `AdvertisedSource::entries_from_caps`; route dedicated model/effort entries directly to `SubmitDecision::SetSessionModel` and `SubmitDecision::SetSessionEffort`.

- [ ] **Step 4: Verify green**

Run: `scripts/spur-cargo test -p spur-tui commands:: -- --nocapture`

Expected: PASS.

### Task 4: End-to-end effort action and label refresh

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app/mod.rs`
- Modify: `crates/spur-tui/src/app/action_routing/session_config.rs`
- Modify: `crates/spur-tui/src/views/session_detail/events.rs`
- Modify: `crates/spur-core/src/orchestrator/input.rs`
- Modify: `crates/spur-core/src/orchestrator/interactive_loop.rs`
- Modify: `crates/spur-core/src/orchestrator/session.rs`

- [ ] **Step 1: Write failing routing and notification tests**

Prove an effort picker acceptance reaches `InteractiveInput::SetSessionEffort`, and a scoped Grok `model_changed` event replaces status labels and removes `/effort` after switching to the composer model.

- [ ] **Step 2: Verify red**

Run: `scripts/spur-cargo test -p spur-tui set_session_effort -- --nocapture`

Expected: FAIL because the action/input path is absent.

- [ ] **Step 3: Implement the glue**

Thread `SetSessionEffort { session_id, value }` through TUI action/user input and core interactive input. Dispatch to `AgentConnection::set_session_effort`. On matching Grok extension notifications, clone and mutate the view caps, then rebuild advertised commands from the updated catalog.

- [ ] **Step 4: Verify green**

Run: `scripts/spur-cargo test -p spur-tui set_session_effort -- --nocapture`

Expected: PASS.

### Task 5: Final verification and delivery

**Files:**
- Modify if needed: `docs/superpowers/specs/2026-07-13-grok-acp-capability-probe-results.md`

- [ ] **Step 1: Format**

Run: `scripts/spur-cargo fmt --all`

- [ ] **Step 2: Run affected suites**

Run: `scripts/spur-cargo test -p spur-acp`

Run: `scripts/spur-cargo test -p spur-tui`

Run: `scripts/spur-cargo test -p spur-core`

- [ ] **Step 3: Lint remotely**

Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-acp -p spur-core -p spur-tui --all-targets -- -D warnings`

- [ ] **Step 4: Review and commit**

Inspect `git diff --check`, `git diff`, and `git status --short`. Commit with:

```text
feat(spur-acp): grok-model wire interactive model and effort
```

