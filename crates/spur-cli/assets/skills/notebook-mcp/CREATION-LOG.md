# Creation log — notebook-mcp

## Date
2026-08-07

## Trigger
User requested `/spurpower-solve` evaluation of live notebook MCP tools and a skill under `assets/skills/`.

## Evaluation method
1. Live MCP `search_tool` inventory of the `notebook` server (64 tools).
2. Cross-check against notebook MCP `tools()` registration modules (lifecycle, cell mutation durability, context pack).
3. `notebook_ping` + `notebook_app_briefing` smoke (daemon alive; app briefing hard-gate present).
4. Family partition script: 10 families, 0 uncovered tools.
5. Z3 `solve_constraints` ownership model (persist):
   - `solve_id`: `sol_05c0e55474144d27`
   - `status`: `sat`
   - Model: foundation owns orient/mutate/run/catalog-nav/kernel/lifecycle; specialists own data-app / open-design / jute-deck / app briefing craft; hard gates `context_pack_first`, `expected_version_required`, `writer_session_required`, `no_raw_ipynb_edit`; no dual ownership duplication; `skill_word_budget` ∈ [350,700], `inline_tool_families` ∈ [7,10].

## Gap vs existing skills
| Skill | Scope | Missing for general notebook use |
|---|---|---|
| `notebook-data-app` | Reactive DAG + ports + Deno app | Protocol, lifecycle, writer/version, general edit/run |
| `open-design` | Visual HTML craft loop | Same |
| `jute-deck-mode` | Deck/Present metadata | Same |
| `code-explore` | Repo graph | Notebook-local refs/DAG |

## Deliverable
- `assets/skills/notebook-mcp/SKILL.md` — foundation protocol skill
- `assets/skills/notebook-mcp/references/tool-surface.md` — full 64-tool inventory

## Follow-ups (optional)
- Catalog eligibility / pinning when skills catalog rebuild runs
- Pressure-test with writing-skills subagent scenarios (open without context_pack; mutate without expected_version)
