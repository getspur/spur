# Design: `/configure` — agent/worker settings browser

**Date:** 2026-07-06
**Status:** Approved for planning

## Motivation

Per-agent worker settings (`additional_directories`, spawn `args`, capability
tags, `skip_permissions*`, default `profile`) today only exist as raw
`.spur/config.toml` edits, or a 3-key CLI (`spur config set tui.*`) that
doesn't touch agent config at all. There is no in-TUI way to inspect or
change a worker's configuration. `/configure` adds a dedicated settings
browser inside the TUI, phased as:

- **Phase 1 (this spec):** agent/worker-level settings.
- **Phase 2 (future, not detailed here):** fold the already-scattered
  TUI/session preferences (`/theme`, `/vim`, `disable_paste_burst`) into the
  same browser, once Phase 1's view scaffolding exists.

## Current state (grounding)

- No `/configure`, `/config`, or `/settings` command exists today. The local
  command catalog (`SpurLocalSource::entries`,
  `crates/spur-tui/src/commands/spur_local.rs:54-147`) has `/help`, `/clear`,
  `/mode`, `/sessions`, `/cost`, `/quit`, `/vim`, `/issues`, `/theme`,
  `/notebook`, `/sprints`.
- `SpurConfig` (`crates/spur-acp/src/config/mod.rs:462-505`) is large — 15+
  subsections. Out of scope beyond the `agents` section for this feature.
- Three inconsistent existing patterns for config-like commands:
  - `/theme <name>` — live-apply **and** auto-persist via
    `spur_acp::config::update_config` (full-`SpurConfig` parse → mutate →
    reserialize → atomic rename), `app/overlays.rs:21-82`.
  - `/vim` — live-apply only, in-memory `edit_mode`, never auto-persists;
    nudges the user toward the CLI (`app/action_routing/session_config.rs:60-79`).
  - CLI `spur config set <key> <value> [--global]` — persist-only, explicitly
    **not** live ("takes effect on next `spur tui` invocation"),
    `crates/spur-cli/src/commands/config_set.rs`, only 3 keys today
    (`tui.edit_mode`, `tui.disable_paste_burst`, `tui.theme`), via a sparser
    `set_key_path` mutation than `/theme`'s full-struct round-trip.
- **No live config-reload exists anywhere in the codebase today.** Two code
  comments explicitly flag this as unbuilt (`mentions/registry.rs:252`,
  `app/events.rs:105`).
- Confirmed via `code_callers`: `handle_delegations`
  (`orchestrator/delegation/mod.rs:73-423`) is spawned from exactly 3 sites —
  `Orchestrator::create_brain_session`, `Orchestrator::load_brain_session`,
  `Orchestrator::run_adhoc` (all in `crates/spur-core/src/orchestrator/`) —
  and each spawn captures `agent_configs: Vec<AgentConfig>` **once**; the
  loop clones that same static snapshot for every delegation dispatched
  through the rest of that session's lifetime. This is why hand-editing
  `.spur/config.toml` today has zero effect on an already-running session.

## Scope

### Editable in v1 (curated subset of `AgentConfig`'s 17 fields)

`additional_directories`, `args`, `capabilities`, `skip_permissions` +
`skip_permissions_args` + `skip_permissions_session_mode`, `profile`
(default `ProfileConfig` override).

### Excluded from v1 (file-edit + restart only)

`name`, `command`, `transport`, `kind` — these define the agent's
identity/connection shape. Live-editing them while a session for that agent
might be mid-flight is a foot-gun, and they're set-once-at-registration
values; a future "add/remove agent" flow is a separate feature.

### Explicitly out of scope

- Editing any `SpurConfig` section other than `agents` (Phase 2 covers
  `tui.*`; everything else — `worktree`, `pm`, `cost`, `plan`, loops, etc. —
  is not addressed by this spec).
- General hot-reload of `.spur/config.toml` from external writers (hand
  edits, `spur config set`). Only edits made *through* `/configure`, from
  inside the running TUI process, live-apply — see Approach discussion below.
- Applying changes to an **already-running** worker session. A worker's
  `cwd`/`additional_directories` are baked into its live ACP `session/new`
  call at spawn time; nothing short of killing and respawning that session
  changes it. Live-apply here only affects **delegations dispatched after**
  the edit.

## Approaches considered

1. **Shared mutable cell on `Orchestrator`** (chosen) — wrap the agent-config
   state in `Arc<parking_lot::RwLock<Vec<AgentConfig>>>` (`parking_lot` is
   already a direct `spur-core` dependency). `handle_delegations`'s loop
   reads a fresh clone each iteration instead of a value captured once at
   spawn. Smallest, most localized change.
2. **General file-watcher hot-reload** — `notify`-based watcher on
   `.spur/config.toml`, reloading on any external change. More general
   (benefits the CLI and hand-edits too) but meaningfully bigger scope:
   debouncing, partial-write/invalid-TOML handling, reloads all of
   `SpurConfig` rather than the slice this feature needs. Deferred; a
   plausible future evolution of Approach 1's primitive, not a prerequisite.
3. **Event-broadcast instead of a lock** — persist, then emit through the
   existing `event_tx`/funnel machinery for the orchestrator to swap its
   local copy on receipt. Reuses an existing propagation pattern but adds
   async ordering/race considerations for no benefit over a direct lock in
   this single-process case.

**Chosen: Approach 1.**

## Architecture

```
SpurLocalSource::entries()          — register "configure" command
  → Action::NavigateTo(ViewId::AgentConfigBrowser { preselect })
  → new view: crates/spur-tui/src/views/agent_config_browser.rs
       left pane: list of configured agents (from self.config.agents.entries)
       right pane: selected agent's editable fields (curated subset above)
       save keybind → Action::AgentConfigSaveRequested { name, updated_entry }

App (action_routing) on AgentConfigSaveRequested:
  1. update self.config.agents.entries in place (own display stays correct)
  2. self.user_input_tx.try_send(UserInput::UpdateAgentConfig { name, updated_entry })

Orchestrator on UserInput::UpdateAgentConfig:
  1. spur_acp::config::update_config(&config_path, |c| { replace the
     matching entry in c.agents.entries })         — persist
  2. *self.agent_configs.write() = <new Vec<AgentConfig>>   — live-apply
  3. emit a confirmation/error event back to the TUI (mirrors existing
     IssuesLoaded / loop-refresh event patterns)

Orchestrator.agent_configs: Arc<parking_lot::RwLock<Vec<AgentConfig>>>
  read fresh by handle_delegations at the top of each loop iteration,
  instead of a Vec captured once at the 3 spawn sites.
```

`UserInput::UpdateAgentConfig { name, updated_entry }` is a new variant on
the existing App→Orchestrator command enum
(`crates/spur-tui/src/app/mod.rs:90-212`), following the same
request/response shape as existing pairs like `PauseLoop`/`ResumeLoop` and
`UpdateIssue`.

## Data flow

1. User: `/configure` (bare) or `/configure <agent-name>` (jump straight to
   that agent) → `SpurLocalSource` entry → `Action::NavigateTo(ViewId::AgentConfigBrowser { preselect })`.
2. View opens, left pane populated from `self.config.agents.entries`
   (loaded at App startup — same source `/theme` already reads/writes for
   `tui.theme`).
3. User edits a field, presses save keybind →
   `Action::AgentConfigSaveRequested { name, updated_entry }` → App updates
   its own `self.config.agents.entries` in place, then
   `self.user_input_tx.try_send(UserInput::UpdateAgentConfig { .. })`.
4. Orchestrator persists (4a), live-applies via the lock (4b), and emits a
   confirmation/error event (4c) so the TUI can flash "saved — applies to
   next delegation" (mirroring `/theme`'s existing persistence hint) or
   surface a failure.
5. Any delegation dispatched afterward by any of the 3 `handle_delegations`
   loops (already-running or freshly spawned) reads the fresh `Vec` from the
   lock.

## Error handling

- **Validation before send:** `additional_directories` entries must be
  absolute paths — reuse the exact check
  `sanitize_agent_additional_directories` already applies
  (`crates/spur-acp/src/config/mod.rs:252-266`), surfaced as an inline
  field-level error in the view *before* dispatch, rather than silently
  dropping the bad entry the way today's static config load does.
- **Persist failure** (disk full, permissions): `update_config`'s
  `anyhow::Result` propagates back through the confirmation event as an
  error variant; the TUI flashes the failure and does **not** apply the
  live-apply side — persist-then-apply, never apply-then-persist, so memory
  and disk can't diverge on a write failure.
- **Concurrent external edit:** `update_config`'s existing doc comment
  already states "last-rename-wins... do NOT use for fields requiring CAS
  semantics" — an accepted, pre-existing tradeoff (same as `/theme` today),
  not a new risk this feature introduces.

## Testing

Per repo convention (failing test first, then the fix/feature):

- Unit tests on the new view's field-edit/validation logic (pure functions,
  no I/O), modeled on existing `registry.rs`/`submit_router.rs` test style.
- Unit test for the persist closure against a temp `.spur/config.toml`,
  modeled on `config_set_preserves_sparse_file_when_setting_one_key`-style
  tests already in `crates/spur-cli/src/commands/config_set.rs`.
- Orchestrator-level test proving live-apply: after
  `UserInput::UpdateAgentConfig`, a subsequently-dispatched delegation
  (within the same already-running `handle_delegations` loop, no restart)
  observes the new `additional_directories`/`args`. This is the test that
  actually proves live-apply works, not just that the lock compiles.

## Known limitations / accepted tradeoffs

- `update_config`'s full-`SpurConfig` serde round-trip drops hand-added TOML
  comments in `.spur/config.toml` on save. Pre-existing behavior (`/theme`
  already does this today), not introduced by this feature.
- Live-apply only reaches **new** delegations in **already-running or
  freshly-spawned** `handle_delegations` loops. It does not reach
  already-spawned worker sessions, and it does not reach edits made outside
  the TUI process (CLI `spur config set`, hand-edited file) — those still
  require a restart, unchanged from today.

## Future work (explicitly deferred)

- Phase 2: fold `/theme`, `/vim`, `disable_paste_burst` into the same
  browser view.
- General file-watcher hot-reload (Approach 2 above), if the CLI/hand-edit
  live-apply gap becomes a real pain point.
- Add/remove agent flow (`name`, `command`, `transport`, `kind`).
