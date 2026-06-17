# Layered (Multi-Level) SPUR Config — Design

**Date:** 2026-06-17
**Status:** Approved (brainstorm complete)
**Topic:** User-level + project-level config with inheritance/override

## Problem

SPUR config lives in `<repo>/.spur/config.toml`. A `~/.spur/config.toml` is
*recognized* but only as an all-or-nothing **fallback**: if the project file
exists it wins entirely and the user file is ignored — there is no field-level
inheritance. Consequences:

- Personal preferences (brain choice, TUI theme/edit-mode, permission posture,
  Telegram bot, cost DB path) and the agent roster must be re-established in
  every repo. A fresh `git clone` needs a full `spur init`.
- The loader logic is duplicated (`spur-cli/src/main.rs::load_config_for_repo`,
  `spur-cli/src/commands/pm_ingest.rs`) and inconsistent (`config_check.rs`
  reads project-only and hard-errors when it is absent).
- A `BotConfig` doc comment already claims config is "from ~/.spur/config.toml +
  .spur/config.toml" — aspirational; never implemented.
- A true 3-level cascade exists, but only for **themes**
  (`.spur/themes/*.yaml`), not for config.

## Goal

Real **layering**: built-in `Default` (bottom) → `~/.spur/config.toml` (user) →
`<repo>/.spur/config.toml` (project, top). The user layer is the base; the
project layer inherits it and overrides only what it sets, **field by field,
across every section**. Configure once at the user level; clone any repo and it
just works; projects stay thin and override only specifics.

## Decisions (from brainstorm)

1. **Scope:** full layering across *every* section (not just preferences or just
   the roster).
2. **Agent roster merge:** union keyed by `name`, **deep field-merge** — a
   project agent overrides only the fields it sets; agents unique to either
   layer are kept.
3. **Plain (non-keyed) arrays** (`brain.fallback`, `capabilities`, `args`, …):
   **replace wholesale** — the most specific layer that mentions a list owns it.
4. **Creation / file thinness:** `spur init --global` populates the user layer;
   plain `spur init` writes a **sparse** project file (only what differs from
   `Default ⊕ user`). Each file stores only what it adds over the layers beneath.
5. **Precedence stack:** two file layers only — `Default < user < project`. No
   general env layer and no `SPUR_CONFIG` path override (out of scope). Existing
   `SPUR_TELEGRAM_BOT_TOKEN` / `RUST_LOG` special-cases stay exactly as they are,
   applied after the merge as today.
6. **Tooling:** `spur config check` validates the **merged** config; add a new
   read-only `spur config show` that prints the merged config with per-section
   origin annotations.

## Merge algebra

The complete, memorable rule:

> **Tables deep-merge, scalars override, arrays replace — with
> `agents.entries` the one special case (keyed-merge by `name`, matched
> entries recurse).**

Implemented at the `toml::Value` level (chosen over a typed `Option`-everything
overlay struct, and over section-level wholesale replace):

- Parse each present file into a `toml::Value` table.
- `merge_tables(base, over)`:
  - both tables → recurse key-by-key.
  - key only in one → keep it.
  - scalar/array in `over` → replaces `base`'s value.
  - **special case:** an `agents.entries` array (array-of-tables) → merge by the
    `name` field: union of names; for a name in both, recurse `merge_tables` on
    the two entry tables (so the project entry overrides only the fields it
    sets). Duplicate names *within one file*: last-wins (matches current
    `merge_agents`).
- Deserialize the merged `Value` into `SpurConfig`. "Inherit only what's set"
  falls out for free: absent keys are simply not in the tree and fall through to
  the layer below (and ultimately to `#[serde(default)]`).

**Rejected alternatives:** (B) a parallel `PartialSpurConfig` with every field
`Option<>` + hand-written `Merge` — ~14 sections of boilerplate kept in lockstep
forever, no payoff over the Value approach; (C) section-level replace — cannot
express "inherit `brain.default`, override only `brain.fallback`", contradicting
decisions 2 and 4.

## Architecture

### New module: `crates/spur-acp/src/config/layered.rs`

- `merge_tables(base: &mut toml::value::Table, over: toml::value::Table)` — the
  deep-merge engine with the `agents.entries` keyed special case.
- `load_layered(repo_root: &Path) -> Result<SpurConfig>` — **the single entry
  point.** Resolve user path via `directories::BaseDirs` (consistent with
  existing config code) → `home/.spur/config.toml`; project path
  `repo_root/.spur/config.toml`. Read whichever exist into `toml::Value`,
  `merge_tables(user, project)`, deserialize. All-missing → `SpurConfig::default()`.
- `sparse_diff(config: &toml::Value, baseline: &toml::Value) -> toml::Value` —
  drop every key whose value equals `baseline`'s; for `agents.entries`, keep only
  entries that differ from the baseline's same-named entry (drop entries
  identical to baseline). Used by the write path.
- `effective_with_origins(repo_root) -> (SpurConfig, OriginMap)` (or equivalent)
  — backs `spur config show`: records, per top-level section, whether it came
  from `project`, `user`, or `default`; `agents.entries` annotated per-agent by
  name (origin may be mixed).

### Loader unification

`load_layered` **replaces** all current load paths:

- `spur-cli/src/main.rs::load_config_for_repo` (and its callers at log-init,
  bot, TUI, agents subcommand, `load_orchestrator`).
- `spur-cli/src/commands/pm_ingest.rs` duplicate.
- `spur-cli/src/commands/config_check.rs` project-only loader.

### Write path — sparse, relative to lower layers

Each file stores only what it adds over the layers beneath it.

- `spur init --global` (new flag) → discover agents/preferences, write **sparse
  vs `Default`** to `~/.spur/config.toml`.
- plain `spur init` → write project file **sparse vs (`Default ⊕ user`)**.
  Consequence: in a repo with nothing project-specific and a populated user
  layer, the project file comes out essentially empty (perhaps only
  `[project] name`) — the "clone → it just works" payoff.
- `spur config set [--global]` → switch to a **`toml::Value`-level targeted set**
  (mutate just the one key path, rewrite). **Critical:** the current RMW
  (`update_config`) deserializes→reserializes a fully-defaulted `SpurConfig`,
  which under sparse files would re-expand them to full. A Value-level set
  preserves sparseness. Atomic write (`NamedTempFile` + rename + fsync) unchanged.
- TOML comments are **not** preserved on rewrite — same as today; out of scope.

### CLI surface

- `spur config check` → validate the **merged** config; succeed when only
  `~/.spur` exists, or when only defaults exist (no more hard error on a missing
  project file). Agent validation (`validate_agent_config`) runs over the merged
  roster.
- `spur config show` (new, read-only) → print the merged effective `SpurConfig`,
  each top-level section annotated by origin (`# from project` / `# from user` /
  `# default`); `agents.entries` annotated per-agent by name.
- `spur init --global` (new flag) → write the user layer per the write path.

## Error handling & edge cases

- A malformed file at **either** layer is a hard error naming the offending path
  (no silent fallthrough — surfacing it beats a confusing merge result).
- `agents.entries` with duplicate `name`s within one file: last-wins.
- The `--global` "invisibility" bug (a `config set --global` value the project
  file shadowed) is **resolved** by real layering — the value now shows through
  unless the project explicitly overrides that exact key.
- Home directory unresolvable (`BaseDirs::new()` → `None`): treat the user layer
  as absent (project + defaults only); do not error.

## Testing

`spur-acp` unit tests (merge engine) + `spur-cli` integration tests (CLI).

- Merge algebra: scalar override; table deep-merge; array replace; keyed-agent
  union + per-field override; all-missing → `Default`.
- Precedence: same key set in both layers → project wins.
- `sparse_diff` round-trip: write-then-load reproduces the effective config;
  sparse-vs-baseline emits minimal keys; an empty project delta yields a near-
  empty file.
- `config set` Value-RMW does not re-expand a sparse file to full.
- `config check` succeeds with user-only and defaults-only; fails on a fatal
  agent error in the merged roster.
- Malformed user file and malformed project file each produce a path-named error.
- Per `CLAUDE.md`, config-validation changes run through
  `scripts/spur-cargo test -p spur-acp`.

## Out of scope

- General env-var override layer; `SPUR_CONFIG` explicit-path override.
- TOML comment preservation on rewrite.
- Migrating the theme cascade's direct `$HOME` use onto `directories::BaseDirs`
  (note the inconsistency; leave it).
- A third "system"-level config layer.
