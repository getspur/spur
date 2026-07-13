# Fast Mode Status Bar Design

## Problem

SPUR synthesizes a `/fast` command when an ACP session advertises the exact
`fast-mode` configuration option. The command can set that option to `on` or
`off`, and the returned configuration snapshot refreshes the session command
registry. The session detail status bar currently renders mode, model, effort,
and usage, but it does not render the active fast-mode value. Users therefore
cannot confirm the persistent setting without reopening the slash-command UI.

## Scope

Add an explicit `fast:on` or `fast:off` segment to the session detail status
bar whenever the active ACP session advertises a recognized `fast-mode`
selector. Sessions without that option render no fast-mode segment.

The capability gate is the option itself, not a hardcoded agent-kind check.
Codex is currently the agent that advertises `fast-mode`; any future compatible
agent will receive the same behavior automatically.

## Data Model

Add a typed helper in `spur-acp` that derives `Option<bool>` from a
`SessionConfigOption` snapshot:

- `Some(true)` for the exact `fast-mode` option with current value `on`.
- `Some(false)` for the exact `fast-mode` option with current value `off`.
- `None` when the option is absent, is not a select option, or has an unknown
  current value.

Using `Option<bool>` keeps unsupported and malformed states distinct from the
valid disabled state. The status bar must not display an unknown value as
`fast:off`.

## Session State Resolution

`SessionDetailView` resolves fast mode using the same precedence as its model
and effort labels:

1. Prefer the live `session_config_options` snapshot returned by ACP.
2. Fall back to the frozen `SpurAgentCaps.config_options` snapshot captured at
   session creation until live options arrive.
3. Return `None` when neither snapshot contains a recognized value.

The existing successful `session/set_config_option` path replaces the live
snapshot and emits `CommandRegistryDirty`, so the status bar reconciles to the
agent-confirmed value without a separate event type or duplicated state.

This change does not add an optimistic fast-mode override. Failed ACP updates
therefore leave the last confirmed value visible rather than showing an
unconfirmed state.

## Rendering

Extend `StatusBarProps` with `fast_mode: Option<bool>`.

When present, render the following status segment after the model and effort
segments and before usage:

- `fast:on` for `Some(true)`.
- `fast:off` for `Some(false)`.

Render the explicit label in both full and compact layouts. This preserves the
requested state visibility even when the terminal width causes lower-priority
status-bar content to compact. Use an existing status-bar color token rather
than adding theme-wide token requirements for this focused change.

All non-session-detail `StatusBarProps` construction sites pass `None`.

## Error Handling

Unknown option kinds and values are treated as unsupported and hidden. The
existing ACP request failure behavior remains unchanged: no new state is
committed, so the status bar continues to show the last confirmed value.

## Testing

Follow a red-green TDD cycle.

1. Add `spur-acp` unit coverage for `on`, `off`, absent, non-select, and unknown
   `fast-mode` values.
2. Add status-bar rendering coverage proving `fast:on` and `fast:off` appear
   when present and no fast segment appears for `None`.
3. Add session-detail coverage proving frozen capabilities provide the initial
   value and a live config refresh replaces it.
4. Run focused tests through `scripts/spur-cargo`, followed by the complete
   `spur-acp` and `spur-tui` crate test suites and formatting checks.

## Non-Goals

- Changing `/fast` into a bare one-step toggle.
- Adding optimistic fast-mode state before ACP confirmation.
- Showing arbitrary ACP configuration options in the status bar.
- Hardcoding Codex agent-kind checks.
- Changing the slash picker or command description.
