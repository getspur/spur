# TUI live project — problem stories + harvest

Capture **SPUR TUI on a real project** (this monorepo by default). Demos are
organized as **problem → feature → proof**, not as chrome tours.

See **[PROBLEM_STORIES.md](./PROBLEM_STORIES.md)** for the catalog.

## Problem stories (value demos)

| ID | User problem | Features that solve it | Film |
|----|--------------|------------------------|------|
| `problem-ops-visibility` | “What’s running? How do I drive this?” | Lineage, Activity, Help, Palette | `10-…` |
| `problem-plan-progress` | “Where is my multi-task campaign?” | Plan browser, Progress, summary | `11-…` |
| `problem-backlog-triage` | “What’s P0 open in the backlog?” | Issues list, P0, issue detail | `12-…` |
| `product-e2e-flow` | “I need the right specialist + model/effort.” | Sessions, Explore adopt/gate/pool, `@worker` cascade | `09-…` |
| **`problem-plan-loop-drive`** | “submit_plan loop is a black box — drive brain↔worker.” | Plan browser + lineage Agents tree + detail tabs + activity | `13-…` |

Each story is a continuous shell-use UAT path + optional VHS media.

### Plan loop control plane (`problem-plan-loop-drive`)

```bash
# Observe plan campaigns + navigate lineage brain/worker outputs (safe)
bash journeys/problem-plan-loop-drive.sh

# Start/Resume selected plan (mutates live work)
SPUR_DEMO_ALLOW_PLAN_START=1 bash journeys/problem-plan-loop-drive.sh

# LIVE seed: brain 1-task submit_plan → wait EXEC → re-walk lineage (costs tokens)
SPUR_DEMO_ALLOW_PLAN_LOOP=1 \
SPUR_DEMO_PLAN_LOOP_WAIT_S=240 \
bash journeys/problem-plan-loop-drive.sh

# Self-run + capture cast/gif/mp4 (recommended for live seed)
./capture-live-seed.sh
# → out/14-live-plan-loop-seed.{cast,gif,mp4,log}

# Film (observe path without live seed)
vhs -q tapes/13-problem-plan-loop-drive.tape
```

**Navigate-mode tip:** `Esc` leaves INSERT so `j`/`k` hit the Agents tree.
`Tab` focuses Agents (digit `1` would re-enter Compose).

**Seed prompt** (when `SPUR_DEMO_ALLOW_PLAN_LOOP=1`): asks the brain to
`submit_plan` with exactly one `codex` task (`demo-echo`, reply `ok`, no file
writes), then polls lineage for `EXEC`/`Running` and re-inspects plan browser.

## Surface probes (regression)

Short component captures (`01`–`08`) still exist for lineage, sessions, palette,
explore tabs, composer draft, and gated agent-send. Prefer problem stories for
marketing and product demos.

## Quick start

```bash
export SPUR_BIN="$(command -v spur)"
cd scripts/e2e/demos/tui-live

./uat.sh --list

# Problem-story UAT only (fast value check)
bash journeys/problem-ops-visibility.sh
bash journeys/problem-plan-progress.sh
bash journeys/problem-backlog-triage.sh
bash journeys/product-e2e-flow.sh

# Full UAT + VHS (all rows in journeys.conf)
./uat.sh --mode uat
./uat.sh --mode capture

# Specialist dispatch with real send at the end
SPUR_DEMO_ALLOW_AGENT_SEND=1 bash journeys/product-e2e-flow.sh
```

Media: `out/*.mp4` + `out/*.gif` (gitignored).

## Capture geometry (Mac Air M2 / wide iTerm)

Defaults live in `scripts/e2e/demos/geometry.env` and match a **Liquid Retina
2560×1664** Air with a wide terminal (~200×50), not 720p:

| Setting | Default | Override |
|---------|---------|----------|
| VHS canvas | **2560×1600** | `SPUR_VHS_WIDTH` / `SPUR_VHS_HEIGHT` |
| VHS font | **18** | `SPUR_VHS_FONT_SIZE` |
| shell-use PTY | **200×50** | `SPUR_DEMO_COLS` / `SPUR_DEMO_ROWS` |
| agg cast size | same as PTY | `SPUR_AGG_COLS` / `SPUR_AGG_ROWS` |
| mp4 preview width | 1920 | `SPUR_CAPTURE_PREVIEW_WIDTH` |

```bash
# Re-stamp all demo tapes after editing geometry.env
scripts/e2e/demos/apply-geometry.sh

# One-off taller/sharper capture
SPUR_VHS_WIDTH=2560 SPUR_VHS_HEIGHT=1664 SPUR_VHS_FONT_SIZE=20 \
  SPUR_DEMO_COLS=210 SPUR_DEMO_ROWS=52 \
  ./render.sh
```

## Safety

1. Default landing: `tui --dashboard`.
2. Problem stories are navigation / read-only unless agent-send is enabled.
3. Agent spend only with `SPUR_DEMO_ALLOW_AGENT_SEND=1`.
4. Never deletes project files; not wired into CI `run-all.sh`.

## Authoring a new problem story

1. Write the **problem sentence** (persona pain from RCA/journey docs).
2. List **features that answer it** (bond, don’t bolt-on).
3. Implement `journeys/problem-*.sh` using lib helpers; wait on **proof** strings.
4. Add a media tape + `journeys.conf` row + `PROBLEM_STORIES.md` + `JOURNEYS.md`.
5. Run UAT on a real project before claiming the film.

## Related

- Fixture / first-run: `scripts/e2e/demos/tui-journeys/`
- Journey catalog: `scripts/e2e/JOURNEYS.md`
- Personas: `docs/rca/2026-04-17-persona-journey-review.md`
- Brain/worker journeys: `docs/spur-brain-worker-collaboration.md`
