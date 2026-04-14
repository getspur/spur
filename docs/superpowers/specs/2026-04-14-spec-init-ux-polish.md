# Init UX Polish — Spec

**Status:** design
**Date:** 2026-04-14
**Roadmap:** `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md`
**Depends on:** Spec 3
**Area:** `spur-cli` cmd_init · `spur-acp` config schema · cookbook + example docs

## Problem

Post-Spec-3, `spur init` detects agents and writes a config-complete `.spur/config.toml` — but the terminal output gives users zero guidance about next steps. The current output:

```
[spur] Scanning for agents...
[spur] Found 2 agents:
  - kiro
  - claude-code
[spur] Config written to ./.spur/config.toml
[spur] Initialized.
```

Journey review (see MCTS analysis in commit log) identified drop-off cliffs for all 5 engineer personas: novices don't know how to install missing agents or what to run next; enterprise users see `--trust-all-tools` without knowing it's safety-default; returning users lose customizations to silent overwrite; power users can't see the brain/fallback chain.

Spec 4+ adds more agents. That fills the catalog but doesn't move the funnel metric. This spec fixes the funnel.

## Goals

1. **Install hints surface in init output** — each missing agent prints its install command.
2. **Setup summary + next-step block** appear after successful config write.
3. **Overwrite guard** — detect existing `.spur/config.toml` and require `--force`.
4. **Empty-path does not write config** — treat "zero agents found" as a no-config outcome, not a lie.

## Non-goals

- `spur agents add` subcommand (separate spec).
- `spur config show --resolved` subcommand (separate spec).
- Interactive prompts (keep init non-interactive; flags only).
- Version-checking / upgrade suggestions for installed agents.

## Design

### Pillar 1 — Install hints in the schema

Add `install_hint: Option<String>` to `AgentConfig` (or equivalently to a sibling field on `DisplayConfig` — design decision below).

Evaluated three placements:
- (a) Top-level field on `AgentConfig` — simplest; one line added to the struct; every user's generated config carries the hint verbatim.
- (b) Nested under `DisplayConfig` — logically cohesive (hints are UX); users rarely need to see them in generated configs.
- (c) Sidecar table keyed by agent name in `seed_agents.toml` that doesn't round-trip into `.spur/config.toml` — purest separation; zero pollution of user configs; most implementation work.

**Decision: (b) — nest under `[display]`.** It's the right semantic home (hints are for humans, not the runtime), and avoids bloating top-level `AgentConfig` which already has 14 fields. The hint WILL round-trip into user configs — acceptable, since users can delete the line if they want.

Concretely:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayConfig {
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Human-readable command to install this agent. Surfaced by
    /// `spur init` when the agent is in the seed template but not on
    /// $PATH. Keep terse (one line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
}
```

Seed template additions (examples):

```toml
[agents.entries.display]
handle = "kiro"
install_hint = "brew install kiro-cli"
```

Install hints for the 5 seed agents (best-effort, should be verified against current installers):

| agent | hint |
|---|---|
| kiro | `brew install kiro-cli` |
| claude-code | `npm install -g @anthropic-ai/claude-code` |
| claude-code-acp | `npm install -g npx` (npx is the entry point; the package pins via `npx --yes …`) |
| codex | `https://docs.openai.com/codex/install` |
| gemini | `npm install -g @google/gemini-cli` |

All are best-guesses — implementer validates against each project's actual install docs during Task 1.

### Pillar 2 — `cmd_init` output rewrite

Three output paths:

**Path A: ≥1 agent found, no existing config.** Write config + print found/skipped/summary/next-step blocks.

**Path B: 0 agents found, no existing config.** Do NOT write config. Print install hints + cookbook reference.

**Path C: existing `.spur/config.toml` detected.** Do NOT write. Print guidance directing to `--force` or manual merge.

`--force` flag on the Init subcommand causes Path C to fall through to Path A (or B).

### Pillar 3 — Output specification

#### Success output (Path A)

```
[spur] Scanning agents on $PATH...

Found 3 of 5 known agents:
  ✓ kiro              kiro-cli acp                                    (brain + worker)
  ✓ claude-code       claude -p --output-format stream-json ...       (brain + worker)
  ✓ gemini            gemini                                          (worker)

Skipped 2 (not installed):
  ✗ claude-code-acp   install: npm install -g npx
  ✗ codex             install: https://docs.openai.com/codex/install

Config written to .spur/config.toml.

Setup summary:
  Brain agent:      claude-code
  Brain fallbacks:  kiro
  Permissions:      safety-default (bypass disabled for all agents)
  Session logs:     .spur/logs/
  Cost tracking:    enabled — run `spur cost` after your first task

Next step: try one of
  spur run "describe the repo in 3 bullets"
  spur watch                                   # interactive TUI
  spur agents show <name>                      # see full config for one agent
  spur config check                            # validate your setup
```

Width constraints: truncate the `command + args` column at 60 chars with trailing `...` to avoid wrapping on 80-column terminals.

#### Empty-path output (Path B)

```
[spur] Scanning agents on $PATH...

No agents found. Install at least one:
  kiro-cli          brew install kiro-cli
  claude            npm install -g @anthropic-ai/claude-code
  npx               npm install -g npx   (then re-run `spur init` for claude-code-acp)
  codex             https://docs.openai.com/codex/install
  gemini            npm install -g @google/gemini-cli

No config written. Re-run `spur init` after installing an agent.

To use a custom agent spur doesn't know about yet:
  docs/spur/agent-onboarding-cookbook.md
```

#### Overwrite-guard output (Path C)

```
[spur] .spur/config.toml already exists with 2 entries.
[spur] To overwrite (losing any customizations), run `spur init --force`.
[spur] To re-scan without writing, run `spur agents detect`.
```

Note: `spur agents detect` does not exist yet. The guidance still serves — it tells the user that re-scanning without writing is the goal, and documents the intended future subcommand. Alternative: drop that line. Leaving it in per the principle "tell users the right answer, even if we haven't built it yet" — makes the future subcommand's purpose obvious when it ships.

### Pillar 4 — `--force` flag

Extend `Commands::Init` to `Commands::Init { #[arg(long)] force: bool }`. Pass through to `cmd_init`.

### Pillar 5 — Setup summary derivation

The summary block is derived from the just-written config. Helper function:

```rust
fn print_setup_summary(cfg: &SpurConfig) {
    println!();
    println!("Setup summary:");
    println!("  Brain agent:      {}", cfg.brain.default);
    if !cfg.brain.fallback.is_empty() {
        println!("  Brain fallbacks:  {}", cfg.brain.fallback.join(", "));
    }
    let any_bypass = cfg.agents.entries.iter()
        .any(|a| a.effective_permissions().skip);
    let bypass_line = if any_bypass {
        let agents: Vec<_> = cfg.agents.entries.iter()
            .filter(|a| a.effective_permissions().skip)
            .map(|a| a.name.as_str())
            .collect();
        format!("bypass enabled for: {}", agents.join(", "))
    } else {
        "safety-default (bypass disabled for all agents)".to_string()
    };
    println!("  Permissions:      {}", bypass_line);
    println!("  Session logs:     .spur/logs/");
    println!("  Cost tracking:    enabled — run `spur cost` after your first task");
}
```

The bypass line adapts: "safety-default" when all agents have `skip = false` (the post-Spec-3 default); lists enabled agents if any have opted in. Critical for the P5 enterprise persona.

## Testing

**Unit (2 new):**

1. `install_hint` deserialize: seed template parse sets hint for kiro = `"brew install kiro-cli"`.
2. Seed template has hints for all 5 agents (contract test — forces maintainers to keep hints current when adding seeds).

**Integration (4 new):** Use the `tempfile + $PATH + stub_binary` pattern from `crates/spur-core/tests/init_agents.rs`.

1. `init_with_zero_agents_writes_no_config`: empty PATH → `.spur/config.toml` does NOT exist after call.
2. `init_with_existing_config_requires_force`: pre-create `.spur/config.toml`, run init without `--force`, assert file unchanged + exit code reflects guidance path.
3. `init_with_force_overwrites_existing_config`: pre-create config, run `init --force`, assert file rewritten.
4. `init_output_contains_next_step_block`: happy path with kiro stubbed, captures stdout via `assert_cmd` or by redirecting print! to a buffer, asserts substring matches for `Setup summary:`, `Next step:`, `spur run`, `spur watch`.

**Docs review:** eyeball that `docs/spur/agent-config.md` describes `install_hint` and the cookbook mentions seed-template hint contribution.

## Affected files

| File | Change |
|---|---|
| `crates/spur-acp/src/config/entries.rs` | Add `install_hint: Option<String>` to `DisplayConfig` |
| `crates/spur-acp/src/seed_agents.toml` | Add `install_hint` for all 5 agents |
| `crates/spur-cli/src/main.rs` | Extend `Commands::Init` with `--force`; rewrite `cmd_init` |
| `crates/spur-cli/tests/init_ux.rs` | **Create** — 4 integration tests |
| `crates/spur-acp/src/config/mod.rs` (tests) | 2 unit tests for hint deserialization |
| `docs/spur/agent-config.md` | Document `install_hint` field under DisplayConfig |
| `docs/spur/agent-onboarding-cookbook.md` | Mention install_hint in the "contribute to seed template" section |
| `.spur/config.toml.example` | Carry install_hint lines as reference |

## Success criteria

1. `spur init` in a fresh dir with zero agents prints the empty-path output AND does NOT create `.spur/config.toml`.
2. `spur init` in a fresh dir with ≥1 stubbed agent prints the 5 blocks (scanning / found / skipped / written / summary / next-step) and creates `.spur/config.toml`.
3. `spur init` with existing `.spur/config.toml` and no `--force` exits without writing.
4. `spur init --force` with existing config overwrites.
5. Every seed agent in `seed_agents.toml` has a non-empty `install_hint` (contract-tested).
6. Integration tests all pass.
7. Novice smoke test (manual): dev with empty repo, no agents installed, runs `spur init` — output mentions at least one agent with a copy-pasteable install command; after install, re-running init reaches "Next step" with ≥1 runnable command.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Install hints go stale | Best-effort at creation; low maintenance cost (5 lines); cookbook steers contributors to update when they add a seed |
| Output format churn breaks integration tests on every cosmetic edit | Assert on substrings (`Setup summary:`, `Next step:`, agent names) not full text |
| `--force` makes destructive overwrite easy to footgun in CI | Acceptable — CI workflows should use `--force` intentionally; the default is safe |
| `install_hint` URLs drift (codex) | Prefer package-manager commands over URLs where possible; URL fallback for codex is explicit and can be updated |
| Setup summary lies about log path if `.spur/logs/` is never created | The directory is created on first agent spawn; if users never run anything, the path is a promise, not a file. Acceptable. |
| Width truncation of command column distorts long commands (claude-code's 7-arg list) | Truncate to 60 chars with `...` suffix; print the full command in `spur agents show <name>` |

## Relationship to roadmap success criteria

> "Time-to-first-prompt for a new user is under 5 minutes on a clean machine."

This spec is the load-bearing step toward that metric. Without install hints and next-step guidance, the floor is ~20 minutes (read docs, install agent, guess at subcommand). With them, the floor drops to the agent install time (~1–2 min) plus the "Next step" copy-paste (~5 seconds).

## What comes after

- `spur config show --resolved` — audit subcommand for P5 enterprise. Own spec.
- `spur agents add <name>` — interactive or flag-driven custom agent onboarding. Own spec.
- `spur agents detect` — the subcommand referenced in Path C overwrite guard. Tiny spec.
