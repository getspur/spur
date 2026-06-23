# SPUR TUI with OpenCode + Z.AI Coding Plan

## Step 1 — Configure OpenCode with Z.AI (one-time, external to SPUR)

1. Subscribe to the [GLM Coding Plan](https://z.ai/subscribe) and create an API key at the [Z.AI API Console](https://z.ai/manage-apikey/apikey-list). Plans from $18/month (Lite) up to Max.
2. Install OpenCode:
   ```bash
   curl -fsSL https://opencode.ai/install | bash
   # or: npm install -g opencode-ai
   ```
3. Authenticate OpenCode with the Coding Plan endpoint:
   ```bash
   opencode auth login      # select "Z.AI Coding Plan" (not plain Z.AI)
   opencode                 # inside the session: /models → pick GLM-5.2 or GLM-4.7
   ```
4. Verify opencode is on `$PATH`: `which opencode`

Optional — enable Z.AI-exclusive MCP servers by adding them to `~/.config/opencode/opencode.json` (or your platform equivalent). These propagate through to SPUR automatically when opencode runs as the brain:

| MCP server | Purpose | Docs |
|---|---|---|
| `@z_ai/mcp-server` (local) | Vision: image/video understanding via GLM-4.6V | [Vision MCP](https://docs.z.ai/devpack/mcp/vision-mcp-server) |
| `web-search-prime` (remote) | Web search | [Search MCP](https://docs.z.ai/devpack/mcp/search-mcp-server) |
| `web-reader` (remote) | Webpage content extraction | [Reader MCP](https://docs.z.ai/devpack/mcp/reader-mcp-server) |

See [Z.AI's OpenCode guide](https://docs.z.ai/devpack/tool/opencode) for the exact MCP config snippets.

## Step 2 — `spur init`

```bash
spur init
```

This is a convergence tool — safe to re-run. It scans `$PATH` for the seed agents (`crates/spur-acp/src/seed_agents.toml`) and merges discovered agents into `.spur/config.toml`. Look for `✓ opencode` in the discovery output.

`spur init` prefers `claude-code` as the default brain if both are installed (`crates/spur-cli/src/commands/init.rs`, `recompute_brain_and_fallback`). When run in a TTY it prompts with a numbered brain picker — select opencode there. In non-interactive runs (CI, `--yes`), set the brain manually:

```toml
# .spur/config.toml
[brain]
default = "opencode"
```

Flags: `--global` writes `~/.spur/config.toml` instead of the repo-local config. `--force` resets the agent list to discovered-only.

## Step 3 — `spur tui`

```bash
spur tui                      # uses brain.default from config
spur tui --brain opencode     # one-shot override
```

All SPUR TUI features work transparently with opencode as the brain: ReAct traces, slash commands, `@mentions`, plan browser, worker delegation, and cost analytics. The opencode subprocess inherits the Z.AI auth and `/models` selection from Step 1, so model choice (GLM-5.2 vs GLM-4.7) is controlled inside opencode, not SPUR.

## Quota notes

GLM Coding Plan quotas are deducted per prompt on a 5-hour rolling window plus a weekly cap. GLM-5.2 and GLM-5-Turbo cost **3× during peak hours** (14:00–18:00 UTC+8) and 2× off-peak; GLM-4.7 is 1× always. For routine work (lint fixes, small edits) switch the model inside opencode to GLM-4.7 to conserve quota; reserve GLM-5.2 for complex refactors. Check usage at [Z.AI Usage Stats](https://z.ai/manage-apikey/subscription).

## Verification checklist

| Check | Command |
|---|---|
| opencode on PATH | `which opencode` |
| OpenCode authed with Z.AI | `opencode` → send a prompt |
| SPUR detected opencode | `spur init` shows `✓ opencode` |
| Brain connection works | `spur tui --brain opencode` → send a message |
| Model visible in TUI | `/model` inside the TUI session |

## Troubleshooting

- **`Brain agent 'opencode' not found in registry`** — `spur init` did not detect opencode on `$PATH`. Re-run `spur init` after installing opencode, or add the entry manually to `.spur/config.toml` matching the seed preset (`command = "opencode"`, `args = ["acp"]`, `transport = "acp"`, `kind = "open-code"`).
- **`opencode acp` exits immediately** — opencode is not authenticated. Run `opencode auth login` and pick "Z.AI Coding Plan" first.
- **Wrong model / quota draining too fast** — model is selected inside opencode, not SPUR. Run `opencode` standalone, use `/models` to switch to GLM-4.7 for routine work, then relaunch `spur tui`.
- **MCP servers not available in SPUR** — MCP servers are configured in opencode's config (`~/.config/opencode/opencode.json`), not SPUR's. The `opencode acp` subprocess loads them automatically.
