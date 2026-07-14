# TUI live project — full harvest (UAT + demo capture)

Capture **SPUR TUI on a real project** (this monorepo by default). Unlike Arc A
fixture demos (`demos/tui-journeys/`), this pack uses your real `.spur/`:

| Surface | What you see |
|---------|----------------|
| Lineage | Live brains / execs / activity |
| Sessions | Real TODAY history + resume → transcript |
| Palette | Views / sessions / workers |
| Explore | Synced ecosystem catalog (skills + agents) |
| Composer | Draft text (safe) |
| Agent send | **Opt-in** minimal brain turn (real model spend) |

## Storyboard

| # | Journey | Keys | Anchors | Gate |
|---|---------|------|---------|------|
| 1 | `lineage-dashboard` | launch `--dashboard` | `Lineage`, `Activity`, `INSERT` | safe |
| 2 | `sessions-picker` | `s` | `Sessions`, `TODAY` | safe |
| 3 | `palette-open` | `Ctrl+K` | `Go to`, `esc dismiss` | safe |
| 4 | `session-resume` | `s` → `Down` → Enter | `Session ·`, `INSERT` | safe |
| 5 | `explore-browser` | Ctrl+K → Explore | `synced`, `catalog`, `Skills` | safe |
| 6 | `explore-agents-tab` | explore → Tab | `Agents` | safe |
| 7 | `composer-draft` | type draft | `draft only` | safe (no send) |
| 8 | `agent-send` | type + Enter | `YOU`, `THINK`/`ok` | **`SPUR_DEMO_ALLOW_AGENT_SEND=1`** |
| 9 | **`product-e2e-flow`** | long continuous flow | sessions → explore adopt → `@worker` cascade | safe (+ optional send) |

### Product E2E story (`product-e2e-flow`)

One continuous shell-use / VHS journey (real project):

1. **Land** on lineage dashboard  
2. **Switch sessions** (`s` → pick free rows, recover attach conflicts)  
3. **Explore adopt** — palette → Explore → filter → ★ skill → Tab Agents → ★ agent → Enter gate → `c` accept → Enter apply → pool grows  
4. **Delegate** — attach session → slow-type `@worker:codex` → Tab (profile) → Tab (model) → Tab (effort) → atom like  
   `@worker:codex agent=accessibility-expert model=gpt-5.6-sol effort=low`  
5. **Optional send** — only with `SPUR_DEMO_ALLOW_AGENT_SEND=1`

```bash
# Full product E2E UAT (no model spend on step 5)
./uat.sh --mode uat   # includes product-e2e-flow among others
bash journeys/product-e2e-flow.sh

# Capture the long film
./render.sh   # includes 09-product-e2e-flow when listed in journeys.conf

# With a real delegated send at the end
SPUR_DEMO_ALLOW_AGENT_SEND=1 bash journeys/product-e2e-flow.sh
```

Notes:

- Explore filter defaults to `accessibility` (`SPUR_DEMO_EXPLORE_FILTER`).  
- Worker default is `codex` (`SPUR_DEMO_WORKER`).  
- **Slow typing is required** for `@worker` cascade — machine-speed type is paste-burst suppressed on live TUI.

Session resume skips the most-recent row (`Down`) to avoid “Session attached
in another window” when the top session is held by another TUI.

## Quick start

```bash
export SPUR_BIN="$(command -v spur)"
# optional: SPUR_DEMO_PROJECT=/path/to/other/repo

cd scripts/e2e/demos/tui-live

./uat.sh --list

# Safe harvest (no model spend)
./uat.sh --mode uat
./uat.sh --mode capture

# Full harvest including a real brain ping (costs tokens)
SPUR_DEMO_ALLOW_AGENT_SEND=1 ./uat.sh
```

Media lands in `out/*.mp4` + `out/*.gif` (gitignored).

## Safety

1. Default landing: `tui --dashboard` (no auto-resume of last chat).
2. Journeys 1–7 never submit a brain/worker turn.
3. Journey 8 is gated; it sends only:
   `demo capture ping — reply with only the word ok`
4. No temp isolation, no `rm -rf` of the project.
5. Opt-in only — not wired into CI `run-all.sh`.

## Related

- Fixture / first-run: `scripts/e2e/demos/tui-journeys/`
- Journey catalog: `scripts/e2e/JOURNEYS.md`
