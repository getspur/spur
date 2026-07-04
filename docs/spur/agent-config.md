# Agent configuration reference

This doc describes the `[[agents.entries]]` schema in `.spur/config.toml`.
It was introduced by Spec 1 (2026-04-14). The shape is additive — old
configs that only set `name`, `command`, `args`, `transport`, etc., keep
working unchanged.

## Top-level fields (unchanged from earlier)

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `name` | String | yes | Unique agent id used by `[brain]` and session routing |
| `command` | String | yes | Binary to spawn |
| `args` | Vec<String> | no | CLI args appended to `command` |
| `transport` | enum | yes | `acp`, `stream-json`, `cli-wrap`, or `stdio` |
| `role` | enum | no | `brain`, `worker`, or `both` (default: `worker`) |
| `capabilities` | Vec<String> | no | Routing tags |
| `cost_tier` | enum | no | `low`, `medium`, `high` (default: `medium`) |
| `rate_limit_window` | Duration | no | e.g. `"5m"` |
| `review` | sub-table | no | Per-agent human-review policy |

## `[agents.entries.display]`

| Field | Default | Notes |
|-------|---------|-------|
| `handle` | lowercase(name) | Short alias for `/handle:cmd` on collision |
| `display_name` | name | Reserved for UX (unused in Spec 1) |

## `[agents.entries.commands]`

| Field | Type | Default | When required |
|-------|------|---------|---------------|
| `dispatch` | DispatchKind | `prompt_text` | always optional |
| `exec_method` | String | — | required when `dispatch = "vendor_exec"` |
| `args_template` | ArgsTemplateKind | `raw_rest` | consulted only for `vendor_exec` |
| `ingest` | [[IngestBinding]] | empty | each binding matches one vendor-ext notification method |
| `response` | [[ResponseBinding]] | empty | each binding matches one vendor-ext response method |

**DispatchKind:** `prompt_text`, `vendor_exec`
**ArgsTemplateKind:** `raw_rest`

### IngestBinding

```toml
[[agents.entries.commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"
```

| Field | Kind | Notes |
|-------|------|-------|
| `method` | String | Full wire method to match |
| `parser` | IngestParserKind | Currently only `json_path_list` |
| `path` | String | Dotted JSON path (no `[…]` indexing) |
| `item_schema` | ItemSchemaKind | Currently only `acp_available_command` |

### ResponseBinding

```toml
[[agents.entries.commands.response]]
method = "_myagent.dev/commands/execute/response"
render = "system_note"
```

**ResponseRenderKind:** `system_note`

#### `[[agents.entries.commands.static]]` — config-declared commands

Static commands appear in the `/` popup at startup, before the agent
connects. Each entry is a triple:

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Command name without leading `/` |
| `description` | yes | Shown in popup |
| `hint` | no | Argument placeholder, e.g. `[model-name]` |

Dispatch is inherited from the parent `[commands]` block — static decls
don't repeat it. For `dispatch = "vendor_exec"`, static commands dispatch
via the same `exec_method` + `args_template` as dynamic ones. When an
agent advertises a command with the same name (via the `ingest` hook),
the dynamic entry overrides the static one on `(handle, name)` match.

#### Response-method convention

For `dispatch = "vendor_exec"`, `[[commands.response]]` methods follow
`{exec_method}/response` — e.g. exec `_myagent.dev/commands/execute`
produces responses at `_myagent.dev/commands/execute/response`. The
orchestrator appends `/response` to the method string when re-emitting
the call result as an `AgentExtNotification`.

## `[agents.entries.permissions]`

Replaces the three flat `skip_permissions*` fields. Old flat fields
still work (promoted transparently by `AgentConfig::effective_permissions`)
but are slated for removal in a future release.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `skip` | bool | false | Enables bypass mode |
| `args` | Vec<String> | empty | Appended to spawn args when `skip = true` |
| `session_mode` | Option<String> | None | ACP session mode set post-new_session when `skip = true` |

Precedence: when both the nested `[permissions]` block and the legacy
flat fields are present, the nested block wins *entirely* if any of its
three fields is non-default. The flat fields are promoted only when the
nested block is fully absent or fully at default values.

## Built-in hook vocabulary

| Hook ID | Kind | Description |
|---------|------|-------------|
| `prompt_text` | DispatchKind | Send `ContentBlock::Text("/cmd args")` on prompt stream |
| `vendor_exec` | DispatchKind | Call a vendor extension RPC |
| `raw_rest` | ArgsTemplateKind | `"/cmd rest…"` → `{ args: { raw: "rest…" } }` |
| `json_path_list` | IngestParserKind | Array at JSON path → items via `item_schema` |
| `acp_available_command` | ItemSchemaKind | Decode each item as `AvailableCommand` |
| `system_note` | ResponseRenderKind | Append trace entry formatted as `⟨handle⟩ response: {params}` |
| `files` | MentionSourceKind | Filesystem file mentions (Spec 2; not wired yet) |

## Validation

On startup, and via `spur config check`, each entry is validated:

- **R1 (fatal):** `dispatch = "vendor_exec"` requires `exec_method`. Agent refuses to start.
- **R3 (warning):** `permissions.skip = true` with no `args` and no `session_mode`. Relies on L2 auto-approve only.

Strongly-typed deserialize covers misspellings: `dispatch = "teleport"`
fails at parse with a clear error listing the accepted variants.

## Running `spur config check`

```
$ spur config check
✓ claude-code-acp
✓ kiro
```

Exit 0 when every entry passes; exit 1 on any fatal error. Warnings
print with a `⚠` prefix but do not flip the exit code.

Useful in CI to catch bad configs before they reach the TUI, and as a
diagnostic when an agent silently fails to start.

## Adding a new agent

For an agent whose behavior matches an existing hook combination,
onboarding is a single `[[agents.entries]]` block. See
`.spur/config.toml.example` for worked examples (claude + kiro).

If the agent exhibits a genuinely novel dispatch/ingest shape, add a
new hook enum variant in `crates/spur-acp/src/config/hooks.rs` plus its
implementation in `crates/spur-tui/src/agents/`, then reference it from
config. That's out of scope for Spec 1; see the roadmap
(`docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md`).

## Delegation descriptor — `[agents.entries.delegation]`

Each worker-capable agent can declare a delegation descriptor that tells the brain what the agent is good at, when to avoid it, and how to shape task prompts for it. Descriptors feed both the brain's system prompt (as a one-liner per agent) and the `list_available_workers` MCP tool (with the full shape).

All fields are optional. Built-in defaults ship for `claude-code-acp`, `kiro`, `codex`, and `gemini`; user values override per-field.

### Example

    [[agents.entries]]
    name = "my-claude"
    command = "claude"
    transport = "acp"

    [agents.entries.delegation]
    description = "Custom claude variant for our auth-flow work."
    tier        = "generalist"             # "specialist" | "generalist"
    good_for    = [
      "auth module refactors",
      "session-state migrations",
    ]
    avoid_for   = ["database schema work"]
    strengths   = ["long-context", "diff-shaped output"]
    limitations = ["no network"]
    input_expectations = "Provide session-state migration doc link in CONTEXT."
    output_shape       = "Unified diff + migration notes."
    inherit_defaults   = true              # default true; false = use user values verbatim

### Field reference

| Field | Role | Where used |
|---|---|---|
| `description` | One-line summary | Workers block in brain prompt, `list_available_workers` |
| `tier` | Specialist/generalist routing hint | Both |
| `good_for` | Positive task patterns | `list_available_workers`; brain routes on |
| `avoid_for` | Soft negative patterns | `list_available_workers`; brain may override with rationale |
| `strengths` | Free-form descriptors | Per-dispatch task prompt only |
| `limitations` | Known failure modes | Per-dispatch task prompt only |
| `input_expectations` | What the brain must supply in CONTEXT | Per-dispatch task prompt only |
| `output_shape` | Shape the worker produces | Brain's EXPECTED_OUTPUT section + `list_available_workers` |
| `inherit_defaults` | Merge with built-in default (default true) | Loader |

### Merge semantics

- **Per-field override:** users replace any subset without restating others.
- **Empty vec inherits (when `inherit_defaults = true`).** Setting `good_for = []` at v1 means "use the built-in default's `good_for`".
- **`inherit_defaults = false`:** user values are used verbatim, including empty vecs. Use when the built-in is genuinely wrong for your setup.

### Validation

spur warns at startup for:
- `good_for`/`avoid_for` entries over 80 chars
- Worker-capable agents with no `description`
- Worker-capable agents with empty `good_for`
- `good_for` entries mentioning a capability (e.g., "plan mode") that isn't declared in `[agents.entries.capabilities]`

Warnings don't block startup.

## Per-delegation profiles

Worker delegations may pass `profile = "<name>"` through `delegate_to_worker`,
`delegate_parallel`, or `submit_plan` task entries. Managed profiles live at
`.spur/agents/<name>.md` in Claude agent-markdown format. When present, SPUR
renders the profile into the worker worktree in the worker kind's native agent
file location and git-excludes that rendered file for the worker worktree only.

The optional `[agents.entries.profile]` config block can override the default
per-kind profile selection strategy when an adapter changes its ACP surface.

### Feature flag

The brain-delegation framework is gated by:

    [brain.delegation]
    framework = "v1"    # "v1" | "legacy"

Defaults: `"v1"` in dev builds (debug_assertions=true); `"legacy"` in release builds at v1 ship. Flag will flip to `"v1"` in release builds at v2, and be removed at v3.

## Command precedence — meta vs conversational

Slash commands come from three sources: spur-local (built into the
TUI), an agent's `[[commands.static]]` config, and runtime
`_<agent>.dev/commands/available` notifications.

Spur defines a small set of **meta-commands** that operate on the
client's view and session lifecycle — `/clear`, `/sessions`, `/help`,
`/quit`, `/mode`, `/cost`, `/vim`. These are client-owned and
**shadow agent-advertised entries with the same name**. If your agent
advertises `/clear`, users will still see spur's built-in `/clear`
(which retires the current brain and lazy-spawns a fresh session on
the next prompt). This is intentional: `/clear` must behave
identically across every brain kind, and the client — not the agent —
owns the view.

Commands that affect the brain's reasoning (e.g. `/compact`,
`/model`, `/undo`) belong in the agent's config and flow through
`prompt_text` or `vendor_exec` dispatch.
