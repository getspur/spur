# ACP-first arg pickers — v2 design (replaces prior v2-delta draft)

**Status:** design approved (brainstorm), pending implementation plan
**Date:** 2026-04-27
**Owners:** Kevin Truong (kevin.truong.ds@gmail.com)
**Relationship to v1:** This spec extends `2026-04-27-codex-model-effort-slash-pickers-design.md` (v1). v1 ships `/model` and `/effort` for codex's `config_options` knobs (vendor-neutral by `config_id`). v2 generalises: spur consumes the standard ACP `AvailableCommand.input` field already on the wire, and renders an arg picker for any agent-advertised slash command that needs one — **with zero per-command, per-agent code in spur**.

**Supersedes:** an earlier v2 draft (`2026-04-27-codex-slash-commands-v2-delta-design.md`, deleted) that proposed a hardcoded `ARG_PICKER_BINDINGS` table in spur-tui. That design violated the control-plane axiom (vendor knowledge in mechanism code). This spec is the L9 staff-engineer revamp arrived at after a multi-round MCTS evaluation.

---

## 1. Goal

Treat **ACP itself as the single source of truth** for which agent commands take args and what kind. Spur's role as an AI-agent control plane provides *mechanism* (a generic arg-picker engine) and the agent provides *policy* (which commands take args, what hint, what typed kind) via standard ACP messages. Each new agent or new command costs **zero spur code changes** — the binding table on the wire IS the binding table.

Concrete user-visible features:

1. **Cache freshness** — spur consumes `session/update.ConfigOptionUpdate` so v1's model/effort current-value reflects external changes.
2. **Free-text arg picker** — every agent-advertised slash command whose `AvailableCommand.input == Some(Unstructured(...))` opens a free-text picker with the advertised hint as placeholder. Works for codex's `/review`, `/review-branch`, `/review-commit`, and any future agent's `Unstructured` arg commands.
3. **Typed arg picker** — when `AvailableCommand.meta._<vendor>.dev.arg_picker_hint` declares a typed kind (e.g. `git_ref`), spur instantiates the matching typed picker. Bridge mechanism until ACP adds typed enum variants to `AvailableCommandInput`.
4. **No-arg commands** — already work today via the existing `AvailableCommandsUpdate` plumbing (kimi worker confirmed end-to-end). v2 doesn't touch this path.

## 2. Non-goals

- Hardcoding any per-command or per-agent table in spur source. The control-plane axiom requires per-vendor knowledge to live at the agent edge or in protocol messages, never in spur code.
- Per-agent TOML config files for arg-picker bindings. The advertised `AvailableCommand.input` and `_meta` fields are the configuration.
- Surfacing the 5 `Op::OverrideTurnContext` knobs codex-acp doesn't expose via `config_options` (`personality`, `service_tier`, `summary`, `permission_profile`, `approvals_reviewer`). Tracked as upstream codex-acp gap, separate issue.
- Surfacing the 4 action-oriented `Op::*` variants codex-acp doesn't expose (`RunUserShellCommand`, `DropMemories`, `UpdateMemories`, `RefreshMcpServers`). Tracked as separate upstream gaps; gemini worker §11.1.
- `/mode` for codex's Approval Preset (collides with spur-local `/mode` Claude plan-mode; namespacing decision deferred).
- Real git-ref browser with branch metadata, ahead/behind, commit graph. v2 picker shows refs and short shas only.

## 3. Background

### 3.1 ACP already advertises arg requirements (verified locally)

`agent-client-protocol-schema-0.11.4/src/client.rs:438-528` defines on the **stable** schema:

```rust
pub struct AvailableCommand {
    pub name: String,
    pub description: String,
    pub input: Option<AvailableCommandInput>,
    pub meta: Option<Meta>,        // ACP _meta extensibility
}

#[non_exhaustive]
pub enum AvailableCommandInput {
    Unstructured(UnstructuredCommandInput),    // free-text with hint
}

pub struct UnstructuredCommandInput {
    pub hint: String,                          // placeholder for the picker
    pub meta: Option<Meta>,
}
```

Two protocol-level facts:

- **`AvailableCommand.input` is the canonical arg signal.** `Some(_)` ⇒ arg required; `None` ⇒ no-arg command. Spur reads this directly; no inference needed.
- **`AvailableCommandInput` is `#[non_exhaustive]`.** ACP can add typed variants (`GitRef`, `FilePath`, `Choice`) without breaking changes. Until that happens, the `_meta` extensibility pattern (also stable) carries advisory typed hints.

### 3.2 codex-acp already populates `input` (verified)

`codex-acp/src/thread.rs:2735-2767`:

```rust
AvailableCommand::new("review", "...").input(Unstructured(UnstructuredCommandInput::new(
    "optional custom review instructions"
))),
AvailableCommand::new("review-branch", "...").input(Unstructured(UnstructuredCommandInput::new(
    "branch name"
))),
AvailableCommand::new("review-commit", "...").input(Unstructured(UnstructuredCommandInput::new(
    "commit sha"
))),
AvailableCommand::new("init",    "..."),    // no input
AvailableCommand::new("compact", "..."),    // no input
AvailableCommand::new("undo",    "..."),    // no input
AvailableCommand::new("logout",  "..."),    // no input
```

Spur has all the data to drive arg-pickers correctly today **without writing a single per-command line of code**. The only missing piece is typed-kind information for `branch name` / `commit sha` (so spur can spawn a git-ref picker instead of a free-text box).

### 3.3 Spur surface (verified by kimi worker)

`AvailableCommandsUpdate` is consumed end-to-end:
`native.rs:1103-1137` (broadcast) → `notification_pump.rs:30-54` → `app::apply_session_update` (`app.rs:2626-2637`) → `SessionDetailView::apply_available_commands` (`session_detail.rs:619-628`) → `CommandRegistry::set_agent_commands` with `CommandSource::Agent`.

So the `Vec<AvailableCommand>` payload (including `input` and `meta`) **already reaches the registry**. Today the registry only uses `name` and `description`; `input` and `meta` are dropped. v2's mechanical change is "stop dropping them."

## 4. Design decisions (resolved during brainstorm)

| # | Question | Choice |
|---|---|---|
| Q1 | How does spur know which commands take args? | Read `AvailableCommand.input` from the cached `Vec<AvailableCommand>` — already on the wire, already cached in `CommandRegistry`. Zero hardcoded table. |
| Q2 | How does spur get typed-picker hints (e.g. git ref) until ACP enum extends? | `_meta._<vendor>.dev.arg_picker_hint` extension on `AvailableCommand`. Spur reads as advisory; falls through to `Unstructured` when absent. Vendor-namespaced + `_hint` suffix marks it as transitional; deletes when ACP adds typed variants. |
| Q3 | Two arg-source channels (`config_options` synthetic + `AvailableCommand.input`) — unify? | Keep separate `QuerySource` impls (`ConfigOptionQuerySource` from v1 + new `CommandInputQuerySource`). The `QuerySource` trait IS the unification point; no new abstraction needed. |
| Q4 | Typed-kind taxonomy ship at v2? | `FreeText` (default for `Unstructured` with no `_meta` hint) + `GitRef { Branch \| Commit }`. Smallest closed set covering codex's needs. Unknown `_meta` kinds → graceful `FreeText` fallback. |
| Q5 | Where do `_meta` parsing helpers live? | `crates/spur-acp/src/adapter/arg_picker_hint.rs` — pure parser, vendor-neutral, alongside v1's `config_options.rs`. spur-tui receives a typed `ArgPickerHint` enum. |
| Q6 | What about `ConfigOptionUpdate` notification? | New arm in `app::apply_session_update` (`app.rs:2626`) — same shape as the existing `AvailableCommandsUpdate` arm. Refreshes v1's cache. |

The Q1-Q6 decisions follow from one meta-principle: **the protocol message IS the binding table**.

## 5. Architecture

### 5.1 Data flow

```
agent (codex-acp)              spur                            ACP wire
─────────────────              ────                            ────────

emits AvailableCommand{        ConsumesAvailableCommandsUpdate session/update.
  name: "review-branch",       (existing path, unchanged)      AvailableCommands
  input:Some(Unstructured{                                     Update
    hint:"branch name"}),      → CommandRegistry receives full
  meta:Some({                    Vec<AvailableCommand> incl.
    "_codex.dev.arg_picker       input + meta
     _hint":{kind:"git_ref",   → cmd registered as Agent source
              git_ref:"branch"}})

User types `/r`──────────────► slash popup filters → "review-branch"
User picks /review-branch───► buffer = "/review-branch "       

TriggerDetector::step          
  matches ^/review-branch\s+   
  → calls registry.arg_picker("review-branch")
  → registry returns hint:
       {free_text_hint: "branch name",
        typed_hint: GitRef{Branch}}     (parsed from cmd.input + cmd.meta)
  → SlashArg state opens

InputCompletionPort
  → instantiates QuerySource:
      typed_hint = GitRef → GitRefQuerySource{Branch, cwd}
      else                → CommandInputQuerySource{free_text_hint}

PickerShell opens
  user fuzzy-picks "main"
  accept → ReplaceTriggerToken("/review-branch main")

User Enter ────────────────►   Dispatch::PromptText (existing)
                              → AgentConnection::prompt
session/prompt ◄──────────────  ContentBlock::Text("/review-branch main")
agent parses & dispatches Op::Review{ReviewTarget::BaseBranch("main")}


────────── parallel: cache-freshness for v1's config_options ──────────

emits session/update.            consumed by existing               session/update.
ConfigOptionUpdate{              AgentNotification pump             ConfigOption
  config_options: [...]}                                            Update
                              → app::apply_session_update
                                NEW match arm:
                                  → orchestrator::
                                    replace_session_config_options
                                  → registry rebuilds; next
                                    /model picker shows new current
```

### 5.2 Touchpoints (4 layers; smallest possible diff)

1. **`crates/spur-acp/src/adapter/arg_picker_hint.rs`** *(new)* — pure parser. `parse(&AvailableCommand) -> ArgPickerSpec`. Reads `cmd.input` (presence + free-text hint); reads `cmd.meta._<vendor>.dev.arg_picker_hint` (typed kind). Returns vendor-neutral `ArgPickerSpec`. Stateless, fully unit-testable.

2. **`crates/spur-tui/src/commands/registry.rs`** — extend `CommandRegistry` to remember the parsed `ArgPickerSpec` per command (alongside `name` and `description`). New method `arg_picker_spec(name) -> Option<ArgPickerSpec>` consulted by `TriggerDetector`. **No `ARG_PICKER_BINDINGS` table; no per-command branches.**

3. **`crates/spur-tui/src/components/`** — two new query sources:
   - `command_input_query_source.rs` — degenerate free-text picker; reads `ArgPickerSpec.free_text_hint` for placeholder. Replaces v2-original's `FreeTextQuerySource` (same idea, different storage origin).
   - `git_ref_query_source.rs` — async git-ref picker. Spawns `git for-each-ref` / `git log --oneline` on open. Instantiated only when `ArgPickerSpec.typed_hint = Some(GitRef{...})`.

4. **`crates/spur-tui/src/app.rs`** — add `ConfigOptionUpdate` match arm in `apply_session_update` around line 2626. Calls `orchestrator::replace_session_config_options`.

### 5.3 What v2 does NOT touch

| Layer | Reason |
|---|---|
| `Cargo.toml` | No new deps. `git` is a host process. |
| `crates/spur-acp/src/connection/` | No new RPCs. Free-text and typed picker results both dispatch via the existing `prompt` RPC as `Dispatch::PromptText`. |
| `crates/spur-core/src/orchestrator.rs` | v1's `replace_session_config_options` setter is reused for the new `ConfigOptionUpdate` arm. No new methods. |
| `crates/spur-tui/src/commands/spur_local.rs` | Spur-local meta commands unchanged. |
| **Per-agent config files (e.g. `.spur/agents/*.toml`)** | **DO NOT CREATE.** ACP advertisement IS the configuration. |

## 6. Types & signatures

### 6.1 `spur-acp` parser

```rust
// crates/spur-acp/src/adapter/arg_picker_hint.rs (NEW)

/// Vendor-neutral description of an arg picker for a single agent-advertised
/// command. Parsed from ACP's `AvailableCommand.input` (presence + free-text
/// hint) and the optional `_meta._<vendor>.dev.arg_picker_hint` extension
/// (typed kind). Consumed by spur-tui without ACP-schema imports.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgPickerSpec {
    /// Hint string from `Unstructured.hint`. Used as the picker's placeholder
    /// regardless of typed kind.
    pub free_text_hint: String,
    /// Typed kind from `_meta._<vendor>.dev.arg_picker_hint.kind`, if any.
    /// `None` ⇒ free-text picker. Unknown kinds ⇒ `None` (graceful).
    pub typed_hint: Option<ArgPickerHint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgPickerHint {
    /// Picker spawns `git for-each-ref` (Branch) or `git log --oneline` (Commit).
    GitRef { kind: GitRefKind },
    // Future variants land here as ACP extension namespaces grow:
    // FilePath, Choice { values }, Url, Email, etc.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRefKind { Branch, Commit }

/// Parse from an `AvailableCommand`. Returns `None` if `cmd.input.is_none()`
/// (i.e. no-arg command).
pub fn parse(cmd: &AvailableCommand) -> Option<ArgPickerSpec>;
```

The parser walks `cmd.meta` looking for any key matching `_<vendor>.dev.arg_picker_hint` (vendor-neutral wildcard) and decodes the typed kind. Multiple vendor namespaces in the same `_meta` (extremely unlikely) take the first parseable one with a deterministic tie-breaker (lex order). Unknown kinds are silently ignored.

### 6.2 `spur-tui` registry extension

```rust
// crates/spur-tui/src/commands/registry.rs

impl CommandRegistry {
    /// On every `set_agent_commands` call, run `spur_acp::adapter::
    /// arg_picker_hint::parse(&cmd)` for each command and store the
    /// resulting Option<ArgPickerSpec> on the registry entry.
    pub fn set_agent_commands(&mut self, handle: String, cmds: Vec<AvailableCommand>) { ... }

    /// Lookup used by TriggerDetector when the buffer matches `^/<cmd>\s+`.
    /// Returns the parsed spec for the currently-active session's command.
    pub fn arg_picker_spec(&self, command_name: &str) -> Option<ArgPickerSpec>;
}
```

`ArgPickerSpec` is the *only* type spur-tui needs from spur-acp for picker decisions. No `AvailableCommand`, no `Meta`, no ACP schema types in spur-tui.

### 6.3 `spur-tui` query sources

```rust
// crates/spur-tui/src/components/command_input_query_source.rs (NEW)

pub struct CommandInputQuerySource {
    pub command: String,
    pub free_text_hint: String,
    last_query: String,
}

impl QuerySource for CommandInputQuerySource {
    fn title(&self) -> &str { &self.free_text_hint }
    fn query_mode(&self) -> QueryMode { QueryMode::Static }
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        self.last_query = query.to_owned();
        vec![RetrievalRow::synthetic_submit(query)]   // 1 confirmation row
    }
    fn accept(&self, _idx: usize) -> Option<RetrievalAccept> {
        Some(RetrievalAccept::ReplaceTriggerToken {
            replacement: format!("/{} {}", self.command, self.last_query),
        })
    }
}

// crates/spur-tui/src/components/git_ref_query_source.rs (NEW)

pub struct GitRefQuerySource {
    pub command: String,
    pub kind: GitRefKind,
    pub cwd: PathBuf,
    state: Arc<Mutex<GitRefState>>,
}

enum GitRefState {
    Pending,
    Loading { handle: JoinHandle<()>, rows: Vec<GitRef> },
    Loaded(Vec<GitRef>),
    Error(String),                 // "git not found" | "not a repo"
}

impl QuerySource for GitRefQuerySource {
    fn title(&self) -> &str { match self.kind { Branch => "Branch", Commit => "Commit" } }
    fn query_mode(&self) -> QueryMode { QueryMode::Async }
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> { /* spawn-on-first; nucleo filter */ }
    fn accept(&self, idx: usize) -> Option<RetrievalAccept> {
        Some(RetrievalAccept::ReplaceTriggerToken {
            replacement: format!("/{} {}", self.command, self.rows[idx].name),
        })
    }
}
```

The `RetrievalAccept::ReplaceTriggerToken` variant is currently `#[allow(dead_code)]` in spur (per kimi finding). v2 finally constructs it.

### 6.4 `TriggerDetector` extension

```rust
// crates/spur-tui/src/components/completion_trigger.rs

pub enum TriggerKind {
    Mention,
    Slash,
    /// v2: cursor in the arg region of a command whose ArgPickerSpec is Some.
    /// command_name lets InputCompletionPort look up the spec and instantiate
    /// the right QuerySource; spec carries no command name (decoupled).
    SlashArg { command_name: String },
}

// In TriggerDetector::step, after `^/<cmd>\s+` match:
let spec = registry.arg_picker_spec(cmd_name);
if spec.is_some() {
    return Open(SlashArg { command_name: cmd_name });
}
// else: no transition; cursor stays in plain text region
```

### 6.5 `InputCompletionPort` query-source instantiation

```rust
match spec.typed_hint {
    Some(ArgPickerHint::GitRef { kind }) => {
        Box::new(GitRefQuerySource::new(cmd_name, kind, session_cwd))
    }
    None => {
        Box::new(CommandInputQuerySource::new(cmd_name, spec.free_text_hint))
    }
}
```

### 6.6 `ConfigOptionUpdate` consumer

```rust
// crates/spur-tui/src/app.rs (extend apply_session_update around L2626)
SessionUpdate::ConfigOptionUpdate(payload) => {
    self.orchestrator.replace_session_config_options(
        &session_id,
        payload.config_options,
    );
    // replace_session_config_options already emits the registry-dirty signal
    // per v1 §6.3; no extra notification needed.
}
```

## 7. Wire path examples

### 7.1 `/review-branch main` (typed picker via `_meta`)

```
1. codex-acp emits session/update.AvailableCommandsUpdate with:
     AvailableCommand{
       name: "review-branch",
       description: "Review the code changes against a specific branch",
       input: Some(Unstructured{ hint: "branch name" }),
       meta: Some({"_codex.dev.arg_picker_hint": {"kind":"git_ref","git_ref":"branch"}}),
     }
2. spur-acp NativeAcpConnection broadcasts SessionNotification.
3. notification_pump → SpurEventBody::AgentNotification.
4. app::apply_session_update → AvailableCommandsUpdate arm (existing) →
   SessionDetailView::apply_available_commands → CommandRegistry::set_agent_commands.
5. Inside set_agent_commands, for each cmd:
     spec = spur_acp::adapter::arg_picker_hint::parse(&cmd);
   For "review-branch":
     spec = Some(ArgPickerSpec{
       free_text_hint: "branch name",
       typed_hint: Some(GitRef{Branch}),
     })
6. User types "/review-branch ", cursor past space.
   TriggerDetector matches ^/review-branch\s+, calls registry.arg_picker_spec("review-branch"),
   spec.is_some() → Open(SlashArg{ command_name: "review-branch" }).
7. InputCompletionPort sees SlashArg, fetches spec, matches typed_hint=GitRef{Branch},
   instantiates GitRefQuerySource{ command: "review-branch", kind: Branch, cwd: session_root }.
8. PickerShell opens; first refresh("") spawns
     `git -C <cwd> for-each-ref --sort=-committerdate --format='%(refname:short)' refs/heads refs/remotes`
9. Rows stream in. User types "ma", nucleo filters to {main, master, ...}.
10. User picks "main" → Accept(ReplaceTriggerToken{ "/review-branch main" }).
11. User Enter → Dispatch::PromptText (existing) → AgentConnection::prompt sends
    ContentBlock::Text("/review-branch main") to codex-acp.
12. codex-acp parses "/review-branch main" → Op::Review{ ReviewTarget::BaseBranch("main") }.
```

### 7.2 `/review let's check the auth changes` (free-text via plain `Unstructured`)

```
1. codex-acp's AvailableCommand for "review" has input=Some(Unstructured{
     hint: "optional custom review instructions"}) and meta=None.
2. parse(&cmd) returns Some(ArgPickerSpec{ free_text_hint: "...", typed_hint: None }).
3. /review picker instantiates CommandInputQuerySource (free-text mode).
4. User types arbitrary text; refresh() returns one synthetic row.
5. Accept → ReplaceTriggerToken("/review <typed>").
6. Submit → Dispatch::PromptText.
```

### 7.3 `/init` (no-arg)

```
1. codex-acp AvailableCommand for "init" has input=None.
2. parse(&cmd) returns None.
3. registry.arg_picker_spec("init") returns None.
4. TriggerDetector does NOT transition to SlashArg on "/init ".
5. User submits "/init" as plain text → Dispatch::PromptText (existing).
6. codex-acp parses "/init" and runs the bundled prompt.
```

### 7.4 `ConfigOptionUpdate` cache-refresh

```
1. External actor changes mode (e.g. another codex client on same session).
2. codex-acp emits session/update.ConfigOptionUpdate(new_options).
3. NativeAcpConnection broadcast → notification_pump → AgentNotification.
4. app::apply_session_update matches new ConfigOptionUpdate arm →
   orchestrator::replace_session_config_options → registry rebuild.
5. Next /model picker shows updated current_value.
```

## 8. Error handling

| Class | Trigger | Catch site | Feedback |
|---|---|---|---|
| **A1** Agent advertises `Unstructured` with empty hint | `parse(&cmd)` succeeds with `free_text_hint = ""` | Picker uses placeholder `"<arg>"` | Picker still works; no error. |
| **A2** Unknown `_meta` typed kind | `parse(&cmd)` returns `typed_hint: None` for unrecognised kinds | Falls through to free-text | Graceful degradation. |
| **A3** Malformed `_meta` JSON | `serde_json::from_value` fails on the hint sub-object | Logged at debug; `typed_hint: None` | Falls through to free-text. |
| **A4** Multiple `_<vendor>.dev.arg_picker_hint` keys | Vendor-collision (extremely unlikely) | Lex-first wins, deterministically | Internally consistent. |
| **G1** `git` binary missing | `Command::new("git").spawn()` returns `ENOENT` | `GitRefQuerySource` first refresh | Single error row "git not found in PATH". User can still type freely; submit dispatches as PromptText. |
| **G2** cwd not a git repo | `git for-each-ref` exits non-zero | `GitRefQuerySource` background task | Single error row "not in a git repo". User can still type. |
| **G3** Git output too large | >50 commits / >200 branches | `GitRefQuerySource` caps internally | Footer row "showing first N — type to narrow". |
| **G4** `ConfigOptionUpdate` payload malformed | Schema mismatch (codex-acp drift) | Existing notification pump deser path | Logged; cache untouched. |

E1-E5 from v1 §3.3 still apply.

## 9. Concurrency & cache freshness

- **`ArgPickerSpec` cache:** lives on `CommandRegistry` next to the per-command `name`/`description`. Repopulated atomically on every `set_agent_commands` call (which is triggered by `AvailableCommandsUpdate` notifications). Snapshotted into the `QuerySource` at picker-open time.
- **Git-ref picker:** `GitRefState` shared via `Arc<Mutex>`. Background task writes; picker reads under lock. Cancellation drops the source; the background task is allowed to finish quickly (small output).
- **`ConfigOptionUpdate` race with in-flight `set_config_option`:** both write to the same orchestrator cache via `replace_session_config_options`. Last-writer-wins; user perception is monotonic.

## 10. Testing strategy

### 10.1 New unit tests

| Component | Tests | Style |
|---|---|---|
| `arg_picker_hint::parse` | `input=None` ⇒ None; `input=Some(Unstructured(""))` ⇒ Some with empty hint; `_meta` GitRef Branch ⇒ Some(typed_hint=Branch); unknown kind ⇒ Some(typed_hint=None); multi-vendor `_meta` ⇒ lex-first; malformed JSON ⇒ typed_hint=None | Table-driven over JSON fixtures |
| `CommandInputQuerySource` | refresh sets last_query; accept produces ReplaceTriggerToken with the live query | Table-driven |
| `GitRefQuerySource` state machine | Pending→Loading on first refresh; Loading→Loaded on completion; Loading→Error on git failure; nucleo filter respected | Table-driven + mock-spawn |
| `apply_session_update::ConfigOptionUpdate` | Cache replaced; registry-dirty signal emitted | Integration with mock notification injection |
| `CommandRegistry::arg_picker_spec` | Returns Some for cached arg-required commands; returns None otherwise | Snapshot |
| `TriggerDetector` SlashArg with various ArgPickerSpec | `^/cmd\s+` opens SlashArg only when registry returns Some(spec); typed-vs-free-text distinction handled in InputCompletionPort, not detector | Snapshot |

### 10.2 Regression guards

- v1 `TriggerDetector` tests T1-T15 unchanged.
- v1 `ConfigOptionQuerySource` snapshot unchanged (different code path, untouched).
- v1 `submit_router` validator unchanged.
- Existing `apply_session_update::AvailableCommandsUpdate` arm test unchanged.

### 10.3 Integration smoke

End-to-end mock-codex test: emit `AvailableCommandsUpdate` with all 7 codex commands (3 with `input`, 4 without; one with `_meta` GitRef hint); assert:

1. All 7 appear in `CommandRegistry`.
2. Only 3 have `Some(ArgPickerSpec)`.
3. `/review-branch` picker is `GitRefQuerySource` (verify via `title()` returning "Branch").
4. `/review` picker is `CommandInputQuerySource` (verify via `title()` returning the hint).
5. `/init` does not open any picker on `/init ` typed.

### 10.4 Verification commands

```sh
cargo test -p spur-acp adapter::arg_picker_hint
cargo test -p spur-tui command_input_query_source
cargo test -p spur-tui git_ref_query_source
cargo test -p spur-tui app::tests::config_option_update
cargo test -p spur-tui completion_trigger::tests::slash_arg_via_registry_lookup
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## 11. Upstream changes (3 parallel tracks)

These run in parallel with v2 implementation. None block v2 from shipping; all reduce v2's transitional surface over time.

### Track 1 — codex-acp emits `_meta` typed hints (highest priority)

Open PR against zed-industries/codex-acp adding to `thread.rs:2735-2767`:

```rust
AvailableCommand::new("review-branch", "...")
    .input(Unstructured(UnstructuredCommandInput::new("branch name")))
    .meta(Some(json!({"_codex.dev.arg_picker_hint": {"kind":"git_ref","git_ref":"branch"}})));

AvailableCommand::new("review-commit", "...")
    .input(Unstructured(UnstructuredCommandInput::new("commit sha")))
    .meta(Some(json!({"_codex.dev.arg_picker_hint": {"kind":"git_ref","git_ref":"commit"}})));
```

Without this, v2's `GitRefQuerySource` is not instantiated for codex; spur falls through to `CommandInputQuerySource` (free-text). v2 still ships; UX is just "type the branch name" instead of "fuzzy-pick from local refs".

### Track 2 — ACP adds typed `AvailableCommandInput` variants

Open PR against zed-industries/agent-client-protocol adding to the schema:

```rust
#[non_exhaustive]
pub enum AvailableCommandInput {
    Unstructured(UnstructuredCommandInput),
    GitRef(GitRefCommandInput),       // NEW
    // FilePath, Choice, etc. as needed
}

pub struct GitRefCommandInput {
    pub kind: GitRefKind,             // Branch | Commit
    pub hint: String,
    pub meta: Option<Meta>,
}
```

Once landed and codex-acp upgrades, the `_meta` extension becomes redundant. spur's `parse()` adds a branch reading the typed variant first; `_meta` reads become legacy fallback then dead code.

### Track 3 — codex-acp surfaces missing `Op::OverrideTurnContext` knobs

Per gemini-worker analysis, codex-rs has 5 mutable per-turn knobs codex-acp does not expose via `config_options` (`personality`, `service_tier`, `summary`, `permission_profile`, `approvals_reviewer`) and 4 action `Op::*` variants entirely missing (`RunUserShellCommand`, `DropMemories`, `UpdateMemories`, `RefreshMcpServers`). File one issue per knob/action against zed-industries/codex-acp.

When `config_options` knobs land, v1's allow-list extends with one row each — **no spur recompile**. When new action RPCs land, they need new spur connection methods, but each is mechanical.

## 12. Build sequence

PR-0 must precede PR-1. PR-1 must precede PRs 2-4. PRs 2-4 are mutually independent.

0. **PR-0 (SDK upgrade prerequisite)** — bump workspace `agent-client-protocol` from `0.10.4` to **`0.11.1`** (latest published on crates.io; 0.12.x exists on GitHub but is not on the registry as of 2026-04-27) and add `unstable_session_model` feature.

   **This is a real refactor, not a one-line bump.** Empirical finding from a probe upgrade attempt on 2026-04-27: 0.11.x reorganized the public API by moving schema types into `agent_client_protocol::schema::*` while leaving runtime items (`Agent`, `Client`, `ConnectTo`, `AgentNotification`, `ClientRequest`, `ClientNotification`) at the crate root. The 0.10 → 0.11 release notes do not call this out — it's a silent reorganization. Mechanical effort:

   - **~30 source files** import schema types directly from the crate root and need their `use` statements updated. Notable: `crates/spur-acp/src/lib.rs` has 40+ symbols on a single `use`; `crates/spur-acp/src/protocol/claude_events.rs` has 10. The other 28 files are smaller — typically 1-5 imports.
   - **0 references to `session/stop`** in spur (only known wire-rename in the range; pre-greped clean).
   - Mixed-namespace imports must be split: root items (e.g. `Agent`) stay; schema items (e.g. `AvailableCommand`) move. A naive sed will break the mixed cases.
   - 5+ doc files in `docs/superpowers/{specs,plans}/` reference old paths in code blocks; non-blocking but should be touched up for consistency.

   **Stabilizations recovered by the bump** (drop these unstable feature gates after upgrade if currently in use anywhere): Session Config Options (stabilized in 0.10.8), `session/list` + `session_info_update` (stabilized in 0.11.1), `logout` capability (stabilized in 0.11.3).

   **Recommended worker:** `codex` — task shape is "syntactic refactor across single-file boundaries" which matches its `good_for`. Brain prepares a symbol-mapping cheatsheet (every symbol → its new path); codex executes the find-replace; brain reviews the diff.

   **Acceptance criteria:** `cargo check --workspace` clean; `cargo test --workspace` clean; no behavioural diff visible to existing tests; v1's `unstable_session_model` feature available for PR-1.

1. **PR-1 (v1)** — `/model` and `/effort` per `2026-04-27-codex-model-effort-slash-pickers-design.md`. Ships `ConfigOptionQuerySource`, `TriggerKind::SlashArg`, v1 connection method.
2. **PR-2 (v2 part 1)** — `ConfigOptionUpdate` arm in `apply_session_update`. ~30 LOC + test. Smallest possible.
3. **PR-3 (v2 part 2)** — `arg_picker_hint::parse` + `CommandInputQuerySource` + registry extension. Ships free-text picker for any agent-advertised command with `Unstructured` input. Validates `RetrievalAccept::ReplaceTriggerToken` revival.
4. **PR-4 (v2 part 3)** — `GitRefQuerySource` + `_meta` typed-hint parsing branch. Tested with mock `_meta` until codex-acp Track-1 upstream lands.

PR-2 and PR-3 ship the bulk of the user-visible v2 value. PR-4 is gated on the `_meta` extension (either codex-acp Track-1 or our own mock for testing).

**If PR-0 is deferred** (Option U-2 from the upgrade brainstorm): v1 + v2 still ship, but on the older 0.10.4 SDK with `unstable_session_model` added in PR-1's Cargo edit. Functional but accumulates upgrade debt.

## 13. Open questions

None at design close. Q1-Q6 in §4 resolved.

## 14. Future work (deferred)

- **`/mode` for codex's Approval Preset.** Collides with spur-local `/mode`. Namespacing decision (e.g. `/codex:mode`) deferred to a separate brainstorm.
- **Multi-arg commands.** Current `SlashArg` state tracks one arg region. `/review --branch x --focus security`-style multi-flag commands need `SlashArg { command, arg_index }` extension; defer until any agent emits one (none do today).
- **`FilePath` and `Choice` typed hints.** Add when an agent emits them in `_meta`. Mechanism is symmetric to `GitRef`; one `QuerySource` impl each.
- **Real-time `git` ref refresh.** v2 snapshots at open-time; live refresh is overkill for the 99% case.
- **MCP server picker** (`/mcp` to enable/disable per-session servers). codex-acp accepts MCP servers in `session/new` but doesn't expose runtime toggling. Tracked in §11 Track-3.
- **Generic `Choice` typed hint** that consumes inline choices from `_meta`. Would let an agent advertise a closed enum without a config_options round-trip. Defer until needed.
