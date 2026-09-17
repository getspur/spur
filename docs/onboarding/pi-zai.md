# SPUR TUI with Pi + Z.AI Coding Plan

Reuse an existing OpenCode Z.AI Coding Plan credential with [Pi](https://pi.dev)
through the [`pi-acp`](https://github.com/svkozak/pi-acp) adapter. Do not create
a second API key.

## Step 1 — Install Pi and `pi-acp`

Pinned pair verified 2026-09-17 (`sol_85fe994647ef4427`; pi-acp needs pi ≥ 0.80.4,
Node 22 recommended):

```bash
npm install -g --ignore-scripts @earendil-works/pi-coding-agent@0.85.1
npm install -g pi-acp@0.0.33
which pi pi-acp
pi --version    # 0.85.1
```

## Step 2 — Reuse the OpenCode Coding Plan key

OpenCode stores the plan key at `~/.local/share/opencode/auth.json` under
provider `zai-coding-plan`. Pi reads `~/.pi/agent/auth.json` key `zai`
(or `ZAI_API_KEY`). Copy without printing the secret:

```bash
python3 - <<'PY'
import json, os, stat
from pathlib import Path
src = json.loads((Path.home() / ".local/share/opencode/auth.json").read_text())
key = src["zai-coding-plan"]["key"]
dest_dir = Path.home() / ".pi" / "agent"
dest_dir.mkdir(parents=True, exist_ok=True)
dest = dest_dir / "auth.json"
payload = json.loads(dest.read_text()) if dest.exists() else {}
payload["zai"] = {"type": "api_key", "key": key}
dest.write_text(json.dumps(payload, indent=2) + "\n")
os.chmod(dest, stat.S_IRUSR | stat.S_IWUSR)
print("wrote", dest, "mode", oct(dest.stat().st_mode & 0o777))
PY
pi auth check --provider zai --json
```

Quota-friendly startup model (GLM-4.7 is 1× on the coding plan):

```json
{
  "defaultProvider": "zai",
  "defaultModel": "glm-4.7",
  "defaultThinkingLevel": "off"
}
```

Write that to `~/.pi/agent/settings.json` (merge if the file already exists).

## Step 3 — `spur init`

```bash
spur init
```

Look for `✓ pi`. If you already customized `.spur/config.toml`, append the seed
block by hand instead of re-running init. Seed argv is `command = "pi-acp"`
with empty `args`, `transport = "acp"`, `kind = "pi"`.

## Step 4 — Probe, then TUI

```bash
python3 scripts/probe_acp_capabilities.py \
  --command pi-acp --args "" --label pi --always-approve
spur tui --brain pi
```

The 2026-09-17 handshake advertised `configOptions` `model` and `thought_level`,
so SPUR synthesizes `/model` and `/effort`. Do not add a static `/model` command.
A billed ping (`Reply with exactly: pong`) completed with `stopReason=end_turn`
using the reused OpenCode key.

## Step 5 — SPUR MCP tools (`code_*`, analyst) inside pi

pi has no built-in MCP and pi-acp does not forward ACP `mcpServers`, but pi
can reach SPUR's standalone stdio MCP servers via the
[`pi-mcp-adapter`](https://github.com/nicobailon/pi-mcp-adapter) extension:

```bash
pi install npm:pi-mcp-adapter
```

The extension reads standard MCP config. This repo ships a project-level
`.pi/mcp.json` wiring the SPUR servers (`spur graph mcp` = the 9 `code_*`
tools, `spur analyst mcp`, and the read-only `spur mcp` bundle), so any pi
session started in the worktree gets them automatically — including pi
running as a SPUR brain/worker over pi-acp. Verified 2026-09-17:
`spur exec --agent pi` successfully called `spur-graph_code_symbol_search`.

Servers are lazy: they only spawn on first tool use, so idle context cost is
one proxy tool (~200 tokens), not the full tool surface.

## Troubleshooting

- **`Brain agent 'pi' not found`** — `pi-acp` is not on `$PATH`, or `.spur/config.toml`
  was not updated. Re-run `spur init` or copy the seed block.
- **`pi auth check` is not `ready`** — OpenCode has no `zai-coding-plan` entry, or
  the copy into `~/.pi/agent/auth.json` failed. Run `opencode auth login` and
  pick **Z.AI Coding Plan**, then repeat Step 2.
- **No `/model` in the TUI** — probe `session/new` for a non-empty `model`
  select. Until that is advertised, pin `defaultProvider`/`defaultModel` in
  `~/.pi/agent/settings.json`.
- **MCP tools missing** — two distinct gaps. (a) ACP-forwarded MCP: `pi-acp`
  accepts `mcpServers` but drops them — install the `pi-mcp-adapter` extension
  and use `.pi/mcp.json` (see Step 5) instead. (b) `/model` missing — probe
  `session/new` for a non-empty `model` select; until advertised, pin
  `defaultProvider`/`defaultModel` in `~/.pi/agent/settings.json`.
