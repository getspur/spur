# Experimental Overlay Fsmonitor Configure Design

**Design epic:** `bd-1vt5` (approved and closed)

## Goal

Expose the existing Git built-in fsmonitor probe and exact-fallback path as an explicit, restart-applied `/configure` opt-in without changing the blocked production release decision in `bd-2uzl`.

## User contract

The persisted configuration is:

```toml
[graph]
overlay_fsmonitor = "off" # or "auto"
```

`off` is the default and preserves the current exact status path. `auto` is experimental and intended for local repositories: it probes `git fsmonitor--daemon status`, uses the optimized status command only when Git reports built-in support and a healthy watcher, and otherwise falls back to the exact status command. There is deliberately no forced `on` mode.

The `/configure graph` pane presents `Overlay fsmonitor: Off | Auto (experimental)`, explains that Auto is for local repositories, states that unsupported or unhealthy environments fall back exactly, and states that a restart is required. Saving follows the existing persist-then-apply `ConfigPatch` flow.

## Application model

The setting is restart-applied. The TUI persists and updates its local confirmed configuration after orchestrator acknowledgement, but the already-running graph MCP module is not mutated. New brain MCP callback servers copy the configured mode into `GraphMcpDeps`; graph requests never reread `.spur/config.toml`.

This increment covers the brain MCP path owned by `/configure`. Standalone `spur graph mcp` and delegated-worker MCP servers remain default-Off until they gain an explicit configuration-composition contract; silently rereading repository configuration inside those hot paths is out of scope.

## Safety invariants

- Missing configuration deserializes to Off and default configuration does not serialize a new graph section solely for this field.
- Off always selects exact fallback.
- Auto never forces native routing; it first probes Git and falls back exactly when the daemon is unsupported, unhealthy, or the optimized command/parse fails.
- Existing overlay snapshot correctness validation and retry behavior remain unchanged.
- `bd-2uzl` remains blocked; this feature is an explicit experimental opt-in, not a production-default release.

## Solver evidence

- `sol_bab0ccf390dd4ebd`: target completeness plus Auto→probe/fallback is `sat/pass`.
- `sol_bfef0d6ec3b34286`: the pre-change system fails because schema, typed patch, UI, and restart wiring are absent.
- `sol_bfff64fff6294637`: Auto→forced-native is rejected with `configuration.attribute_allowed_pair.violation`.

POST evaluation must rerun the same configuration rules against the landed implementation facts.
