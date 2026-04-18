# Seed Template Migration + Codex Onboarding — Spec 3

**Status:** design
**Date:** 2026-04-14
**Roadmap:** `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md`
**Depends on:** Spec 1 + Spec 2
**Area:** `spur-core` orchestrator · `spur-cli` init · documentation

## Problem

After Specs 1+2 the runtime command pipeline is fully config-driven. But the **bootstrapping** path — `spur init` — still contains the last bastion of hardcoded agent knowledge:

```rust
// crates/spur-core/src/orchestrator.rs:882–958
struct SeedAgent {
    name: &'static str,
    command: &'static str,
    args: Vec<&'static str>,
    transport: TransportKind,
    skip_permissions_args: Vec<&'static str>,
    skip_permissions_session_mode: Option<&'static str>,
}

let known_agents = [
    SeedAgent { name: "kiro", command: "kiro-cli", ... },
    SeedAgent { name: "claude-code", command: "claude", ... },
    SeedAgent { name: "claude-code-acp", command: "npx", ... },
    SeedAgent { name: "codex", command: "codex", ... },
    SeedAgent { name: "gemini", command: "gemini", ... },
];
```

This means:
1. Adding a new seed agent requires touching `orchestrator.rs` — Rust code, not config.
2. The `SeedAgent` struct duplicates `AgentConfig` fields but lacks the Spec 1+2 sub-tables (`commands`, `permissions`, `display`). Generated configs are incomplete.
3. Codex's seed entry has `transport: Acp` but no `[commands]` block — no static commands, no dispatch configuration.
4. No onboarding documentation exists. Adding an agent requires reading source code.

## Goals

1. **Replace hardcoded `known_agents` with an embedded `seed_agents.toml`** — same schema as `config.toml`, parsed at `spur init` time.
2. **Delete `SeedAgent` struct** — the seed template uses `AgentConfig` directly.
3. **Ship a complete codex config** with static commands, proving Spec 2's framework.
4. **Write the onboarding cookbook** — step-by-step guide for adding new agents.

## Non-goals

- New hooks or transport adapters (codex uses existing ACP transport + prompt_text dispatch).
- Runtime config reload.
- `spur init` merge/update behavior (pre-existing UX issue, orthogonal).
- Validating against a live codex binary in CI (manual validation documented).

## Design

### Pillar 1 — Seed template

New file `crates/spur-core/src/seed_agents.toml`, embedded via `include_str!`:

```toml
# Seed agents for `spur init`. Same schema as .spur/config.toml.
# `spur init` parses this, scans $PATH for each command, and writes
# matching entries to .spur/config.toml.
#
# To add a new agent: add an [[agents.entries]] block below. No Rust changes.

# ── kiro ──────────────────────────────────────────────────────────

[[agents.entries]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "kiro"

[agents.entries.commands]
dispatch = "vendor_exec"
exec_method = "_kiro.dev/commands/execute"
args_template = "raw_rest"

[[agents.entries.commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"

[[agents.entries.commands.response]]
method = "_kiro.dev/commands/execute/response"
render = "system_note"

[agents.entries.permissions]
args = ["--trust-all-tools"]

# ── claude-code (stream-json fallback) ────────────────────────────

[[agents.entries]]
name = "claude-code"
command = "claude"
args = ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--permission-mode", "acceptEdits"]
transport = "stream-json"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "claude-sj"

[agents.entries.commands]
dispatch = "prompt_text"

[agents.entries.permissions]
args = ["--dangerously-skip-permissions"]

# ── claude-code-acp ───────────────────────────────────────────────

[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "claude"

[agents.entries.commands]
dispatch = "prompt_text"

[agents.entries.permissions]
session_mode = "bypassPermissions"

# ── codex ─────────────────────────────────────────────────────────

[[agents.entries]]
name = "codex"
command = "codex"
args = ["--acp"]
transport = "acp"
role = "both"
cost_tier = "low"

[agents.entries.display]
handle = "codex"

[agents.entries.commands]
dispatch = "prompt_text"

[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"

# ── gemini ────────────────────────────────────────────────────────

[[agents.entries]]
name = "gemini"
command = "gemini"
args = []
transport = "cli-wrap"
role = "worker"
cost_tier = "low"

[agents.entries.display]
handle = "gemini"

[agents.entries.commands]
dispatch = "prompt_text"
```

### Pillar 2 — init_agents() refactor

Before (~80 lines):

```rust
pub async fn init_agents(&mut self) -> Result<Vec<String>> {
    struct SeedAgent { /* 6 fields */ }
    let known_agents = [ /* 5 entries, ~60 lines */ ];
    let mut found = Vec::new();
    for seed in &known_agents {
        // which + manual AgentConfig construction (~20 lines per agent)
    }
    Ok(found)
}
```

After (~15 lines):

```rust
const SEED_TOML: &str = include_str!("seed_agents.toml");

pub async fn init_agents(&mut self) -> Result<Vec<String>> {
    let seeds: AgentsConfig = toml::from_str(SEED_TOML)
        .expect("embedded seed_agents.toml must parse — checked by test");

    let mut found = Vec::new();
    for seed in seeds.entries {
        let ok = tokio::process::Command::new("which")
            .arg(&seed.command)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            info!(agent = %seed.name, command = %seed.command, "Found agent");
            found.push(seed.name.clone());
            self.registry.register(seed);
        }
    }
    Ok(found)
}
```

Deleted:
- `SeedAgent` struct
- `known_agents` array
- Manual `AgentConfig { name: ..., command: ..., args: ..., ... }` construction

### Pillar 3 — Onboarding cookbook

New file: `docs/spur/agent-onboarding-cookbook.md`

Structure:

```markdown
# Agent Onboarding Cookbook

## Quick start

Add a `[[agents.entries]]` block to `.spur/config.toml`. Run `spur config check`.

## Decision tree

### 1. Choose transport

| Your agent... | Transport |
|---|---|
| Speaks ACP (has `--acp` flag or ACP wrapper) | `transport = "acp"` |
| Emits Claude-style stream-json on stdout | `transport = "stream-json"` |
| Accepts text on stdin, emits text on stdout | `transport = "stdio"` |
| Is a CLI that takes a prompt as trailing arg | `transport = "cli-wrap"` |

### 2. Choose dispatch

| Your agent... | Dispatch |
|---|---|
| Accepts `/slash` commands as prompt text | `dispatch = "prompt_text"` |
| Has a vendor-ext RPC for commands | `dispatch = "vendor_exec"` + `exec_method` |

### 3. Add static commands (optional)

[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"

### 4. Add permissions (optional)

[agents.entries.permissions]
args = ["--trust-all-tools"]     # CLI flag for bypass
session_mode = "bypassPermissions"  # ACP session mode for bypass

### 5. Validate

$ spur config check
✓ my-agent

## Worked example: codex

[full codex config block with annotations]

## Worked example: adding a hypothetical agent

[step-by-step walkthrough for a fictional "my-agent"]

## Adding to the seed template

To make your agent discoverable by `spur init`, add an entry to
`crates/spur-core/src/seed_agents.toml`. No Rust changes needed.
```

### Config example update

`.spur/config.toml.example` updated to match the seed template with added comments explaining each field. This is the user-facing reference.

## Testing

**Compile-time (1 new):**

```rust
#[test]
fn seed_agents_toml_parses() {
    let _: AgentsConfig = toml::from_str(SEED_TOML)
        .expect("seed_agents.toml must parse as AgentsConfig");
}
```

**Unit (3 new):**

1. `init_agents()` with a mock PATH containing only "kiro-cli" returns `["kiro"]` and registers one agent with full Spec 1+2 config (commands, permissions, display populated).
2. `init_agents()` with empty PATH returns empty vec.
3. Codex seed entry has `dispatch = "prompt_text"` and one static command.

**Validation (2 new):**

1. `spur config check` passes for every entry in `seed_agents.toml`.
2. Generated `.spur/config.toml` from `spur init` round-trips through `spur config check`.

## Affected files

| File | Change |
|---|---|
| `crates/spur-core/src/seed_agents.toml` | **Create** — embedded seed template |
| `crates/spur-core/src/orchestrator.rs` | Delete `SeedAgent` struct + `known_agents` array, refactor `init_agents()` to parse embedded TOML |
| `.spur/config.toml.example` | Update with full Spec 1+2+3 schema, all five seed agents |
| `docs/spur/agent-onboarding-cookbook.md` | **Create** — onboarding guide |

## Success criteria

1. `grep -r "SeedAgent" crates/` → zero matches
2. `grep -r "known_agents" crates/` → zero matches
3. `init_agents()` body is ≤20 lines
4. Adding a new seed agent requires only a TOML block — no Rust
5. Codex config has static commands and passes `spur config check`
6. Cookbook exists at `docs/spur/agent-onboarding-cookbook.md`
7. At least 5 agents in seed template (kiro, claude-code, claude-code-acp, codex, gemini)
8. `spur config check` passes for all seed entries

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Codex ACP implementation has quirks | Manual validation step; if a new hook is needed, it's documented as a Spec 4 finding |
| Embedded TOML has a parse error | Compile-time test catches it; build fails |
| `spur init` overwrites existing config | Pre-existing issue; cookbook documents "edit manually if config exists" |
| Seed template grows unbounded | Template is for well-known agents only; custom agents go directly in config.toml |
| claude-code stream-json args drift | Users update args in their config.toml; seed template updated in maintenance PRs |

## Relationship to roadmap success criteria

After Spec 3 ships:

> "At least 4 agents are onboarded (claude, kiro, codex, +1 of opencode/kimi/gemini)."

✅ Five agents: kiro, claude-code, claude-code-acp, codex, gemini.

> "No new 'special-case for agent X' branches have appeared anywhere in spur-tui."

✅ `SeedAgent` struct deleted. `known_agents` deleted. `init_agents()` is generic.

> "The built-in hook count is ≤12."

✅ No new hooks added. Codex reuses `prompt_text` dispatch + static commands.

## What Spec 4+ picks up

- Remaining agents (opencode, kimi-cli) — one spec per agent
- Each proves the framework against a new wire protocol or dispatch shape
- Adds at most one new hook or one new transport adapter
- opencode: possibly needs a new ingest parser (typical case)
- kimi-cli: possibly needs a new transport adapter (worst case)
