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

Each story is a continuous shell-use UAT path + optional VHS media.

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
