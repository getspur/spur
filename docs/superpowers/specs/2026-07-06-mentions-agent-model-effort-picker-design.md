# Mentions: cascading worker → agent → model → effort picker

**Status:** design approved, pending implementation plan
**Date:** 2026-07-06
**Owners:** Kevin Truong (kevin.truong.ds@gmail.com)
**Scope:** Extend the brain-session `@worker` mention so a user can, in one continuous typed completion, pin a worker's **agent** (persona), **model**, and **effort** for that turn — surfaced to the brain as a richer advisory hint, with no change to the already-shipped `delegate_to_worker` MCP schema.

---

## 1. Goal

Today `@worker:<name>` mentions (`crates/spur-tui/src/mentions/worker_source.rs`) carry only a bare worker name into the brain's prompt, rendered as a single advisory hint line ("*User-suggested workers for delegation this turn: X — preference, not override*"). The brain still has to call `delegate_to_worker(agent=..., profile=..., model=..., effort=...)` itself with no steer on persona, model, or reasoning effort.

This spec adds a **cascading completion** to the same `@` mention: after picking a worker, continuing to type resolves, in order, an optional **agent** (persona), **model**, and **effort**, fuzzy-matched against real candidate pools — and composes all of it into one enriched advisory hint. Any slot left unpicked falls back to today's implicit default (profile frontmatter → agent default, per the existing D8 precedence in `crates/spur-core/src/orchestrator/delegation/execute.rs::resolve_effective_model_effort`).

## 2. Vocabulary (locked in during brainstorming — avoids a naming collision)

| Term (this spec / mentions UI) | Maps to (unchanged) | Controls |
|---|---|---|
| **worker** (a.k.a. agentkind) | `delegate_to_worker.agent`, `AgentConfig`/`AgentKind` | which CLI/process runs |
| **agent** | `delegate_to_worker.profile`, `AgentProfile` (`crates/spur-core/src/agent_profiles/`) | behavior/persona |
| **model** | `delegate_to_worker.model` | which model; cost |
| **effort** | `delegate_to_worker.effort` | reasoning depth; cost |

`delegate_to_worker`'s existing flat MCP fields (`agent`, `profile`, `model`, `effort`) are **unchanged** — this is purely additive at the mentions/UI layer. "Agent" as used in this spec never collides with `delegate_to_worker.agent` in code because the two live at different structural paths (top-level MCP param vs. a mentions-layer struct field); the collision risk was flagged and resolved before writing this doc.

## 3. Non-goals

- **No change to `delegate_to_worker`/`delegate_parallel`/`submit_plan`'s MCP schema.** `profile`/`model`/`effort` already exist and already work end-to-end (confirmed live in `docs/rca/2026-07-05-codex-model-effort-profile-subagent-evaluation.md`). This spec only makes it easier to *compose a value* for them from the TUI.
- **Not a hard override.** Exactly like today's bare worker hint, the composed selection is advisory text prepended to the prompt ("*preference, not override*"). The brain still decides and still calls `delegate_to_worker` itself. SPUR never auto-injects the picked values into a tool call on the brain's behalf.
- **Not for direct (single-agent) sessions.** `@worker` mentions — and this extension — remain brain-session-only (`MentionRegistry::for_brain_session`); direct sessions stay files-only (`MentionRegistry::for_direct_session`).
- **No live, per-keystroke ACP round-trip.** Model/effort candidates come from a persisted cache (§6), never a synchronous probe blocking the picker.
- **No visual/graphical picker widget.** Reuses the existing `@`-trigger + fuzzy-filter text mechanism (`Matcher` in `registry.rs`) exactly as today; no new popup chrome beyond the existing completion dropdown.

## 4. Background: what already exists (cross-check findings)

Confirmed by reading `crates/spur-core/src/agent_profiles/`, `crates/spur-acp/src/{profile_strategy,spur_agent_caps}.rs`, and `crates/spur-core/src/orchestrator/delegation/{worker_attempt,execute}.rs`, cross-referenced with `docs/rca/2026-07-05-codex-model-effort-profile-subagent-evaluation.md`:

- **Model/effort override plumbing is agent-kind-agnostic and already fully wired**: `SpurAgentCaps::model_option()`/`thought_level_option_from()` (`spur_agent_caps.rs:97,235`) resolve whichever live `SessionConfigOption` the running agent advertises (category `Model`/`ThoughtLevel`, or legacy `"model"`/`"reasoning_effort"` ids); `apply_session_overrides` (`worker_attempt.rs:243-407`) sends `session/set_config_option` for agent-select, model, and effort, all fail-soft.
- **Only agent (persona) *selection surface* differs per kind** (`profile_strategy.rs::ProfileStrategy::for_kind`): claude-code/opencode use `ConfigOption`; kiro uses `SpawnArg{flag:"--agent"}` (this fix landed after the RCA was written, confirmed in current code); codex has no live selection surface (upstream gap, tracked, not client-fixable).
- **`AgentConfig` has no declarative model/effort field** — only `args` (spawn-argv escape hatch) and `profile: Option<ProfileConfig>` (select-strategy override). Per-agent model/effort *defaults* live only in `.spur/agents/<name>.md` frontmatter (`AgentProfile.model`/`.effort`), consumed by D8 precedence.
- **`@worker` mentions today carry only `name`/`description`/`tier`**: `WorkerMentionDescriptor` (`worker_source.rs:10-18`) built by `App::build_worker_snapshot` (`crates/spur-tui/src/app/events.rs:85-102`) straight from `AgentConfig` entries. `MentionEntry` (`entry.rs:19-43`) has no agent/model/effort slot. `prepend_worker_hint` (`hint.rs:21-44`) turns picked worker mentions into one flat advisory line — no persona/model/effort ever flows through it.
- **`Orchestrator::check_agents`** (`crates/spur-core/src/orchestrator/connection.rs:54-77`) only calls `initialize`, never `session/new` — so it does not (and today cannot) observe `config_options`, which live only on `NewSessionResponse`.
- **Empirical probe (2026-07-06, this investigation)**: a raw ACP handshake (`initialize`→`session/new`, no prompt) against the real local CLIs measured real candidate-pool sizes and timing (see §9). This is why a live per-keystroke probe is infeasible and a persisted cache is required, not just an optimization.

## 5. Design decisions

| # | Question | Choice | Rationale |
|---|---|---|---|
| D1 | Where does agent/model/effort selection happen? | One continuous `@worker` completion; tokens typed after the worker resolve positionally into agent → model → effort slots. | Matches "type continuously, resolve positionally" UX; avoids 4 separate `@` triggers or an unmanageable flat combined list (opencode alone has 133 models — see §9). |
| D2 | What does the composed mention become? | One `MentionEntry`/`ProtectedRange` with URI `worker://<name>?agent=<x>&model=<y>&effort=<z>` — only picked slots present as query params. | Single atom, single hint line; unset slots stay implicit (existing D8 default). |
| D3 | Where do agent (persona) candidates come from? | `.spur/agents/*.md` via existing `AgentProfile::load`, filtered to profiles `render_for_kind` supports for the selected worker's kind. | Reuses existing profile mechanism unchanged; no new persona source. |
| D4 | Where do model/effort candidates come from? | A persisted, per-worker-name cache (`~/.spur/cache/agent-model-catalog.json`), never a live per-keystroke probe. | ACP only reveals real choices via a live `session/new` handshake, measured at 0.4–7s (§9) — far too slow to block typing. |
| D5 | What populates the cache? | Both: (a) `spur agents check`, extended to also perform `session/new`+shutdown and persist `config_options`; (b) a lazy background probe the picker triggers on a missing/stale entry, non-blocking. | (C) from brainstorming — eager path for normal operation, self-healing fallback so a fresh install isn't stuck with an empty picker. |
| D6 | Cache staleness policy | 24h TTL (mirrors `upgrade_check.rs`'s existing `CHECK_INTERVAL`), plus immediate invalidation if the registered `AgentConfig.command`/`args` (`cli_identity`) drifts from what's stamped in the cache entry. | CLI upgrades/swaps can change the real catalog; TTL alone wouldn't catch that promptly. |
| D7 | Cache file format/location | Mirrors `crates/spur-cli/src/upgrade_check/cache.rs` exactly: versioned struct, atomic temp-file-then-`rename` write, `~/.spur/cache/...json`. | Direct existing precedent in this codebase; no new pattern invented. |
| D8 | Is the composed selection a hard override? | No — fail-soft advisory hint only, same contract as today's bare-name hint. | Consistent with existing philosophy; brain still calls `delegate_to_worker` itself. |
| D9 | Can a frozen slot be edited? | Yes — backspacing into an already-frozen slot's text re-opens that slot and clears everything after it. No separate undo stack; the slot's value is just whatever the (re-parsed) substring says. | Falls out naturally from normal fuzzy-filter text editing; no new state needed beyond re-parsing on edit. |

## 6. Architecture

### 6.1 Data model

**`AgentModelCatalog`** (new), cached at `~/.spur/cache/agent-model-catalog.json`, structure mirrors `upgrade_check/cache.rs::CacheV1`:

```rust
struct AgentModelCatalogV1 {
    version: u32, // 1
    entries: HashMap<String, WorkerCatalogEntry>, // keyed by worker NAME, not bare kind
}

struct WorkerCatalogEntry {
    probed_at: DateTime<Utc>,
    cli_identity: String,           // command + args joined; drift ⇒ immediate staleness
    models: Vec<ConfigOptionChoice>,  // {value, name, description}
    efforts: Vec<ConfigOptionChoice>,
}
```

Keyed by **worker name** (registry entry), not bare `AgentKind` — two registrations of the same kind could have different args/accounts and thus different real catalogs.

**Mentions-layer additions** (`crates/spur-tui/src/mentions/`):

- `MentionEntry` (`entry.rs`) gains three new optional fields: `agent: Option<String>`, `model: Option<String>`, `effort: Option<String>`. `None` ⇒ not picked ⇒ implicit default at delegation time (unchanged D8 precedence).
- Composed URI: `worker://codex?agent=spur-narrow-implementer&model=gpt-5.5&effort=low` (only picked params present).

### 6.2 Picker completion state machine

```rust
enum WorkerMentionSlot { Worker, Agent, Model, Effort, Done }
```

- `@` opens `Slot::Worker` — unchanged today's flat fuzzy worker list.
- Accepting a worker candidate advances to `Slot::Agent`; candidates come from `.spur/agents/*.md` filtered to the selected kind (D3). Zero candidates ⇒ auto-advance to `Slot::Model`.
- Each subsequent token is fuzzy-matched against the **current open slot's** candidate pool. A high-confidence match (or explicit accept from the dropdown) freezes that slot and advances to the next. `Slot::Model` candidates come from the cache's `.models` for that worker name (empty/"probing…" if cold); `Slot::Effort` from `.efforts`.
- **Tokens are `,`-delimited, not whitespace-delimited** (`@codex,narrow,gpt-5,high`), discovered necessary during implementation verification: `InputBar`'s completion-trigger detector (`components/completion_trigger.rs`) closes the mention popup unconditionally on the first whitespace character typed (a pre-existing rule serving today's single-token file/issue/code mentions, which never needed to survive a space). A whitespace-delimited cascade would therefore never reach `MentionRegistry::query` with more than one token live in the TUI, even though unit tests calling `query()` directly with a hand-crafted multi-word string never exercise the trigger layer and so didn't catch this. `,` was chosen over `/` because real catalog model values use `/` for provider qualification (e.g. opencode's `provider/model` ids number in the hundreds); `,` has no known collision with any real worker/agent/model/effort identifier convention. This keeps `completion_trigger.rs` — a shared, invariant-heavy FSM used by every mention/slash-command trigger — completely untouched; the fix is isolated to `MentionRegistry`'s own tokenizer.
- Any slot can be left unfilled — ending the mention (whitespace, which closes the trigger popup per the above, or submit) with slots still open leaves them `None`.
- A token that doesn't fuzzy-match anything in the current slot above threshold is **not consumed** — the mention finalizes with remaining slots at `None`, and the token (and everything after it, `,`-joined) becomes ordinary unconsumed suffix text. This guarantees the picker never eats real user text on a near-miss.
- Backspacing into an already-frozen slot's text re-opens that slot and clears everything after it (D9) — no separate undo stack; re-parses the edited substring.

### 6.3 Probe & cache mechanics

- **Probe execution** reuses the existing `AgentConnection` `initialize`→`session/new`→`shutdown` sequence (the same shape `check_agents` already does for `initialize`), reading `config_options` off `NewSessionResponse` via `SpurAgentCaps`. Not the raw JSON-RPC script used to measure timing in this investigation (§9) — that was throwaway.
- **`spur agents check`** (`crates/spur-core/src/orchestrator/connection.rs::check_agents`) is extended to also call `session/new`+shutdown per agent and persist the resulting `config_options` into the cache. **This is not free** — it adds the measured 0.4–7s per agent (§9) to what `check_agents` costs today, since it currently only calls `initialize`.
- **Lazy fallback**: when the picker needs `Slot::Model`/`Slot::Effort` candidates for a worker name with a missing or stale (TTL or `cli_identity` mismatch) cache entry, it triggers the same probe on a background task, non-blocking; the picker shows whatever it already has (possibly nothing) immediately, and a subsequent keystroke picks up the refreshed cache once the background probe lands. The exact async-task → UI-state channel should reuse whatever existing mechanism `spur-tui/src/app/` uses for background work feeding into rendering (no clean existing precedent for a multi-second background probe was found during this investigation — `registry.rs`'s `CodeGraphSourceUpdate` is a cheap synchronous staleness check, not a real async task; the implementation plan should identify the right channel rather than inventing one ad hoc).
- **Cache read/write**: mirrors `upgrade_check/cache.rs` exactly — versioned struct, atomic temp-file-then-`rename`, no locking (last-writer-wins is acceptable for a cache).

### 6.4 Hint generation

`prepend_worker_hint` (`crates/spur-tui/src/mentions/hint.rs:21-44`) is extended to parse the full `worker://<name>?agent=&model=&effort=` URI instead of just the bare name, preserving **distinct full tuples** (dedup exact repeats only — not by bare name, since the same worker could legitimately be mentioned twice with different combos). Bare `@worker:<name>` mentions (no query params) continue to emit **exactly today's hint text**, unchanged — the enriched form is strictly additive. Concretely, the membership check (`known_workers.contains(*n)`) must split the URI remainder on `?` first and check only the name portion — today it checks the whole remainder, which works only because there's no query string yet.

Example enriched line:
> `[UI hint] User-suggested workers for delegation this turn: codex (agent=spur-narrow-implementer, model=gpt-5.5, effort=low) (preference, not override; honor unless delegation.avoid_for clearly matches, or the task needs a different combination).`

## 7. Touchpoints

| Crate | Change |
|---|---|
| `crates/spur-acp` | New `agent_model_cache` module (read/write/staleness, mirroring `upgrade_check/cache.rs`) — lives here, not `spur-cli`, because both `spur-core` (`check_agents`) and `spur-tui` (picker lazy fallback) need to read/write it, and `spur-tui` depends on `spur-acp` directly but not on `spur-cli`. Also hosts the shared probe helper (`initialize`→`session/new`→`shutdown`, reusing existing `AgentConnection`/`SpurAgentCaps`). |
| `crates/spur-core` | `Orchestrator::check_agents` (`orchestrator/connection.rs:54-77`) gains `session/new`+shutdown per agent, calling into the new `spur-acp` probe helper and persisting the result via the shared cache module. **Verified via spur-analyst SQL: `check_agents` has exactly one caller today** (`spur-cli`'s `cmd_agents` `Check` branch) — the added per-agent latency (§9) only affects that manual CLI command, not any background/hot path. |
| `crates/spur-tui/src/mentions/` | `entry.rs` (`MentionEntry` new fields — see construction-site note below), `worker_source.rs`/`registry.rs` (completion state machine, `Slot` enum, agent/model/effort candidate sourcing from the shared cache), `hint.rs` (`prepend_worker_hint` URI parsing extension). **Verified via grep (struct-literal construction isn't a graph edge, so `code_*` returned zero hits — grep is the correct fallback here): every `MentionEntry` literal lists all 11 current fields explicitly, none use `..Default::default()` spread.** Adding `agent`/`model`/`effort` therefore requires a one-line touch at all 8 construction sites: `registry.rs:679` (section-header rows) and `:1226` (test helper), `worker_source.rs:39`, `entry.rs:100` (`entry_for_path`, shared by file/directory entries), `issue_source.rs:81`, `code_graph/source.rs:206` and `:239`, `datasource_source.rs:39`. Mechanical (`None` at each), but real — the implementation plan should enumerate these explicitly rather than discover them mid-task. |
| `crates/spur-tui/src/app/` | Background probe trigger (calling the same `spur-acp` probe helper) + result channel (lazy fallback path). |
| `crates/spur-cli` | **No change** beyond whatever `check_agents`'s CLI-facing output already does — the cache module does not live here (see `spur-acp` row). |

**Verified blast radius of the URI format change**: grepping all of `crates/spur-tui/src/` for `worker://` shows `hint.rs`'s `strip_prefix("worker://")` is the **only** place in the entire crate that parses this URI shape — `worker_source.rs` only ever constructs it, never re-parses it. So appending query params is a narrowly-scoped, low-risk change confirmed by exhaustive search, not just inspection of the one function being edited.

**New risk surfaced during verification, not previously covered**: `Orchestrator::create_connection` (`orchestrator/connection.rs:120-138`) spawns agent connections via `build_connection_from_transport(config, args, perm_tx, &self.repo_root)` — the **live repo root**, not an isolated worktree (unlike real delegations, which run `run_one_worker_attempt` against a dedicated worktree). A probe-only `session/new` added to `check_agents` would therefore run with `cwd` = the actual repo, not a disposable directory. Checked `crates/spur-acp/src/session_lock.rs` for a collision risk — it's fine, that lock guards re-*attaching* to an existing session id, not creating a new one, so no conflict with a live brain/worker session in the same repo — but some agent CLIs create local state/config on session creation, so the implementation plan should consider probing against a scratch temp directory instead of `self.repo_root` to avoid incidental writes into the live tree.

## 8. Failure modes & mitigations

| Failure | Mitigation |
|---|---|
| Probe fails (unauthenticated CLI, missing binary, timeout) | Cache entry not written for that worker; picker shows zero candidates for `Slot::Model`/`Slot::Effort` and auto-skips to `Done` (same as "no agents exist for this worker" today). A short-TTL (~5 min) negative cache entry avoids re-probing a broken CLI every keystroke. |
| Corrupted/unparseable cache file | Treated as absent, mirroring `upgrade_check/cache.rs`'s existing version-mismatch-returns-`None` behavior. |
| Concurrent probes (two SPUR processes, or `agents check` racing a lazy probe) | Atomic temp-file-then-rename ⇒ last-writer-wins; acceptable for a cache. |
| User types a token matching nothing in the open slot | Token is not consumed; mention finalizes with remaining slots `None`; token becomes ordinary prompt text. |
| Same worker mentioned twice with different combos in one message | Both tuples surface as separate lines in the hint; brain reconciles (out of scope to disambiguate further). |

## 9. Evidence: live ACP probe (2026-07-06)

Raw ACP handshake (`initialize` → `session/new`, no prompt ever sent) run against the real local CLIs to determine feasibility before committing to a cache-based design:

| Kind | `initialize` | `session/new` | **Total wall-clock** | Model options | Effort options | Notes |
|---|---|---|---|---|---|---|
| opencode | 1.9s | 7.0s | **8.9s** | 133 | 2 | also a 2-option `mode` |
| codex | 2.8s | 0.4s | **3.2s** | 4 (16 model×effort combos in a top-level `models` block) | 4 | also `mode` (3), `fast-mode` (2) |
| claude-code | 2.9s | 2.4s | **5.2s** | 5 | 6 | also a 10-option `agent` config (claude-code-acp's own built-in personas — separate from SPUR's `.spur/agents/*.md`, not reused by this design) |

This confirms: (a) a live per-keystroke probe is infeasible (3–9s dominated by process spawn / `npx` resolution / opencode's catalog enumeration), and (b) candidate pools can be large (133 for opencode) — ruling out a flattened combined fuzzy list (§ rejected "Approach 3" during brainstorming).

## 10. Out of scope / future work

- Surfacing claude-code-acp's own built-in `agent` config option (10 built-in personas) as an *additional* agent candidate source alongside `.spur/agents/*.md` — noted as a possible follow-up, not adopted here to keep one source of truth for "agent."
- A `spur agents refresh-models` explicit manual-refresh CLI command, if the TTL + lazy fallback prove insufficient in practice.
- Configurable TTL (currently hardcoded at 24h, matching `upgrade_check.rs` precedent).
- Visual/graphical picker treatment — explicitly deferred; this spec reuses the existing text completion mechanism only.

## 11. References

- `docs/rca/2026-07-05-codex-model-effort-profile-subagent-evaluation.md` — per-kind model/effort/profile capability matrix, live-confirmed.
- `docs/superpowers/specs/2026-07-04-per-delegation-model-effort-override-design.md` — the underlying `delegate_to_worker` `model`/`effort` override this spec builds on top of (unchanged by this spec).
- `docs/superpowers/specs/2026-04-20-worker-mentions-design.md` / `docs/superpowers/plans/2026-04-20-worker-mentions.md` — original `@worker` mention design (predates model/effort entirely).
- `crates/spur-cli/src/upgrade_check/cache.rs` — cache read/write/atomic-rename precedent this design mirrors directly.
- Live probe evidence: raw ACP handshake measurements captured 2026-07-06 against locally-installed `opencode`, `codex-acp@1.0.2`, `claude-agent-acp@0.54.1` (see §9).
- Upstream/downstream integration cross-check (2026-07-06, `code_*`/spur-analyst SQL): confirmed `check_agents` has exactly one caller (low blast radius for the added latency); enumerated all 8 `MentionEntry` construction sites needing a mechanical touch (struct-literal construction isn't a graph edge, so this required a targeted grep fallback after `code_*` returned zero hits); confirmed `prepend_worker_hint`'s URI parsing is the only consumer of the `worker://` shape anywhere in `spur-tui`; surfaced a new risk (probe sessions run against the live `repo_root`, not an isolated worktree) via `Orchestrator::create_connection` and `session_lock.rs`. Findings folded into §6.4 and §7 above.
