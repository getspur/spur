# Grok Build CLI — ACP capability probe (model / session info)

- **Date:** 2026-07-13
- **Status:** live probe completed (Grok 0.2.93)
- **Related:** research on missing `/model` + non-ACP session metadata when Grok is hosted under SPUR
- **Probe script:** `scripts/probe_grok_acp.py`
- **Unit tests:** `scripts/test_probe_grok_acp.py` (no `grok` binary required)
- **Latest report:** `.spur/logs/probe-grok-20260712T223617.report.json`
- **Latest JSONL:** `.spur/logs/probe-grok-20260712T223617.jsonl`

---

## 1. Goal

SPUR synthesizes mid-session `/model` and `/effort` **only** from ACP
`session/new` → `configOptions` (see `crates/spur-acp/src/adapter/config_options.rs`
and `SpurAgentCaps`). Grok Build’s **interactive TUI** has rich slash commands
(`/model`, `/session-info`, …), but those are pager/shell builtins — not
automatically visible to ACP clients.

This work:

1. Adds a durable, repeatable **ndjson ACP handshake probe** for
   `grok agent stdio`.
2. Records the capability matrix SPUR cares about (model, effort, commands,
   session list/info).
3. Documents expected SPUR UX when Grok under-advertises.

---

## 2. How to run

```bash
# Handshake only (no billed prompt) — default tries set_config_option probes
python3 scripts/probe_grok_acp.py --always-approve

# Optional: short prompt to observe session/update variants
python3 scripts/probe_grok_acp.py --always-approve \
  --prompt "Reply with exactly: pong" --timeout 60

# Also probe legacy session/set_model
python3 scripts/probe_grok_acp.py --try-set-model

# Unit tests for synthesizer prediction helpers
python3 scripts/test_probe_grok_acp.py
```

Artifacts (default):

| Path | Contents |
|---|---|
| `.spur/logs/probe-grok-<ts>.jsonl` | Full send/recv JSON-RPC frames |
| `.spur/logs/probe-grok-<ts>.report.json` | Structured matrix + summaries |

The probe argv matches `seed_agents.toml`:

```text
grok --no-auto-update agent [--model …] [--always-approve] stdio
```

---

## 3. SPUR synthesizer contract (what “have `/model`” means)

| Wire fact | SPUR effect |
|---|---|
| `configOptions` contains select option with `category: "model"` (or id `"model"`) **and** non-empty choices | Slash **`/model`** advertised |
| Select option with `category: "thought_level"` (or id `reasoning_effort` / `effort`) + choices | Slash **`/effort`** advertised |
| Empty / missing `configOptions` | **No** synthesized `/model` or `/effort` |
| `available_commands_update` | Agent-sourced slash names (prompt-plane or client-handled) |
| `session_info_update` / `usage_update` | Status-bar / usage metadata when handlers accept them |
| Model only in proprietary storage (`summary.json`, `_meta.modelId`) | Invisible to synthesizer |

---

## 4. Live matrix

- **Probed:** 2026-07-12T22:36:17Z (UTC) · unit tests `python3 scripts/test_probe_grok_acp.py -v` → **8/8 ok**
- **Binary:** `/Users/kevintruong/.local/bin/grok` (also `~/.grok/bin/grok`)
- **Cmd:** `grok --no-auto-update agent --always-approve stdio` (+ second run with `--try-set-model`)
- **Artifacts:** `.spur/logs/probe-grok-20260712T223608.*` (handshake) · `.spur/logs/probe-grok-20260712T223617.*` (with `--try-set-model`)

| Capability | Result | Notes |
|---|---|---|
| `grok --version` | **`grok 0.2.93 (f00f96316d4b)`** | PATH + `~/.grok/bin` both present |
| `initialize` / `agentCapabilities` | **ok** `protocolVersion=1` | keys: `loadSession`, `promptCapabilities` (`embeddedContext: true`, image/audio false), `mcpCapabilities` (http+sse), `_meta` (hooks, fs_notify). Auth methods: `cached_token`, `grok.com`. `agentInfo=null` |
| `session/new.configOptions` count | **0** | empty / absent — synthesizer gets nothing |
| Model select advertised | **false** | SPUR `/model` **not** synthesized. Model lives in non-configOption planes: `session/new.models` (`currentModelId=grok-4.5`, available: `grok-4.5`, `grok-composer-2.5-fast`), `initialize._meta.modelState`, `_meta.x.ai/sessionConfig` (category `"model"` options) |
| Effort / thought_level advertised | **false** | SPUR `/effort` **not** synthesized. Effort only in model `_meta.reasoningEfforts` (`high`/`medium`/`low`) and `_meta.x.ai/sessionConfig` with **`category: "mode"`** (not `thought_level`) |
| `available_commands_update` | **true** (2×) | Large skill/slash menu (compact, always-approve, context, **session-info**, goal, plugins, many marketplace skills). **No** `/model` or `/effort` command names |
| `session/set_config_option` (advertised) | **n/a** | nothing advertised → no advertised probe |
| `session/set_config_option` (unadvertised) | **`-32601` Method not found** | mid-session config switch path missing |
| `session/set_model` (legacy) | **method accepted, params rejected** | `-32602 Invalid params` / `"unknown model id"` when probing `modelId=grok-build` (probe default). Not `-32601` — method exists but needs a real model id |
| `session/list` / SessionInfo shape | **not probed** | no `sessionCapabilities.list` on initialize; probe skipped. Session detail only via `_meta.x.ai/sessionDetail` on `session/new` |
| Extension methods (`_x.ai/*`, `x.ai/*`) | **yes** | `_x.ai/announcements/update`, `_x.ai/mcp/init_progress`, `_x.ai/mcp/servers_updated`, `_x.ai/settings/update` |

### `matrix` (from latest `*.report.json`)

```json
{
  "config_options_advertised": false,
  "model_select_advertised": false,
  "effort_select_advertised": false,
  "available_commands_advertised": true,
  "modes_advertised": false,
  "spur_slash_model": false,
  "spur_slash_effort": false,
  "extension_methods_seen": [
    "_x.ai/announcements/update",
    "_x.ai/mcp/init_progress",
    "_x.ai/mcp/servers_updated",
    "_x.ai/settings/update"
  ],
  "session_update_variants_seen": [
    "available_commands_update"
  ]
}
```

### Live-run environment notes

- Brain agent `run_terminal_command` could not spawn `/bin/bash` (ENOENT); live probe executed via codex worker with full shell.
- Second run (`--try-set-model`) logged concurrent stderr fatals `Auth(AuthorizationRequired)` / transport closed; probe still exited **0** and wrote a complete report.
- **Confirmed SPUR UX:** no synthesized `/model` or `/effort` for Grok under SPUR until Grok advertises ACP `configOptions` (or SPUR grows a Grok-specific adapter for `models` / `_meta.x.ai/sessionConfig`).

### Pre-live evidence (TUI session storage, 2026-07-13)

Observed on a **native Grok TUI** session under `~/.grok/sessions/…` (not
necessarily identical to `grok agent stdio`, but informative):

| Observation | Implication |
|---|---|
| `updates.jsonl` uses ACP-shaped `session/update` (`agent_message_chunk`, `tool_call`, …) | Conversation stream is ACP-compatible |
| Turn completion via `_x.ai/session/update` / `turn_completed` | Custom extension method (allowed by ACP `_` prefix rule) |
| Model in `summary.json` → `current_model_id`, effort → `reasoning_effort` | **Proprietary index**, not `configOptions` |
| Model also on user chunks as `_meta.modelId` | Not `config_option_update` |
| Official Grok agent-mode docs list stream variants + `x.ai/*` extensions; do **not** document `configOptions` / `available_commands_update` for model | ACP model plane likely thin |

**Live probe confirmed:** Grok sessions under SPUR will **not** show
synthesized `/model` or `/effort`; status bar model label stays empty unless
another path populates it. Model selection remains spawn-time
(`grok agent --model …`) or Grok TUI-local. Proprietary model/effort surfaces
(`session/new.models`, `_meta.x.ai/sessionConfig`) are invisible to the
current synthesizer.

---

## 5. Design implications for SPUR

1. **No SPUR synthesizer bug** when `/model` is missing on Grok — empty
   `configOptions` correctly yields zero advertised model commands.
2. **Do not add fake static `/model`** to `seed_agents.toml` unless a live
   probe shows Grok’s agent interprets `/model …` as prompt text **and**
   that is desirable UX. A static entry without `set_config_option` would
   only send prompt text and confuse users who expect Codex-like switching.
3. **Spawn-time model** remains the reliable lever:
   `args = ["--no-auto-update", "agent", "--model", "<id>", "stdio"]` or
   `grok agent --model <id> stdio`.
4. **Upstream fix (Grok Build):** advertise `configOptions` with
   `category: "model"` / `thought_level` on `session/new`, implement
   `session/set_config_option`, optionally emit `config_option_update` and
   `available_commands_update`. SPUR’s existing path lights up with no
   synthesizer changes.
5. **Probe is the regression gate** when Grok CLI versions ship — re-run
   `scripts/probe_grok_acp.py` and refresh §4.
6. **SPUR read-only status:** model and effort may display from Grok `_meta`
   at session freeze; mid-session switching remains unsupported without standard
   `configOptions`.

---

## 6. Related SPUR code

| Area | Path |
|---|---|
| Caps freeze | `crates/spur-acp/src/spur_agent_caps.rs` |
| `/model` `/effort` synthesis | `crates/spur-acp/src/adapter/config_options.rs` |
| Submit routing | `crates/spur-tui/src/commands/submit_router.rs` |
| Grok seed entry | `crates/spur-acp/src/seed_agents.toml` (`kind = "grok"`) |
| Prior multi-agent probe | `docs/superpowers/specs/2026-07-04-agent-profile-acp-probe-results.md` (Grok was **not** in that matrix) |

---

## 7. Changelog (this delivery)

- Added `scripts/probe_grok_acp.py` — handshake + optional set/list/prompt probes.
- Added `scripts/test_probe_grok_acp.py` — pure helper tests.
- Documented gap + runbook (this file).
- Annotated Grok seed entry and onboarding cookbook with ACP model caveats.
