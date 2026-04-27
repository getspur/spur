# ACP-first arg pickers v2 — PR-2 + PR-3 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` for execution. Each task uses checkbox (`- [ ]`) syntax. **TDD discipline:** write the failing test in the same task as the implementation; commit red → green → refactor.

**Goal:** Ship the two v2 features that survived design close and have a production wire fixture today (verified 2026-04-27 against `npx --yes @zed-industries/codex-acp@0.12.0`):

- **PR-2** — `ConfigOptionUpdate` cache-freshness arm. Without this, v1's `/model` and `/effort` are *empty* on any session that's loaded/resumed (`orchestrator.rs:3557-3561` initialises `config_options: Vec::new()`).
- **PR-3** — free-text arg picker for any agent-advertised slash command whose `AvailableCommand.input == Some(Unstructured(...))`. Codex emits 3 such commands today (`/review`, `/review-branch`, `/review-commit`) — captured wire shapes in `crates/spur-acp/tests/data/codex_acp_0_12_new_session_response.json` and `tests/codex_0_12_wire_probe.rs`.

**Out of scope (explicitly):**
- **PR-4** — `GitRefQuerySource` + `_meta._<vendor>.dev.arg_picker_hint` parsing. Probe of codex-acp 0.12.0 confirms it does NOT yet emit `_meta`. Defer until codex Track-1 (spec §11.1) lands.
- **Multi-arg commands** — single-arg only (per spec §14).
- **`/mode`** — collides with spur-local `/mode` (per spec §2).
- **Real-time `git` ref refresh** — picker snapshots at open time (per spec §14).

**Architecture:**
- PR-2 routes a `ConfigOptionUpdate` notification through the existing `notification_pump → app::apply_session_update` chain into `orchestrator::replace_session_config_options`, which already emits `CommandRegistryDirty` (verified `orchestrator.rs:3232-3240` for the `new_session` path).
- PR-3 extends `arg_picker_hint::parse(&AvailableCommand) -> Option<ArgPickerSpec>` to read `cmd.input` (free-text hint) and produce `typed_hint: None` for `Unstructured`. `CommandRegistry::set_agent_commands` calls the parser. `InputCompletionPort` dispatches `typed_hint == None` to a new `CommandInputQuerySource` (degenerate single-row free-text picker), which is wired into the existing `TriggerKind::SlashArg` state machine. **Zero per-command, per-vendor branches in spur-tui.**

**Tech stack:** Rust 2021. SDK `agent-client-protocol = "0.11.1"` with `unstable_session_model`. `tokio` broadcast. Existing `nucleo_matcher` (no new deps).

**Specs:**
- Primary: `docs/superpowers/specs/2026-04-27-acp-first-arg-pickers-v2-design.md` (sections §6.1, §6.2, §6.3, §6.6)
- Background: prior v1 plan `docs/superpowers/plans/2026-04-27-acp-upgrade-and-codex-model-effort-pickers.md`
- Wire reality: `crates/spur-acp/tests/codex_0_12_wire_probe.rs`

---

## Wave A — PR-2: `ConfigOptionUpdate` cache-freshness

**Why first:** PR-2 is small (~30 LOC + tests) and self-contained. It also fixes the loaded-session empty-picker bug independently, so it can ship even if PR-3 review takes time.

### Task A.1: Failing integration test for the loaded-session bug

**Files:**
- New: `crates/spur-tui/tests/config_option_update_arm.rs`

- [ ] **Step 1: Write a TUI integration test** that constructs a `SessionDetailView`, simulates a `SpurEventBody::AgentNotification` carrying a `SessionUpdate::ConfigOptionUpdate(...)` payload (model: `gpt-5` → `gpt-5.5`), and asserts that `session_config_options_for_test()` reflects the new payload. Mirror the shape of the existing `crates/spur-tui/tests/advertised_commands_event.rs` test for `CommandRegistryDirty`.

- [ ] **Step 2: Run and confirm it fails**

```sh
timeout 300 cargo test -p spur-tui --test config_option_update_arm 2>&1 | tail -20
```
Expected: failure (the `ConfigOptionUpdate` arm doesn't exist yet — the catch-all `_ =>` swallows it).

- [ ] **Step 3: Commit red**

```
test(spur-tui): red — ConfigOptionUpdate session-update arm
```

### Task A.2: Add `ConfigOptionUpdate` arm to `apply_session_update`

**Files:**
- Modify: `crates/spur-tui/src/app.rs:2810-2824` (the `apply_session_update` match)
- Modify: `crates/spur-tui/src/views/session_detail.rs` — add `apply_config_option_update(opts)` if a wrapper makes the call site cleaner (optional; can inline)

- [ ] **Step 1: Add the arm**

Add a `SessionUpdate::ConfigOptionUpdate(payload)` arm that calls into the same code path `CommandRegistryDirty` uses today — i.e. `session_detail.apply_advertised_commands(&payload.config_options)`.

The existing handler at `views/session_detail.rs:646` already does the registry rebuild; PR-2 just needs to plumb the notification arm to call it.

- [ ] **Step 2: Verify the test now passes**

```sh
timeout 300 cargo test -p spur-tui --test config_option_update_arm 2>&1 | tail -20
```

- [ ] **Step 3: Commit green**

```
feat(spur-tui): consume ConfigOptionUpdate session-update notifications
```

### Task A.3: Plumb `session_update_variant_name` for log clarity

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:1596-1606`

- [ ] **Step 1: Add `ConfigOptionUpdate => "config_option_update"`** so log lines aren't tagged `"other"`.

- [ ] **Step 2: Run spur-acp tests crate-by-crate (5-min timeout)**

```sh
timeout 300 cargo test -p spur-acp --lib 2>&1 | tail -10
```

- [ ] **Step 3: Commit**

```
feat(spur-acp): name ConfigOptionUpdate variant for diagnostics
```

### Task A.4: Wave A sweep + 3-gate review

- [ ] **Per-crate test sweep with 5-min timeouts:**

```sh
timeout 300 cargo test -p spur-acp --lib && \
timeout 300 cargo test -p spur-core --lib && \
timeout 300 cargo test -p spur-tui --lib && \
timeout 300 cargo fmt --all -- --check
```

- [ ] **Dispatch 3-gate review** (parallel codex/gemini/kimi) on `git diff <wave-A-base>..HEAD`. Mirror Phase 2 review prompts: codex for SDK API correctness, gemini for SPUR invariant preservation + end-to-end data flow, kimi for fmt/clippy/Cargo hygiene.

- [ ] **Address findings**, then commit any cleanup as `style(...)` or `fix(...)` per the prior phase's pattern.

---

## Wave B — PR-3: free-text arg picker

### Task B.1: Failing parser test

**Files:**
- Modify: `crates/spur-acp/src/adapter/arg_picker_hint.rs` — add tests *only* in this commit

- [ ] **Step 1: Add tests** for a future `parse(&AvailableCommand) -> Option<ArgPickerSpec>` covering:

  - `cmd.input.is_none()` → `None`
  - `cmd.input == Some(Unstructured{ hint: "branch name" })` → `Some(ArgPickerSpec { free_text_hint: "branch name", typed_hint: None })`
  - empty hint → `Some(ArgPickerSpec { free_text_hint: "", typed_hint: None })`

- [ ] **Step 2: Run, confirm fails (function does not exist)**

```sh
timeout 300 cargo test -p spur-acp adapter::arg_picker_hint 2>&1 | tail -10
```

- [ ] **Step 3: Commit red**

### Task B.2: Implement `parse()`

**Files:**
- Modify: `crates/spur-acp/src/adapter/arg_picker_hint.rs`

- [ ] **Step 1: Implement** the function as defined in spec §6.1, **but with `typed_hint` always `None` for now** (PR-4 will wire the `_meta` branch). Keep the existing `ArgPickerHint::ConfigOption` variant intact so v1's synthesizer continues to compile.

- [ ] **Step 2: Tests pass**

- [ ] **Step 3: Commit green**

### Task B.3: Failing registry test for auto-derived spec

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs` — add a test asserting that when `set_agent_commands` is called with a `Vec<AvailableCommand>` containing a command with `input = Some(Unstructured{...})`, `arg_picker_spec(name)` returns `Some(ArgPickerSpec{ free_text_hint, typed_hint: None })`.

- [ ] **Step 1: Add test, confirm fails** (today the entry's `arg_picker_spec` is always `None` because the caller pre-builds entries; the registry doesn't auto-derive).

- [ ] **Step 2: Commit red**

### Task B.4: Auto-derive `arg_picker_spec` in `set_agent_commands`

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs:72-79` (`set_agent_commands`)
- Modify: callers that build `CommandEntry` from `AvailableCommand` so they set `arg_picker_spec` from `parse(&cmd)`. Likely `crates/spur-tui/src/agents/*.rs` builders — verify with grep.

- [ ] **Step 1: Locate all callers**

```sh
grep -rn "set_agent_commands\|build_entry" crates/spur-tui/src/ | head
```

- [ ] **Step 2: Update the entry builder** to call `spur_acp::adapter::arg_picker_hint::parse(&cmd)` and store the result on `CommandEntry.arg_picker_spec`.

- [ ] **Step 3: Tests pass**

- [ ] **Step 4: Commit green**

### Task B.5: Failing test for `CommandInputQuerySource`

**Files:**
- New: `crates/spur-tui/src/components/command_input_query_source.rs`

- [ ] **Step 1: Add the file with tests only** for `CommandInputQuerySource` per spec §6.3:
  - `title()` returns `free_text_hint`
  - `refresh("foo")` returns one synthetic row capturing `"foo"`
  - `accept(0)` returns `RetrievalAccept::ReplaceTriggerToken { replacement: "/<command> foo" }`

- [ ] **Step 2: Confirm fails** (struct does not exist)

- [ ] **Step 3: Commit red**

### Task B.6: Implement `CommandInputQuerySource`

**Files:**
- Modify: same file as B.5
- Modify: `crates/spur-tui/src/components/mod.rs` to export the new module

- [ ] **Step 1: Implement** per spec §6.3. Mirror `ConfigOptionQuerySource` shape so reviewers can diff easily.

- [ ] **Step 2: Tests pass**

- [ ] **Step 3: Commit green**

### Task B.7: Wire `InputCompletionPort` to dispatch on `typed_hint`

**Files:**
- Modify: `crates/spur-tui/src/components/input_completion.rs`

- [ ] **Step 1: In the `SlashArg` arm, add a match on `spec.typed_hint`:**
  - `Some(ArgPickerHint::ConfigOption{...})` → existing `ConfigOptionQuerySource` path (unchanged)
  - `None` → new `CommandInputQuerySource::new(command_name, spec.free_text_hint)`
  - Future variants (`GitRef`) — deliberately omit; reviewers should call out if added without a test.

- [ ] **Step 2: Add a unit test** asserting the dispatch switch (use the existing test scaffolding in `input_completion.rs`).

- [ ] **Step 3: Commit**

### Task B.8: Submit-router arm for free-text dispatch

**Files:**
- Inspect: `crates/spur-tui/src/commands/submit_router.rs`

- [ ] **Step 1: Verify** that `Dispatch::PromptText` already handles `/review-branch main`-style submits as plain prompt text. If a separate arm is needed for "advertised command, free-text payload", add it; if not, document that PromptText is sufficient.

- [ ] **Step 2: Add a regression test** that simulates the user typing `/review-branch main`, hitting Enter, and asserts `SubmitDecision::PromptText { text: "/review-branch main" }`.

- [ ] **Step 3: Commit**

### Task B.9: End-to-end smoke test

**Files:**
- New: `crates/spur-tui/tests/codex_review_branch_picker_smoke.rs`

- [ ] **Step 1: Compose a happy-path test** modeled on `tests/codex_model_picker_smoke.rs`:
  - Inject the captured `available_commands_update` payload (we have it in `crates/spur-acp/tests/data/`)
  - Type `/review-branch ` in the input bar
  - Assert: trigger detector enters `SlashArg{ command_name: "review-branch" }`; popup uses `CommandInputQuerySource`; `title()` returns `"branch name"`.

- [ ] **Step 2: Test passes; commit**

### Task B.10: Wave B sweep + 3-gate review

- [ ] **Per-crate test + fmt sweep** as in A.4.

- [ ] **Dispatch 3-gate review** parallel codex/gemini/kimi on `git diff <wave-B-base>..HEAD`.

- [ ] **Address findings.**

---

## Build sequence

Wave A → Wave B (B.1 must follow A.4). Inside each wave, tasks are linear because each implementation step depends on its red-phase test compiling.

## Acceptance criteria

- [ ] `timeout 300 cargo test -p spur-acp` passes (171+/171+ lib).
- [ ] `timeout 300 cargo test -p spur-core` passes (278+/278+ lib).
- [ ] `timeout 300 cargo test -p spur-tui` passes (436+/436+ lib + new tests).
- [ ] `cargo fmt --all -- --check` returns 0 lines.
- [ ] `crates/spur-tui/tests/codex_review_branch_picker_smoke.rs` opens a `CommandInputQuerySource` for `/review-branch`.
- [ ] Manual: open spur on a fresh codex session → type `/review-branch ` → arg picker opens with placeholder "branch name".
- [ ] Manual: open spur on a *resumed* codex session → `/model` and `/effort` populate within ~1s of the session/load completing (relies on codex emitting `ConfigOptionUpdate` after load — TBD; if codex doesn't emit, document as upstream limitation and the loaded-session bug remains for v3).

## Risk register

| # | Risk | Mitigation |
|---|---|---|
| R1 | Codex doesn't actually emit `ConfigOptionUpdate` after `session/load` | Probe via `node scripts/probe-codex-acp.mjs --load`; if confirmed missing, file Track-1 issue and document the remaining loaded-session gap. PR-2 still fixes any case where any agent emits the notification. |
| R2 | `CommandEntry.arg_picker_spec` auto-derivation breaks existing dynamic-command callers that intentionally set `None` | Keep the auto-derivation gated to the `set_agent_commands` *new* code path; don't touch advertised/dynamic distinctions. |
| R3 | `RetrievalAccept::ReplaceTriggerToken` is `#[allow(dead_code)]` | Removing the allow is part of B.6; reviewer should call out if it's still dead after B.10. |
| R4 | Submit of free-text breaks v1's existing `/model gpt-5` SetSessionConfigOption dispatch | B.8's regression test guards this. |
