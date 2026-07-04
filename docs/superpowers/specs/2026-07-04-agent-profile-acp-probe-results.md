# Agent-profile selection over ACP — live probe results (PoC)

**Date:** 2026-07-04
**Method:** planted namespaced agent definitions (`spur-probe`) in a scratch workspace
(`.claude/agents/spur-probe.md`, `.kiro/agents/spur-probe.json`, `.opencode/agent/spur-probe.md`),
then drove each worker binary over a raw ndjson ACP handshake:
`initialize` → `session/new` (cwd = workspace) → `session/set_config_option` / `session/set_mode`.
Versions probed are the ones pinned in `.spur/config.toml` (NOT `seed_agents.toml`, which is stale).
Gemini excluded (shut down).

## Verification matrix

| kind (version) | agent-def file | discovered? | spawn-time selection | runtime (protocol) selection | model/effort over ACP |
|---|---|---|---|---|---|
| `claude-code-acp` (adapter 0.54.1) | `.claude/agents/*.md` | ✅ listed as values of an `agent` config option | ❌ adapter has no `--agent` flag (source-inspected 0.33.1 + 0.54.1) | ✅ **verified live:** `session/set_config_option {configId:"agent", value:"spur-probe"}` → current `default`→`spur-probe` | ✅ `model` (category `model`) + `effort` (category `thought_level`) |
| `claude-stream-json` (claude CLI 2.1.197, `claude-code-sj` entry) | `.claude/agents/*.md` | n/a | ✅ `--agent <name>` documented on the CLI ("Overrides the 'agent' setting") — not live-tested (API-billed) | n/a | CLI flags |
| `kiro` (kiro-cli 2.10.0) | `.kiro/agents/*.json` | ✅ surfaces as an ACP **mode** | ✅ **verified live:** `acp --agent spur-probe` → `currentModeId: "spur-probe"`; unknown name fails soft to `kiro_default`; also has `--model` / `--effort` spawn flags | ✅ **verified live:** `session/set_mode {modeId:"spur-probe"}` → accepted | ❌ **zero config options advertised** — m11 model/effort override silently no-ops on kiro; spawn flags are the only lever |
| `open-code` (opencode 1.17.11) | `.opencode/agent/*.md` | ✅ listed as values of the `mode` config option (`build`, `plan`, `spur-probe`) | ❌ no `--agent` on the `acp` subcommand | ✅ **verified live:** `set_config_option {configId:"mode", value:"spur-probe"}` → current `build`→`spur-probe` | ✅ `model` + `effort` (category `thought_level`) |
| `codex-acp` (adapters 1.0.2 **and** 1.1.0) | `.codex/agents/*.toml` **exists** (codex subagents, see below) | ❌ planted `.codex/agents/spur-probe.toml` NOT surfaced over ACP: no `agent` config option, no `/agent(s)` slash command in `available_commands_update`, no `_meta` | ⚠️ `-c model="gpt-5.4-mini"` accepted at spawn but advertised current value stayed `gpt-5.5` — do not rely on `-c` | ✅ **verified live:** `set_config_option {configId:"reasoning_effort", value:"low"}` → `xhigh`→`low`. No agent/profile option. | ✅ `model` + `reasoning_effort` (category `thought_level`) |
| `kimi` (1.40.0) | none found | — | none (`acp` takes no options; requires `-y --afk` before `acp` to run) | ❌ | ❌ advertises **no** config options — prompt plane only |

Additional verified facts:

- **Discovery follows the ACP session cwd, not the process cwd.** The claude adapter run with
  process cwd `/tmp` and `session/new cwd = <workspace>` still listed `spur-probe`. This matches
  SPUR's worker flow exactly (process spawned wherever spur runs; session cwd = worktree), and
  means a file overlay materialized into the worker worktree is sufficient for discovery.
- **The 2026-07-04 per-delegation-override design doc's claim "claude-code-acp advertises no
  thought-level option" is stale.** Adapter 0.54.1 advertises `effort` with category
  `thought_level`, which `thought_level_option_from` matches by category — the shipped m11
  effort override works on claude workers now.
- **Codex subagents exist but are invisible over ACP (probed 2026-07-04).** Per
  https://developers.openai.com/codex/subagents, codex defines agents in `.codex/agents/*.toml`
  (project) / `~/.codex/agents/*.toml` (user) with `name`, `description`,
  `developer_instructions`, and optional `model` / `model_reasoning_effort`; built-ins are
  `default`, `worker`, `explorer`. Semantically these are **delegation targets for the main
  thread** — the CLI `/agent` command switches/inspects agent *threads*, it is not a
  "run the main session as X" selector. Live probes of codex-acp 1.0.2 and 1.1.0 with a planted
  `.codex/agents/spur-probe.toml` found no exposure: advertised slash commands are
  `mcp skills status review review-branch review-commit compact goal logout` (+ `$`-skills),
  config options are unchanged, and no `_meta` appears in initialize or session/new.
  Materializing `.codex/agents/*.toml` in worker worktrees is still worthwhile: the bundled
  codex-core may auto-delegate to them mid-task (unverified — needs a billed prompt), and it
  future-proofs for adapter support.
- **`seed_agents.toml` lags `.spur/config.toml`:** seed pins claude adapter 0.33.1 and the
  deprecated `@zed-industries/codex-acp@0.14.0`; live config uses 0.54.1 and
  `@agentclientprotocol/codex-acp@1.0.2` (latest 0.55.0 / 1.1.0 at probe time).

## Design implications

1. **Profile selection is (mostly) a protocol-plane problem, not a spawn-args problem.**
   For claude (`agent` option) and opencode (`mode` option), selecting the file-defined agent is
   just another `session/set_config_option` call — the same RPC m11 already uses, applied at the
   same point in `run_one_worker_attempt`. The in-progress m12 generic `config_overrides` map
   covers both with zero new mechanism: `config_overrides: {"agent": "<profile>"}` (claude) /
   `{"mode": "<profile>"}` (opencode).
2. **Kiro is the inverse:** rich spawn flags (`--agent`, `--model`, `--effort`), zero config
   options. A per-kind spawn-arg mapping is required for kiro — including to make the already-
   shipped m11 model/effort override real on kiro at all. Runtime agent switching via
   `session/set_mode` is available as an alternative that needs no argv change.
3. **Codex has an agent-file concept (`.codex/agents/*.toml`) but no ACP selection surface**;
   its agents are subagent delegation targets, not main-session personas. Model/effort ride the
   protocol plane reliably. Treat the `-c` spawn override as unverified/broken on adapters
   1.0.2–1.1.0. Profile-as-main-agent on codex remains config-profile territory
   (`~/.codex/config.toml` `[profiles]` + CLI `--profile`), which the ACP adapter does not expose.
4. **Kimi supports nothing over ACP config** — profiles degrade to the prompt plane (SPUR owns
   the task prompt via `prompt_text` dispatch).
5. **The worktree file-overlay + git-exclude design remains necessary** as the distribution
   mechanism for the agent definition files themselves (session-cwd discovery, verified), with
   per-worktree `core.excludesFile` guarding `finalize_worker_branch`'s `git add -A` from
   committing injected files into task results.

## Raw evidence

Probe artifacts (scratch, not committed): `/tmp/spur-agent-probe/*.out`, `rt_*.json`,
`probe.sh`, `rt_probe.py`, `mode_probe.py`. Adapter source inspections:
`@agentclientprotocol/claude-agent-acp` 0.33.1 and 0.54.1 dist bundles
(no `--agent` argv parsing; `settingSources: ["user","project","local"]`;
wire handler `session/set_config_option` with params `{sessionId, configId, value}`).
