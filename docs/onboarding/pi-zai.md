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

## Troubleshooting

- **`Brain agent 'pi' not found`** — `pi-acp` is not on `$PATH`, or `.spur/config.toml`
  was not updated. Re-run `spur init` or copy the seed block.
- **`pi auth check` is not `ready`** — OpenCode has no `zai-coding-plan` entry, or
  the copy into `~/.pi/agent/auth.json` failed. Run `opencode auth login` and
  pick **Z.AI Coding Plan**, then repeat Step 2.
- **No `/model` in the TUI** — probe `session/new` for a non-empty `model`
  select. Until that is advertised, pin `defaultProvider`/`defaultModel` in
  `~/.pi/agent/settings.json`.
- **MCP tools missing** — `pi-acp` accepts ACP MCP servers but does not forward
  them to Pi. Configure Pi MCP separately if you need it.
