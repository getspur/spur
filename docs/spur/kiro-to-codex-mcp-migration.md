# Kiro to Codex MCP Migration

This note converts the current `~/.kiro/settings/mcp.json` setup into a
Codex-friendly shape and separates what should stay MCP config from what
should become skills or project docs.

## Current grounded inputs

Source config inspected on this machine:

- `~/.kiro/settings/mcp.json`
- `~/.kiro/powers/installed/*`
- `~/.codex/config.toml`

Current Kiro MCP servers:

| Kiro server | Status | Recommended Codex home |
|---|---|---|
| `sequentialthinking` | enabled | `~/.codex/config.toml` |
| `aws-docs` | enabled | `~/.codex/config.toml` |
| `context7` | enabled | `~/.codex/config.toml`, but sanitize API key first |
| `knowledge-graph` | enabled | `.codex/config.toml` in the repo |
| `fetch` | disabled | omit or keep disabled |
| `github` | disabled | omit or keep disabled |

Draft Codex config: `docs/spur/examples/codex-config.from-kiro.toml`

## Translation rules

### 1. MCP transport config translates directly

Kiro:

```json
{
  "mcpServers": {
    "name": {
      "command": "npx",
      "args": ["-y", "server"],
      "env": {},
      "disabled": false
    }
  }
}
```

Codex:

```toml
[mcp_servers.name]
command = "npx"
args = ["-y", "server"]
enabled = true
```

Direct mappings:

| Kiro | Codex |
|---|---|
| `mcpServers.<name>.command` | `[mcp_servers.<name>].command` |
| `args` | `args` |
| `env` | `[mcp_servers.<name>.env]` |
| `url` | `url` |
| `disabled: true` | `enabled = false` |

Non-direct mappings:

| Kiro | Codex note |
|---|---|
| `autoApprove` | No direct MCP-server equivalent. Handle via Codex approval/sandbox policy, not per-server MCP config. |
| inline secrets in args/env | Replace with placeholders or env forwarding before migration. |

### 2. Powers do not translate to MCP config

Kiro powers bundle three different concerns:

- MCP server connection data in `mcp.json`
- workflow guidance in `POWER.md`
- detailed procedures in `steering/*.md`

Codex best practice is to keep these separate:

- MCP connectivity in `~/.codex/config.toml` or project `.codex/config.toml`
- reusable workflow behavior in skills
- project-specific guidance in `AGENTS.md`, repo docs, or repo-local skills

OpenAI’s current Codex docs explicitly separate MCP configuration from
skills. Codex stores MCP servers in `config.toml`, and skills are a
directory with `SKILL.md` plus optional references/scripts.

## Sanitization decisions

### Context7

Your current Kiro config embeds a live API key in the `args` array.
That should not be copied as-is into any shared file.

Use one of these patterns:

1. Keep a placeholder in a checked-in draft and replace it locally.
2. Prefer a small wrapper script that reads the key from the environment.
3. Keep the server only in `~/.codex/config.toml`, not in a repo file.

### GitHub MCP

Kiro already uses a placeholder token and keeps the server disabled. The
Codex draft preserves that posture and forwards `GITHUB_PERSONAL_ACCESS_TOKEN`
from the shell environment instead of hardcoding it.

### Knowledge graph

`http://localhost:27496/mcp` is machine-local and likely project-local.
That belongs in repo-scoped `.codex/config.toml`, not global user config.

## Recommended split

### User-wide `~/.codex/config.toml`

Put stable personal tools here:

- `sequentialthinking`
- `aws_docs`
- `context7`
- optional `openaiDeveloperDocs`

### Repo-local `.codex/config.toml`

Put workspace-specific tools here:

- `knowledge_graph`
- any future local database/browser/devbox MCP servers tied to this repo

## Equivalent Codex CLI commands

For servers that do not need secrets embedded in the command line, the
same setup can be done with `codex mcp add`:

```bash
codex mcp add sequentialthinking -- npx -y @modelcontextprotocol/server-sequential-thinking
codex mcp add aws_docs --env FASTMCP_LOG_LEVEL=ERROR -- uvx awslabs.aws-documentation-mcp-server@latest
codex mcp add knowledge_graph --url http://localhost:27496/mcp
codex mcp add openaiDeveloperDocs --url https://developers.openai.com/mcp
```

`context7` is the exception in your current setup because the Kiro config
passes the API key inline. Prefer editing `config.toml` with a sanitized
placeholder or wrapping startup in a local script rather than pasting a
live secret into shell history.

## Kiro powers to Codex or Spur skills

Installed Kiro powers found locally:

- `backend-debug-flow`
- `backend-industry-research`
- `frontend-backend-integration-review`
- `frontend-debug-flow`
- `power-builder`
- `quickwit-log-inspector`
- `stripe`
- `terraform`

Recommended mapping:

| Kiro power | Target form | Why |
|---|---|---|
| `backend-debug-flow` | Codex skill or `.spur/skills/.../SKILL.md` | mostly workflow, not transport |
| `backend-industry-research` | Codex skill or Spur skill | mostly methodology and evidence hierarchy |
| `frontend-backend-integration-review` | Codex skill or Spur skill | structured review workflow |
| `frontend-debug-flow` | Codex skill or Spur skill | workflow plus reasoning style |
| `power-builder` | Codex skill | meta-workflow for building reusable powers/skills |
| `quickwit-log-inspector` | split: MCP server + skill | server config is separate from investigation workflow |
| `stripe` | split: remote MCP server + optional skill | the server is real MCP; the best practices belong in a skill |
| `terraform` | split: remote MCP server + optional skill | same pattern as Stripe |

## Spur-specific recommendation

Spur already has a skill loading path:

- bundled skills under `crates/spur-core/src/skills/`
- user overrides under `.spur/skills/<name>/SKILL.md`

That means Kiro power migration should usually target Spur skills, not
new agent config fields, unless the power is truly describing wire
protocol behavior.

Good first conversions:

1. `backend-industry-research` -> `.spur/skills/backend-industry-research/SKILL.md`
2. `frontend-backend-integration-review` -> `.spur/skills/frontend-backend-integration-review/SKILL.md`
3. `quickwit-log-inspector` -> keep MCP in Codex config, move workflow into skill text

## Recommended next steps

1. Copy the user-wide blocks from `docs/spur/examples/codex-config.from-kiro.toml` into `~/.codex/config.toml`.
2. Add a repo-local `.codex/config.toml` with only `knowledge_graph` for this repo.
3. Add `openaiDeveloperDocs` to Codex so OpenAI guidance is available through MCP.
4. Convert one Kiro workflow power into a real Spur skill before migrating more.

## References

- OpenAI Codex MCP docs: <https://developers.openai.com/codex/mcp>
- OpenAI Codex config basics: <https://developers.openai.com/codex/config-basic>
- OpenAI Codex skills docs: <https://developers.openai.com/codex/skills>
