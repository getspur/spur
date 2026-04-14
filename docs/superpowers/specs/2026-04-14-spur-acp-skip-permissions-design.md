# Per-agent skip-permissions for spur-acp

**Date:** 2026-04-14
**Scope:** `crates/spur-acp` (config schema), `crates/spur-core` (spawn/init paths)
**Status:** Design approved; empirically validated via probe

## Problem

Operators want a way to tell SPUR "run this agent in bypass mode" — the
equivalent of `claude --dangerously-skip-permissions` or
`kiro-cli chat -a`. Today SPUR has two permission-related side effects but no
first-class config lever:

1. If `NativeAcpConnection` is constructed with `permission_tx = None`, ACP
   `request_permission` callbacks are silently auto-approved via
   `auto_approve()` in `crates/spur-acp/src/connection/native.rs:1419`. This
   is a side effect of *how the connection happens to be built*, not a
   declared configuration.
2. No mechanism exists to inject agent-specific CLI flags (e.g.
   `--trust-all-tools` for kiro) or to set the claude-code-acp ACP session
   mode to `bypassPermissions`.

We want operators to flip one config switch per agent and have SPUR wire
every relevant bypass mechanism correctly.

## Constraints

- **No runtime CLI override in v1.** Per-session toggles (`spur run --yolo`,
  TUI keybinds) are deliberately out of scope; the chosen schema makes them
  a future extension rather than a refactor.
- **spur-acp stays agent-agnostic.** The transport crate must not grow
  per-agent knowledge. Agent-specific bypass mechanisms are declared in the
  agent's config entry, not hardcoded in Rust.
- **Existing configs must keep working.** All new fields default such that
  omitting them yields today's behavior exactly.

## Design

### 1. Config schema

Three new fields on `AgentConfig` in `crates/spur-acp/src/config.rs`:

```rust
pub struct AgentConfig {
    // …existing fields…

    /// When true, SPUR runs this agent in bypass mode. The three lanes
    /// below are each conditional on this flag being set.
    #[serde(default)]
    pub skip_permissions: bool,

    /// Spawn-time CLI args. When `skip_permissions == true`, these are
    /// appended to `args` before spawn. Use for agents whose bypass is
    /// a command-line flag (kiro-cli `--trust-all-tools`, claude direct
    /// `--dangerously-skip-permissions`).
    #[serde(default)]
    pub skip_permissions_args: Vec<String>,

    /// ACP session mode to set via `set_session_mode` immediately after
    /// `new_session`, when `skip_permissions == true`. Use for agents
    /// that expose bypass as an ACP session mode (claude-code-acp →
    /// "bypassPermissions"). Non-fatal if the agent rejects the mode —
    /// L2 auto-approve still catches any `request_permission` calls.
    #[serde(default)]
    pub skip_permissions_session_mode: Option<String>,
}
```

TOML for the three agents SPUR ships with:

```toml
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
skip_permissions = false                                   # operator opts in
skip_permissions_session_mode = "bypassPermissions"

[[agents.entries]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
skip_permissions = false
skip_permissions_args = ["--trust-all-tools"]

[[agents.entries]]
name = "claude-code"    # deprecated stream-json profile
command = "claude"
args = ["-p", "--output-format", "stream-json", "…"]  # elided
transport = "stream-json"
skip_permissions = false
skip_permissions_args = ["--dangerously-skip-permissions"]
```

### 2. Three bypass lanes

`skip_permissions = true` activates up to three lanes, each declared
per-agent:

- **L1a — Spawn args.** Where `AgentConfig` becomes a `NativeAcpConnection`,
  extend `args` with `skip_permissions_args` when the flag is on. Applies
  uniformly across all transports (`acp`, `stdio`, `cli-wrap`,
  `stream-json`) since they all receive spawn args.

- **L1b — ACP session mode.** After `new_session` returns, if
  `skip_permissions_session_mode.is_some()`, call
  `conn.set_session_mode(SetSessionModeRequest::new(session_id, mode))`.
  Errors are `warn!` + continue; L2 still catches any permission calls.
  Only meaningful on transports that support `set_session_mode` (native
  ACP). Other transports' default trait impl returns "not supported",
  which is the correct no-op.

- **L2 — spur-acp auto-approve.** Pass `permission_tx: None` into
  `NativeAcpConnection::new` when `skip_permissions == true`. Triggers
  the existing `auto_approve()` fast-path (`native.rs:1088–1090`) — zero
  new code in the transport.

  Minor defensive improvement to `auto_approve`: instead of blindly
  picking `options.first()`, prefer the first option whose `kind` starts
  with `allow_`. Falls back to `options.first()` if none found. This
  hedges against a future agent whose first option is rejection;
  verified today that both Claude and Kiro put an allow-class option
  first, so the behavior is unchanged in practice.

### 3. Touchpoints in spur-core

Two sites need updating; the rest falls out naturally.

- **`crates/spur-core/src/orchestrator.rs:875` — seed table for
  `spur init` auto-discovery.** Extend the hardcoded tuple to include
  `skip_permissions_args` and `skip_permissions_session_mode` per agent.
  Seed `skip_permissions` defaults to `false` — the operator opts in by
  editing their config. Unknown agents (`gemini`, `codex`) get empty
  values until their bypass mechanism is understood.

- **Brain/worker spawn paths — wherever `AgentConfig` becomes a
  connection.** Apply the three lanes:

  ```rust
  let mut spawn_args = cfg.args.clone();
  if cfg.skip_permissions {
      spawn_args.extend(cfg.skip_permissions_args.iter().cloned());
  }
  let perm_tx = if cfg.skip_permissions { None } else { Some(tx.clone()) };
  let mut conn = NativeAcpConnection::new(&cfg.name, &cfg.command,
                                           spawn_args, perm_tx);
  conn.initialize(init_req).await?;
  let sess = conn.new_session(cwd, mcp).await?;
  if cfg.skip_permissions {
      if let Some(mode) = &cfg.skip_permissions_session_mode {
          let req = SetSessionModeRequest::new(sess.session_id.clone(),
                                                mode.as_str());
          if let Err(e) = conn.set_session_mode(req).await {
              tracing::warn!(agent=%cfg.name, mode=%mode, err=%e,
                  "skip_permissions: set_session_mode failed; \
                   relying on L2 auto-approve");
          }
      }
  }
  ```

### 4. Out of scope for v1

- **Runtime override.** Per-session toggles (`spur run --yolo`, env vars,
  TUI keybinds) are not implemented. The chosen schema makes them
  additive: future versions can pass an override into the spawn path
  without touching `AgentConfig`.
- **`CLAUDE_CONFIG_DIR` settings-file path.** The claude-code-acp wrapper
  also honors `permissions.defaultMode = "bypassPermissions"` in its
  `settings.json`. We verified session-mode works end-to-end and picked
  the session-mode path to avoid touching the operator's global Claude
  settings.
- **Agents without known bypass mechanism.** `gemini`, `codex`, and any
  future entry with neither `skip_permissions_args` nor
  `skip_permissions_session_mode` fall through to L2-only — every ACP
  permission request is silently auto-approved. Safe default; a louder
  noise ("skip_permissions requested but agent '<name>' has no declared
  mechanism") at startup would be a follow-up hardening.

## Validated assumptions

Before committing to this design we ran a throwaway probe
(`crates/spur-acp/examples/skip_perm_spike.rs`) that spawns each agent,
sends a prompt guaranteed to trigger a file-write tool, and counts the
ACP `request_permission` calls. Every row reproduced the intended
semantics:

| # | Agent | Mode | Permission calls | Notifications | Duration | File written |
|---|---|---|---|---|---|---|
| 1 | claude-code-acp | baseline | **1** | 7 | 19.8 s | ✓ |
| 2 | claude-code-acp | `set_session_mode(bypassPermissions)` | **0** | 7 | 16.8 s | ✓ |
| 3 | kiro | baseline | **1** | 3 | 28.1 s | ✓ |
| 4 | kiro | `acp --trust-all-tools` | **0** | 3 | 17.8 s | ✓ |

Observed permission-option orderings (used to size-check `auto_approve`
defaults):

- Claude (normal): `[allow_always, allow, reject]`
- Claude (ExitPlanMode): `[bypassPermissions?, acceptEdits, default,
  plan]`
- Kiro: `[allow_once, allow_always, reject_once]`

First option is allow-class in every case observed, so
`options.first()` is safe today. The defensive "prefer `kind` starting
with `allow_`" improvement hedges against future agents.

The probe binary lives at `crates/spur-acp/examples/skip_perm_spike.rs`
and is kept in the tree as a permanent diagnostic — mirroring the
existing `compat_spike.rs` pattern. It is not part of the test suite
(no `cargo test` invocation runs it) and it requires a live installed
agent to execute. The testing plan below describes when to re-run it.

## Risk inventory

- **Bypass semantics drift.** Agent vendors may change what their bypass
  flag actually bypasses (e.g. kiro's `--trust-all-tools` today suppresses
  ACP permission calls; tomorrow it might only suppress a subset). L2
  auto-approve is the belt-and-suspenders: even if L1a/L1b quietly
  regress, `permission_tx = None` still auto-approves whatever leaks
  through. Matrix-run the probe on every agent version bump.
- **Running as root.** claude-code-acp's wrapper disables bypass when
  `IS_ROOT && !IS_SANDBOX`. If SPUR ever runs under root, L1b silently
  becomes a no-op and L2 carries the load. Not a correctness problem but
  worth documenting in operator runbooks.
- **Permission options with unexpected first element.** Addressed by the
  defensive `auto_approve` change. If a future agent sends an empty
  options list, auto-approve falls back to `PermissionOptionId::new("allow")`
  as today — no regression.

## Testing plan (post-implementation)

- **Unit tests in `crates/spur-acp`.**
  - Round-trip `AgentConfig` with all three new fields through serde,
    verify defaults.
  - `auto_approve` defensive selection: with options whose first entry
    is `reject_once`, verify an `allow_*` kind is still chosen.
- **Integration test in `crates/spur-core`.**
  - With a fixture `AgentConfig { skip_permissions: true,
    skip_permissions_args: ["--x"], skip_permissions_session_mode:
    Some("bypassPermissions") }`, spy on the spawn call and confirm
    (a) `--x` is appended to args, (b) `permission_tx` is `None`,
    (c) `set_session_mode` is invoked with `"bypassPermissions"` on the
    connection.
  - With `skip_permissions: false`, confirm none of the above fire.
- **Manual matrix.** Re-run the probe binary against production agents
  whenever an agent version (claude-agent-acp, kiro-cli) is bumped. Paste
  the matrix into the bump PR description.

## Open questions

None. All identified empirical claims were either resolved by reading
the claude-agent-acp source or by running the probe.
