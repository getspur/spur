# Capability-aware spur — M8 implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:subagent-driven-development` for execution. Each task uses checkbox (`- [ ]`) syntax. **TDD discipline:** write the failing test in the same task as the implementation; commit red → green → refactor.

**Goal:** Ship M8 from `docs/superpowers/specs/2026-04-27-acp-capability-aware-spur-design.md` — a frozen-per-session `SpurAgentCaps` cache + explicit `ClientCapabilities` literal + capability-gated `set_session_model` + `authenticate` `_meta` plumbing + `SessionInfoUpdate` arm. Closes 4 of 6 user-felt issues from the v2 grounding pass.

**Out of scope (this plan):**
- **M9** — `adapter/codex.rs` terminal extraction; `adapter/opencode.rs` rawOutput. Separate plan, gated on M8.A landing.
- **M10** — typed `GitRefQuerySource` (renames v2 PR-4). Separate plan, gated on M9 + codex-acp upstream Track-1 PR merging.
- **Auth dialog UX rework.** M8.D ships minimal request-`_meta` plumbing only; full UI for gemini gateway / kimi terminal-auth is a separate spec.
- **Mid-session capability renegotiation.** ACP 0.12 has no protocol affordance.

**Architecture summary:**
- `SpurAgentCaps { raw: AgentCapabilities }` newtype lives in `crates/spur-acp/src/spur_agent_caps.rs`.
- Cached on the orchestrator session entry, parallel to `config_options`, behind `Arc<SpurAgentCaps>` for cheap UI clones.
- `NativeAcpConnection::initialize` builds an explicit `ClientCapabilities` literal including `meta["terminal_output"]=true`; captures `InitializeResponse.capabilities` into the cache.
- `NativeAcpConnection::set_session_model` reads cached caps and dispatches `SetSessionModelRequest` if advertised, else falls back to `set_session_config_option`, else returns `CapabilityMissing` — **deterministic capability-gated, not runtime probe**.
- `CommandRegistry::available_commands_for_session(sid, &caps)` filters dispatch-by-dispatch; UI just renders what comes back.
- `authenticate(method_id, meta)` extends signature to plumb optional request `_meta` through.
- `apply_session_update::SessionInfoUpdate` arm — unblocks codex's session metadata refresh from being silently dropped.

**Tech stack:** Rust 2021. SDK `agent-client-protocol = "0.11.1"` with `unstable_session_model`. `tokio` broadcast. No new deps.

**Specs:**
- Primary: `docs/superpowers/specs/2026-04-27-acp-capability-aware-spur-design.md`
- Compat matrix + grounding: `docs/superpowers/specs/2026-04-27-acp-first-arg-pickers-v2-design.md` Appendix A + B
- Predecessor plan (PR-2/PR-3, executed): `docs/superpowers/plans/2026-04-27-acp-arg-pickers-v2-pr2-pr3.md`

---

## Wave 0 — SDK API verification (preconditions)

**Why first:** spec §12 lists 4 SDK questions; wrong signatures = wasted Wave A. Wave 0 is research-only; no commits.

### Task 0.1: ~~Verify `ClientCapabilities` constructor / builder pattern~~ ✅ resolved

**Resolved 2026-04-27 against `agent-client-protocol-schema-0.12.0` source (`~/.cargo/registry/.../agent-client-protocol-schema-0.12.0/src/client.rs:1514-1648`):**

- `ClientCapabilities` is `#[non_exhaustive]` with `Default + Clone` and a chained builder.
- Builder shape: `ClientCapabilities::new().fs(FileSystemCapabilities::new().read_text_file(true).write_text_file(true)).terminal(true).meta(json!({"terminal_output": true}))`.
- `terminal` is `bool`, **not a struct**. `fs` is `FileSystemCapabilities` (full name; not `FsCapabilities`).
- `InitializeRequest::new(ProtocolVersion::LATEST).client_capabilities(caps)` is the canonical request-builder shape.
- Schema 0.12.0's `Meta` accepts `serde_json::Value` via `IntoOption<Meta>`.

### Task 0.2: ~~Verify `unstable_setSessionModel` vs stable `set_session_model`~~ ✅ resolved

**Resolved 2026-04-27 against schema 0.12.0 `src/agent.rs`:**

- SDK 0.12.0 uses **stable method name `"session/set_model"`** (`SESSION_SET_MODEL_METHOD_NAME`, agent.rs:4679). `SetSessionModelRequest` is canonical.
- `AgentCapabilities` (agent.rs:3872-3928) does **NOT** have a bool flag for `session/set_model`, `session/set_mode`, or `session/set_config_option`. These are protocol-stable methods.
- **Spur derives support from `NewSessionResponse` payload presence**, not from `AgentCapabilities`:
  - `supports_set_mode` ← `modes.as_ref().is_some_and(|m| !m.available_modes.is_empty())`
  - `supports_set_model` ← `models.is_some()`
  - `supports_set_config_option` ← `!config_options.is_empty()`
- Spur's dispatch always calls `client.set_session_model(SetSessionModelRequest)`; the SDK handles wire-name resolution. Older agents that emit `unstable_setSessionModel` are out-of-scope until they upgrade (per the v2 grounding).

### Task 0.3: Probe codex `SessionInfoUpdate` payload (still pending)

- [ ] **Step 1: Extend `crates/spur-acp/tests/codex_0_12_wire_probe.rs`** with a multi-turn session: `initialize` → `session/new` → `session/prompt "what's 2+2"` → `session/prompt "and 3+3"`. Capture every `session/update` notification.
- [ ] **Step 2: Inspect captured `SessionInfoUpdate` payloads.** What fields does codex set? Title, summary, working_dir, custom timestamps?
- [ ] **Step 3: Document findings** in `crates/spur-acp/tests/data/codex_session_info_update_sample.json` as a fixture for Task E.1.

**Wave 0 acceptance:** Tasks 0.1 + 0.2 already resolved; only 0.3 remains. Can run in parallel with Wave A.

**Cargo.toml note:** the spec says `agent-client-protocol = "0.11.1"`, but `cargo tree` resolves to schema **0.12.0** (the runtime crate). All Wave A code targets the 0.12.0 schema.

---

## Wave A — Core capability cache (M8.A)

**Why first:** every other wave depends on the cache existing. Smallest possible diff to enable downstream work.

### Task A.1: Failing test for `SpurAgentCaps` newtype

**Files:**
- New: `crates/spur-acp/src/spur_agent_caps.rs`

- [ ] **Step 1: Write tests** for `SpurAgentCaps::new(&InitializeResponse, &NewSessionResponse)` and the read accessors. Per spec §6.1, the cache wraps both responses (set_* gating derives from session-create state, not from `AgentCapabilities`). Cover:
  - Empty `NewSessionResponse` (no modes/models/configOptions) → `supports_set_mode/_model/_config_option` all return `false`.
  - Codex-style fixture (3 modes, 24 models, 3 configOptions) → all 3 return `true`.
  - Gemini-style fixture (`models: Some(_)`, `configOptions: None`) → `supports_set_model: true`, `supports_set_config_option: false`.
  - `AgentCapabilities { load_session: true }` → `supports_load_session: true`.
  - `meta_capability("terminal_output")` returns `true` when `agent.meta` contains `{"terminal_output": true}`; `false` for missing key, non-bool value, or absent meta.
- [ ] **Step 2: Confirm fails** (file does not exist).
- [ ] **Step 3: Commit red**

```
test(spur-acp): red — SpurAgentCaps newtype + read accessors
```

### Task A.2: Implement `SpurAgentCaps`

**Files:**
- Modify: `crates/spur-acp/src/spur_agent_caps.rs`
- Modify: `crates/spur-acp/src/lib.rs` — add `pub mod spur_agent_caps;` and re-export.

- [ ] **Step 1: Implement** per spec §6.1. Constructor takes `(&InitializeResponse, &NewSessionResponse)` and clones `agent_capabilities`, `modes`, `models`, `config_options.unwrap_or_default()` into the cache. Load fixture from `crates/spur-acp/tests/data/codex_acp_0_12_new_session_response.json` for the codex-positive test.
- [ ] **Step 2: Tests pass.**
- [ ] **Step 3: Commit green.**

```
feat(spur-acp): SpurAgentCaps newtype + read accessors
```

### Task A.3: Failing test for explicit `ClientCapabilities` literal

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs` — add a unit test asserting that the constructed `InitializeRequest` carries an explicit `ClientCapabilities` literal with `meta["terminal_output"] == true`. Mirror the shape of existing tests for the request builder.

- [ ] **Step 1: Write test** that captures the request payload (mock the SDK transport) and asserts the JSON shape.
- [ ] **Step 2: Confirm fails** (today the request uses SDK defaults; `meta` is None).
- [ ] **Step 3: Commit red.**

### Task A.4: Implement explicit `ClientCapabilities` in `initialize`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs::initialize` (around line 269 per the v2 grounding).

- [ ] **Step 1: Build the literal** per spec §6.2 (Wave 0-verified shape):

```rust
use agent_client_protocol::{ClientCapabilities, FileSystemCapabilities};
let caps = ClientCapabilities::new()
    .fs(FileSystemCapabilities::new().read_text_file(true).write_text_file(true))
    .terminal(true)  // bool
    .meta(serde_json::json!({"terminal_output": true}));
let req = InitializeRequest::new(ProtocolVersion::LATEST).client_capabilities(caps);
```

- [ ] **Step 2: Tests pass** (including the existing initialize tests — confirm no regression).
- [ ] **Step 3: Commit green.**

```
feat(spur-acp): explicit ClientCapabilities literal at initialize
```

### Task A.5: Capture caps after `session/new` into the orchestrator session cache

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs` — after both `initialize` and `session/new` complete, build `SpurAgentCaps::new(&init_response, &new_session_response)` and stash `Arc<SpurAgentCaps>` keyed by `SessionId`.
- Modify: `crates/spur-core/src/orchestrator.rs` — add `spur_agent_caps(&self, sid: &SessionId) -> Option<Arc<SpurAgentCaps>>` getter parallel to `session_config_options`.
- New: `crates/spur-tui/tests/spur_agent_caps_event.rs` — integration test mirroring `tests/advertised_commands_event.rs` shape.

- [ ] **Step 1: Write integration test.** Compose a mock `NativeAcpConnection` that returns:
  - A stubbed `InitializeResponse` with known `agent_capabilities` (e.g. `load_session: true`, `meta: Some({"terminal_output": true})`).
  - A stubbed `NewSessionResponse` with codex-style modes/models/configOptions (load from existing fixture).

  Open a session, then assert the orchestrator's `spur_agent_caps()` returns a cache where `supports_set_mode/_model/_config_option` and `supports_load_session` and `meta_capability("terminal_output")` are all `true`.
- [ ] **Step 2: Confirm fails.**
- [ ] **Step 3: Implement.** Plumb both responses through native → orchestrator. Use `Arc<SpurAgentCaps>` for cheap clones. Cache is populated AFTER session/new completes (not after initialize alone — set_* gating needs the session-create payload).
- [ ] **Step 4: Tests pass.**
- [ ] **Step 5: Commit green.**

```
feat(spur-core): cache SpurAgentCaps on orchestrator session entry
```

### Task A.6: Wave A sweep + 3-gate review

- [ ] **Per-crate test sweep with 5-min timeouts:**

```sh
timeout 300 cargo test -p spur-acp --lib && \
timeout 300 cargo test -p spur-core --lib && \
timeout 300 cargo test -p spur-tui --lib && \
timeout 300 cargo fmt --all -- --check
```

- [ ] **Dispatch 3-gate review** (parallel codex/gemini/kimi) on `git diff <wave-A-base>..HEAD`. Same prompts as v2 plan A.4: codex for SDK API correctness, gemini for SPUR invariant preservation + capability semantics, kimi for fmt/clippy/Cargo hygiene.
- [ ] **Address findings**, commit cleanup as `style(...)` or `fix(...)`.

---

## Wave B — `set_session_model` with state-gated fallback (M8.B)

### Task B.1: Failing test for `set_session_model` state-gated dispatch

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs` — add tests covering 3 paths. Per spec §6.3, "state-gated" means dispatch is decided from `SpurAgentCaps` (which is derived from `NewSessionResponse`).

- [ ] **Step 1: Write tests:**
  - Caps where `models.is_some()` → dispatches `SetSessionModelRequest` (uses stable wire method `"session/set_model"`).
  - Caps where `models.is_none()` AND `config_options` non-empty → falls back to `set_session_config_option(ConfigId::model(), Value::ValueId(model_id))`.
  - Caps where neither → returns `Err(AcpError::CapabilityMissing("set_model"))`.
- [ ] **Step 2: Confirm fails.**
- [ ] **Step 3: Commit red.**

### Task B.2: Implement `set_session_model`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Implement** per spec §6.3. Wave 0 confirmed the SDK wire method is stable `"session/set_model"`; spur calls `client.set_session_model(SetSessionModelRequest)` and the SDK handles serialization.
- [ ] **Step 2: Tests pass.**
- [ ] **Step 3: Commit green.**

```
feat(spur-acp): set_session_model with capability-gated fallback
```

### Task B.3: AcpError::CapabilityMissing variant

**Files:**
- Modify: `crates/spur-acp/src/error.rs` (or wherever `AcpError` lives).

- [ ] **Step 1: Add variant** `CapabilityMissing(&'static str)` with display impl.
- [ ] **Step 2: Add unit test** for display formatting.
- [ ] **Step 3: Commit.**

### Task B.4: Submit-router routes /model to set_session_model when caps advertise

**Files:**
- Modify: `crates/spur-tui/src/commands/submit_router.rs` (or wherever `/model` dispatch is wired today via `set_config_option`).

- [ ] **Step 1: Write test** asserting that for a session whose caps advertise `set_model`, submitting `/model claude-sonnet-4-7` dispatches `set_session_model`, NOT `set_session_config_option`.
- [ ] **Step 2: Confirm fails.**
- [ ] **Step 3: Update dispatch** to consult capabilities.
- [ ] **Step 4: Tests pass; commit.**

---

## Wave C — Registry filter + UI gating (M8.C)

### Task C.1: Failing test for `available_commands_for_session`

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs` — add a test asserting that:
  - When caps lack `set_config_option` AND `set_model`, the synthesized `/model` command is filtered out.
  - When caps lack `set_mode`, the synthesized `/mode` command is filtered out.
  - All other commands (e.g. agent-advertised free-text-arg commands) pass through unchanged.

- [ ] **Step 1: Add test, confirm fails.**
- [ ] **Step 2: Commit red.**

### Task C.2: Implement registry filter

**Files:**
- Modify: `crates/spur-tui/src/commands/registry.rs`

- [ ] **Step 1: Add `available_commands_for_session(&self, sid: &SessionId, caps: &SpurAgentCaps) -> Vec<&CommandEntry>`** that filters by `dispatch` per spec §6.5.
- [ ] **Step 2: Tests pass.**
- [ ] **Step 3: Commit.**

### Task C.3: UI consumes the filter

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` — wherever `available_commands` is read for the slash-command popup.

- [ ] **Step 1: Refactor consumption** to call `available_commands_for_session(sid, &caps)`. Caps come from the orchestrator's `spur_agent_caps` getter (Task A.5).
- [ ] **Step 2: Add integration test** that opens a slash-command popup on a mock gemini-cli session (caps without `set_config_option`); assert `/model` is absent from the rendered list.
- [ ] **Step 3: Commit.**

```
feat(spur-tui): UI gates slash commands on advertised capabilities
```

### Task C.4: Greyed-out hint for capability-gated absent commands

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` — render a footer hint when the user types `/model` on a session that doesn't support it. (Note: alternative is to omit the command from the popup; spec §12 Q4 leaves this open.)

- [ ] **Step 1: Choose UX** per spec §12 Q4. Default: footer hint when typed; tooltip on hover. Document the choice.
- [ ] **Step 2: Implement and test.**
- [ ] **Step 3: Commit.**

---

## Wave D — `authenticate` `_meta` plumbing (M8.D, stretch)

### Task D.1: Extend `authenticate` signature

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs::authenticate`

- [ ] **Step 1: Add test** asserting that `authenticate(GATEWAY, Some(json!({"gateway":{"baseUrl":"x"}})))` produces a request with `request.meta == Some(json!({"gateway":{"baseUrl":"x"}}))`.
- [ ] **Step 2: Confirm fails.**
- [ ] **Step 3: Extend signature** per spec §6.4. Existing callers updated to pass `None`.
- [ ] **Step 4: Tests pass; commit.**

### Task D.2: Pipe through to UI auth path

**Files:**
- Modify: wherever spur-tui invokes `authenticate` today (find via grep).

- [ ] **Step 1: Identify the call site(s).**
- [ ] **Step 2: Pass `None` for now** (preserves existing behavior); document that gemini gateway / kimi terminal-auth flows will pass `Some(...)` in a follow-up auth-UX spec.
- [ ] **Step 3: Commit.**

---

## Wave E — `SessionInfoUpdate` arm (M8.E, bundle)

### Task E.1: Add `SessionInfoUpdate` arm to `apply_session_update`

**Files:**
- Modify: `crates/spur-tui/src/app.rs:2805-2833` — replace the catch-all swallow with an explicit `SessionInfoUpdate(payload) => { /* log + cache */ }` arm.

- [ ] **Step 1: Write integration test** mirroring `crates/spur-tui/tests/config_option_update_arm.rs` shape: inject a `SessionUpdate::SessionInfoUpdate(...)` notification (using the Wave 0 captured fixture) and assert the orchestrator's session-info cache reflects it.
- [ ] **Step 2: Confirm fails** (catch-all swallows it today).
- [ ] **Step 3: Add the arm.** If Wave 0's probe revealed that codex sets nontrivial fields (title, working_dir), cache them on the orchestrator session entry. Otherwise just log at TRACE so future protocol additions land here cleanly.
- [ ] **Step 4: Add `SessionInfoUpdate` to `session_update_variant_name` (`native.rs:1597-1608`)** so log lines aren't tagged `"other"`.
- [ ] **Step 5: Tests pass; commit green.**

```
feat(spur-tui): consume SessionInfoUpdate session-update notifications
```

---

## Wave F — Wave-wide sweep + 3-gate review

- [ ] **Per-crate test sweep with 5-min timeouts** (same as A.6).
- [ ] **Dispatch 3-gate review** parallel codex/gemini/kimi on `git diff <wave-0-base>..HEAD`.
- [ ] **Address findings.**
- [ ] **Manual smoke test:**
  - Open spur on a fresh codex session → verify `/model`, `/effort`, `/mode` all visible (codex advertises set_config_option).
  - Open spur on gemini-cli session → verify `/model` is absent or greyed-out (gemini lacks set_config_option).
  - Open spur on claude-code-acp session → verify `/model` dispatches `SetSessionModelRequest` not `set_session_config_option` (check trace log for the wire method name).

---

## Build sequence

Wave 0 → Wave A → (Wave B + Wave C in parallel) → (Wave D + Wave E in parallel) → Wave F.

Inside each wave, tasks are linear because each implementation step depends on its red-phase test compiling. Across waves, B and C are independent of each other (B is connection-layer; C is registry-layer); both depend on A. D and E are also mutually independent.

## Acceptance criteria

- [ ] `timeout 300 cargo test -p spur-acp` passes (existing + new SpurAgentCaps tests).
- [ ] `timeout 300 cargo test -p spur-core` passes.
- [ ] `timeout 300 cargo test -p spur-tui` passes (existing + new registry-filter, session-capabilities-event, session-info-update arm tests).
- [ ] `cargo fmt --all -- --check` returns 0 lines.
- [ ] `cargo clippy --workspace -- -D warnings` clean.
- [ ] Manual: codex session shows all 3 model/effort/mode pickers (caps advertise set_config_option).
- [ ] Manual: gemini-cli session does not surface `/model` or `/effort` (caps lack set_config_option).
- [ ] Manual: claude-code-acp session uses `set_session_model` for model switching (verify via wire log).
- [ ] Manual: spur's `initialize` request payload contains explicit `clientCapabilities.meta.terminal_output: true` (verify via wire log against codex; this gate is the prerequisite for M9's terminal recovery).

## Risk register

| # | Risk | Mitigation |
|---|---|---|
| R1 | Wave 0's SDK probe reveals `ClientCapabilities` cannot be field-by-field constructed (e.g. SDK requires an opaque builder method) | Use whatever the SDK exposes. Wave 0's job is to discover this; Wave A adapts. |
| R2 | `unstable_setSessionModel` and `set_session_model` are distinct method names that the SDK doesn't auto-resolve | Spur dispatches by capability inspection in `set_session_model` impl. Document in rustdoc. |
| R3 | A pre-M8 caller of `authenticate(method_id)` hits the new signature break | Wave D's Task D.2 finds and updates all call sites; pass `None` to preserve behavior. |
| R4 | Greyed-out vs hidden command UX choice surprises users | Spec §12 Q4 leaves this open; Task C.4 documents the choice. Iterate post-merge based on feedback. |
| R5 | Capability cache races with session/new completion | UI renders disabled state for ~100ms while session/new awaits; not an error. Documented in spec §8 C4. |
| R6 | Codex `SessionInfoUpdate` payload turns out to be empty / uninformative | Wave E still adds the explicit arm to remove the silent drop; cache fields opportunistically. Worst case: arm logs at TRACE only. |
| R7 | Test-vs-prod drift on capability inspection — spur reads `raw.foo` but agent emits different field name | Wave 0's probe covers the canonical agents (codex, claude-code-acp, gemini). Add fixtures for kimi-cli + opencode in M8.A; re-test on each agent before merge. |

---

## Appendix — Out-of-scope follow-ups (referenced for context)

- **M9 plan:** `2026-04-27-spur-acp-adapter-fill-m9.md` (to be written after M8.A merges).
- **M10 plan:** `2026-04-27-spur-acp-typed-arg-pickers-m10.md` (to be written after M9 merges + codex-acp Track-1 PR is merged upstream).
- **Auth UX spec:** capture gemini gateway / kimi terminal-auth dialog work in a separate spec; M8.D ships the wire plumbing only.
- **Wire fixtures for kimi-cli + opencode:** capture during M8.A's integration tests so M9 has ground truth to extract against.
