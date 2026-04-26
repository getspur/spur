# Codex model & reasoning-effort slash pickers

**Status:** design approved (brainstorm), pending implementation plan
**Date:** 2026-04-27
**Owners:** Kevin Truong (kevin.truong.ds@gmail.com)
**Scope:** Add interactive `/model` and `/effort` slash commands that switch the codex-acp session's active model and reasoning effort mid-session, using a fuzzy-search arg picker modeled on the existing `@`-mention popup.

---

## 1. Goal

Codex-acp exposes mid-session model and reasoning-effort switching via the standard ACP `session/set_config_option` RPC, returning a list of available choices in `NewSessionResponse.config_options`. Spur currently drops that payload on the floor and has no slash command for either knob. This spec adds the smallest end-to-end path that:

1. captures the advertised choices per session,
2. surfaces them as `/model` and `/effort` slash commands with fuzzy-searchable arg pickers, and
3. round-trips the user's selection through the typed ACP RPC, refreshing the cached choices from the response.

The design is **vendor-neutral** at every layer: the orchestrator caches raw `Vec<SessionConfigOption>`; the synthesizer in `spur-acp` filters by an allow-list of `config_id` values (`model`, `reasoning_effort`); the TUI registers the resulting commands through a generic `Advertised` source. Any future ACP agent that returns a `select`-shaped option for one of the allow-listed `config_id`s automatically gets the same picker UX.

## 2. Non-goals

- Persisting model / effort defaults across sessions. (Use codex's `~/.codex/config.toml` or `-c model=…` at spawn time.)
- Exposing codex's `mode` (Approval Preset) selector through `/mode`. The existing spur-local `/mode` is Claude plan-mode and would collide; out of scope for v1.
- Auto-deriving slash commands from non-`select` config options (booleans, free-text). Future work.
- A real-time picker that re-fetches choices while open. Snapshot at picker-open time is sufficient.
- Extending support to live codex-acp builds in CI. Mocks cover the wire.

## 3. Background

### 3.1 codex-acp surface (verified)

Reviewed at <https://github.com/zed-industries/codex-acp> HEAD (codex-acp v0.12.0, pinned against codex-rs `rust-v0.124.0`):

- `NewSessionResponse.config_options: Vec<SessionConfigOption>` — codex returns `mode`, `model`, and (when applicable) `reasoning_effort` as `select` shapes with `current_value` and a list of `(id, name, description?)` choices. (`thread.rs:2837-2929`)
- `session/set_config_option` mutates the live session via `Op::OverrideTurnContext { model, effort }` and returns the updated `Vec<SessionConfigOption>`. (`thread.rs:2951-2964` typed handler; `:2967-3019` underlying mutation)
- `session/set_model` accepts a compact `"<preset>/<effort>"` ID. We expose this on the trait for completeness but route v1 user-facing flows through `set_config_option` so model and effort stay orthogonal.

### 3.2 Spur surface (verified)

- `agent-client-protocol = "0.10.4"` in workspace Cargo.toml; current feature set is `["unstable_session_usage"]`. Both `SetSessionModelRequest` (gated by `unstable_session_model`) and `SetSessionConfigOptionRequest` (no extra gate beyond `unstable`) exist in 0.10.4.
- `spur-acp/src/connection/mod.rs` defines `AgentConnection`. `set_session_mode` is wired end-to-end via `AcpCommand::SetSessionMode` (`native.rs:112`, `:1007`). No equivalent exists for model or config-option RPCs.
- `spur-core/src/orchestrator.rs` `create_brain_session` (~L2966) reads only `.session_id` from `NewSessionResponse`; `models` and `config_options` are discarded.
- `spur-tui/src/commands/spur_local.rs` taxonomy doc-comment classifies `/model` as a *conversational* command expected to be agent-advertised. Codex-acp does not emit `_<vendor>.dev/commands/available` advertisements for it.
- `spur-tui/src/components/picker_shell.rs` already drives a generic `PickerShell` over `Box<dyn QuerySource>`. `SlashQuerySource` is the precedent for static-list fuzzy pickers using `nucleo_matcher`. Reusable as-is for a model/effort picker.

## 4. Design decisions (resolved during brainstorm)

| # | Question | Choice | Rationale |
|---|---|---|---|
| Q1 | Where do `/model` and `/effort` live in the slash-command taxonomy? | **Synthesizer pattern** (B). Generated from `NewSessionResponse.config_options`, registered as `CommandSource::Advertised`. | Stays inside the existing meta-vs-conversational taxonomy; auto-lights-up for any future agent returning matching config options. |
| Q2 | Where does the synthesizer live? | **`spur-acp/src/adapter/config_options.rs`** (A). | Same crate as other adapter modules; pure mapping function, fully testable in isolation. Requires a vendor-neutral `AdvertisedCommand` type so spur-tui doesn't import ACP schema types. |
| Q3 | Picker UX shape? | **Fuzzy-search picker** (B), modelled on the `@`-mention popup. | Discoverable; mirrors existing UX users already know. |
| Q4 | Which select options become slash commands? | **Allow-list `{model, reasoning_effort}`** (A). | Avoids silent collision with spur-local `/mode`; explicit about scope; vendor-neutral by `config_id`. |
| Q5 | How does the arg picker get triggered? | **Extend `TriggerDetector` with `SlashArg` kind** (α). | Buffer is the source of truth; same state for typed / pasted / cursor-motion / vim / picker-completed input; scales to multi-arg commands. Mitigation: TDD the state machine first to neutralize regression risk on existing `@` and `/` triggers. |

The Q5 decision was further validated via a multi-round MCTS-style rollout simulation across 8 user journeys and 4 future-pressure scenarios, with α dominating on 6 of 8 journeys and all 4 future-pressure scenarios. The deciding meta-principle: **prefer state-machine designs over event-handler designs when the state is observable from the buffer.**

## 5. Architecture

### 5.1 Data flow

```
codex-acp                              spur                              user
─────────                              ────                              ────
session/new ──────────────────────────►│ orchestrator: create_brain_session
                                       │   stores raw Vec<SessionConfigOption>
NewSessionResponse{config_options} ◄───│   on the session record
                                       │
                                       │ spur-acp::adapter::config_options
                                       │   synthesize(&config_options)
                                       │   → Vec<AdvertisedCommand>
                                       │   filtered to allow-list
                                       │
                                       │ CommandRegistry::ensure_cache
                                       │   merges SpurLocalSource +
                                       │   AgentAdvertised + AdvertisedSource
                                       │
                                       │ User types `/m` ──────────────────► type
                                       │   slash popup filters → "model"
                                       │   user picks → buffer = "/model "  pick
                                       │
                                       │ TriggerDetector::step
                                       │   ^/model\s+ matched
                                       │   → SlashArg{cmd:"model"}
                                       │ InputCompletionPort opens
                                       │   PickerShell with
                                       │   ConfigOptionQuerySource
                                       │
                                       │ User types "gpt" ─────────────────► fuzzy
                                       │   nucleo filters; user picks       pick
                                       │   "gpt-5-codex"
                                       │
                                       │ Submit
                                       │ AgentConnection::
                                       │   set_session_config_option
                                       │   {config_id:"model", value:"gpt-5-codex"}
session/set_config_option ◄────────────┤
SetSessionConfigOptionResponse ───────►│ updated config_options replace cache
   {config_options:[...]}              │ → registry rebuilds
                                       │ → next /model picker reflects new
                                       │   "current_value"
```

### 5.2 Touchpoints (5 layers)

1. **`Cargo.toml`** — add `unstable_session_model` to workspace `agent-client-protocol` features.
2. **`crates/spur-acp/src/connection/mod.rs` + `native.rs`** — extend `AgentConnection` trait with `set_session_model` and `set_session_config_option`; add `AcpCommand::SetSessionModel` and `AcpCommand::SetSessionConfigOption` channel variants; mirror the `SetSessionMode` plumbing pattern.
3. **`crates/spur-acp/src/adapter/config_options.rs`** *(new)* — pure synthesizer. Defines vendor-neutral `AdvertisedCommand` and `AdvertisedChoice` types and the allow-list constant.
4. **`crates/spur-core/src/orchestrator.rs`** — capture `NewSessionResponse.config_options` onto the session record around L2966; refresh from every `SetSessionConfigOptionResponse`. Expose `session_config_options(session_id)` and `replace_session_config_options(session_id, opts)` getters/setters. Vendor-neutral; orchestrator never calls `synthesize`.
5. **`crates/spur-tui/`** — extend `Dispatch` and `CommandSource` enums; add `commands/advertised.rs`; extend `TriggerKind` with `SlashArg{command, config_id}`; extend `CommandRegistry::arg_picker(name)`; add `components/config_option_query_source.rs`.

### 5.3 Boundary discipline

| Crate | Sees ACP schema types? |
|---|---|
| `spur-acp` | Yes — wire layer. Owns `AdvertisedCommand`/`AdvertisedChoice` boundary type. |
| `spur-core` | Yes — caches raw `Vec<SessionConfigOption>` because orchestrator talks to ACP. |
| `spur-tui` | **No.** Only `AdvertisedCommand`/`AdvertisedChoice`. |

## 6. Types & signatures

### 6.1 `spur-acp` connection trait additions

```rust
// crates/spur-acp/src/connection/mod.rs
#[async_trait::async_trait]
pub trait AgentConnection: Send + Sync {
    // ...existing methods...

    async fn set_session_model(
        &self,
        session_id: &SessionId,
        model_id: ModelId,
    ) -> Result<SetSessionModelResponse, AcpError>;

    async fn set_session_config_option(
        &self,
        session_id: &SessionId,
        config_id: SessionConfigId,
        value: SessionConfigOptionValue,
    ) -> Result<Vec<SessionConfigOption>, AcpError>;
}
```

`set_session_model` is wired but no v1 `Dispatch` variant routes user input to it. Reserved for future use.

### 6.2 `spur-acp` synthesizer module

```rust
// crates/spur-acp/src/adapter/config_options.rs (NEW)

#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedCommand {
    pub name: String,             // slash name without leading "/"
    pub description: String,
    pub hint: Option<String>,
    pub config_id: String,        // ACP wire id, may differ from `name`
    pub choices: Vec<AdvertisedChoice>,
    pub current_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdvertisedChoice {
    pub value: String,            // wire SessionConfigOptionValue
    pub label: String,            // display in picker
    pub description: Option<String>,
}

const ALLOW_LIST: &[(&str, &str, &str)] = &[
    ("model",            "model",  "Switch model for this session"),
    ("reasoning_effort", "effort", "Switch reasoning / thinking effort"),
];

pub fn synthesize(options: &[SessionConfigOption]) -> Vec<AdvertisedCommand>;
```

### 6.3 `spur-core` session record cache

```rust
pub struct BrainSessionRecord {
    // ...existing fields...
    pub config_options: Vec<SessionConfigOption>,
}

// facade additions
fn session_config_options(&self, session_id: &SessionId) -> Vec<SessionConfigOption>;
fn replace_session_config_options(&self, session_id: &SessionId, opts: Vec<SessionConfigOption>);
```

`replace_session_config_options` is responsible for emitting a registry-refresh signal on whichever notification channel `spur-tui` already uses to invalidate its `CommandRegistry::ensure_cache`. The plan resolves the exact wire (existing `SessionEvent` variant vs. a new one).

### 6.4 `spur-tui` registry, trigger, and query source

```rust
// crates/spur-tui/src/commands/entry.rs
pub enum Dispatch {
    SpurLocal(Action),
    PromptText { /* unchanged */ },
    VendorExec { /* unchanged */ },
    SetSessionConfigOption { config_id: String },   // NEW
}

pub enum CommandSource {
    Spur,
    Agent,
    Advertised,                                     // NEW
}

// crates/spur-tui/src/commands/advertised.rs (NEW)
pub struct AdvertisedSource;
impl AdvertisedSource {
    pub fn entries(opts: &[SessionConfigOption]) -> Vec<CommandEntry>;
}

// crates/spur-tui/src/components/completion_trigger.rs
pub enum TriggerKind {
    Mention,
    Slash,
    SlashArg { command: String, config_id: String }, // NEW
}

// crates/spur-tui/src/commands/registry.rs
pub enum ArgPickerKind {
    ConfigOption { config_id: String },
}
impl CommandRegistry {
    pub fn arg_picker(&self, command_name: &str) -> Option<ArgPickerKind>;
}

// crates/spur-tui/src/components/config_option_query_source.rs (NEW)
pub struct ConfigOptionQuerySource {
    pub command: String,           // slash name, e.g. "model"
    pub config_id: String,
    pub choices: Vec<AdvertisedChoice>,
}
impl QuerySource for ConfigOptionQuerySource { /* nucleo filter; ReplaceTriggerToken */ }
```

### 6.5 Naming convention

We rename `reasoning_effort` → `effort` at the slash surface only. The `AdvertisedCommand.config_id` field carries the wire name (`reasoning_effort`); `name` carries the slash name (`effort`). The `ALLOW_LIST` is the single source of truth for the mapping.

## 7. Wire path (happy path, `/model gpt-5-codex`)

```
1. PickerShell::handle_key
   → Accept(ReplaceTriggerToken{ replacement: "/model gpt-5-codex" })
   → buffer = "/model gpt-5-codex", picker closes

2. User Enter → submit_router::route
   → Dispatch::SetSessionConfigOption { config_id: "model" }
   → SubmitDecision::SetSessionConfigOption { config_id: "model", value: "gpt-5-codex" }

3. app.rs → Action::SetSessionConfigOption { config_id, value }
   → UserInput::SetSessionConfigOption on input channel

4. spur-cli main.rs → InteractiveInput::SetSessionConfigOption {
       session_id, config_id, value,
   }

5. orchestrator handler:
   let updated = brain.connection.set_session_config_option(
       &session_id,
       SessionConfigId::new(config_id),
       SessionConfigOptionValue::new(value),
   ).await?;
   self.replace_session_config_options(&session_id, updated);
   self.notify_command_registry_dirty(&session_id);

6. NativeAcpConnection::set_session_config_option
   sends AcpCommand::SetSessionConfigOption to ACP thread; awaits oneshot.

7. ACP thread:
   let resp = connection.set_session_config_option(req).await?;
   tx.send(Ok(resp.config_options));

8. codex-acp returns updated config_options → cache replaced → next picker fresh.
```

## 8. Error handling

| Class | Trigger | Catch site | User feedback |
|---|---|---|---|
| **E1** Agent has no `config_options` | `synthesize()` returns empty | `AdvertisedSource::entries` returns empty | Silent absence — no `/model` in popup. Correct UX. |
| **E2** Command typed but unknown to current session | `CommandRegistry::arg_picker(name)` returns `None` | `TriggerDetector::step` does not transition to `SlashArg` | Falls back to plain text submit; agent rejects. |
| **E3** Arg value not in cached choice list | User pastes `/model bogus` and submits | `submit_router` validates against cache | Toast: `unknown model 'bogus'. options: gpt-5-codex, gpt-5, o4-mini`. Never crosses wire. |
| **E4** ACP RPC fails | `set_session_config_option` returns `AcpError` | Orchestrator's existing toast path (mirrors SetSessionMode) | `Failed to set <config_id>: <error>`. Cache **not** updated. |
| **E5** SDK feature flag missing | `unstable_session_model` not enabled | `cargo check` (compile error on import) | Caught by CI before merge. |

E3 is **client-side** validation against the cached choice list. We do not let invalid values reach codex; our error message is more actionable than the agent's.

## 9. Concurrency & cache freshness

- **Single source of truth:** the orchestrator's per-session `Vec<SessionConfigOption>`. `synthesize` is invoked on read by `AdvertisedSource::entries`.
- **Snapshot semantics:** `ConfigOptionQuerySource` copies the choice list at picker-open; the snapshot is immutable for the lifetime of one picker session.
- **Cache invalidation:** every successful `set_session_config_option` round-trip replaces the cache from the response; a registry-dirty notification refreshes the slash popup's `current` hint on next open.
- **Race window:** between user pick and RPC return, the picker is closed. Reopen-before-response shows the prior `current_value` — acceptable, self-correcting.
- **Cancellation:** in-flight `set_session_config_option` is **not** cancelled by `session/cancel` (it's a session-config write, not a turn). Agent shutdown mid-call surfaces as E4.

## 10. Testing strategy

TDD discipline: tests before implementation everywhere; **`TriggerDetector` tests written first** as the α-mitigation.

### 10.1 Test pyramid

| Layer | Crate | Style | Proves |
|---|---|---|---|
| Synthesizer unit | `spur-acp` | Table-driven | Allow-list filter, choice ordering, `current_value` extraction. |
| `TriggerDetector` unit | `spur-tui` | Snapshot + table-driven | `SlashArg` transitions; **no regression** on `Mention`/`Slash`. |
| `submit_router` validator | `spur-tui` | Table-driven | E3 rejection format. |
| Connection mock | `spur-acp` | Mock `AgentConnection` | RPC method names + payload shapes. |
| Integration | `spur-core` | Mock-codex over stdio | Cache populates from `NewSessionResponse`; refreshes on response. |
| End-to-end smoke | `spur-tui` | Headless TUI driver / `insta` | Type → pick → submit → RPC → cache update. |

### 10.2 `TriggerDetector` test cases (must exist before implementation)

```text
T1.  ""              + type '@'                → Open(Mention, prefix=1)
T2.  ""              + type '/'                → Open(Slash,   prefix=1)
T3.  "hello "        + type '@'                → Open(Mention, prefix=7)
T4.  "x"             + type '@'                → None        // not at boundary
T5.  "/help"         + type ' '                → Close       // slash trigger ends
T6.  "/model "       + cursor at end           → Open(SlashArg{cmd:"model"}, prefix=7)
T7.  "/model gpt"    + cursor at end           → Update(SlashArg, query="gpt")
T8.  "/effort high"  + cursor at end           → Update(SlashArg, query="high")
T9.  "/model "       + paste "gpt"             → Open(SlashArg, query="gpt")
T10. "/model gpt"    + arrow-back to col 8     → Update(SlashArg, query="g")
T11. "/model gpt"    + backspace to "/model"   → Close
T12. "/unknown foo"  + cursor at end           → None        // not in arg_picker registry
T13. "/model gpt x"  + cursor at end           → None        // multi-word, no second-arg picker yet
T14. " /model "      + cursor at end           → None        // not at column 0
T15. "/Model "       + cursor at end           → None        // command lookup is case-sensitive
```

T15 fixes the case-sensitivity question: command-name lookup is exact (matches existing `SlashQuerySource`); fuzzy casing applies only to the arg query inside `nucleo`.

### 10.3 Synthesizer test cases

```text
S1. Empty input                                    → empty output.
S2. One allow-listed select with 3 choices         → 1 AdvertisedCommand, 3 choices, ordered.
S3. Allow-listed but type ≠ Select (e.g. boolean)  → omitted.
S4. Allow-listed select with 0 choices             → omitted (degenerate).
S5. Non-allow-listed config_id (e.g. "mode")       → omitted.
S6. Multiple allow-listed options                  → returned in allow-list order.
S7. current_value populated correctly.
S8. Hint format when current_value is Some         → `current: <value>`.
```

### 10.4 Out of test scope

- Live codex-acp in CI (mock-codex covers wire).
- `nucleo` correctness (third-party, well-tested).
- Full TUI pixel matrix for the picker (one snapshot to confirm wiring is enough).

### 10.5 Verification commands (preview; lives in plan)

```sh
cargo test -p spur-acp adapter::config_options
cargo test -p spur-tui completion_trigger
cargo test -p spur-tui submit_router
cargo test -p spur-core orchestrator::tests::session_config_options
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## 11. Open questions

None at design close. All Q1–Q5 forks resolved during brainstorm; T15 case-sensitivity resolved in §10.2.

## 12. Future work (explicitly deferred)

- `/mode` slash command for codex's Approval Preset selector (collides with spur-local `/mode`; needs a namespacing decision).
- Auto-derive slash commands from non-`select` config options (booleans → toggle commands; free-text → typed-arg pickers without choices).
- Multi-arg commands (`SlashArg { command, arg_index }`); the chosen state-machine design (α) extends naturally.
- `set_session_model` user-facing path (compact `"<preset>/<effort>"` form). Trait method already exposed.
- Inline arg validation (red-underline on invalid value mid-typing).
- Persisting model / effort defaults across sessions.
