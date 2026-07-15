# TUI live project — problem stories + harvest

Capture **SPUR TUI on a real project** (this monorepo by default). Demos are
organized as **problem → feature → proof**, not as chrome tours.

See **[PROBLEM_STORIES.md](./PROBLEM_STORIES.md)** for the catalog.

## Problem stories (value demos)

| ID | User problem | Features that solve it | Film |
|----|--------------|------------------------|------|
| **`problem-plan-loop-drive`** | “submit_plan loop is a black box — drive brain↔worker.” | Plan browser + lineage Agents tree + detail tabs + activity | `13-…` |
| `product-e2e-flow` | “I need the right specialist + model/effort without losing context.” | Sessions, Explore adopt/gate/pool, `@worker` cascade | `09-…` |
| `problem-ops-visibility` | “What’s running? How do I drive this?” | Lineage, Activity, Help, Palette | `10-…` |
| `problem-plan-progress` | “Where is my multi-task campaign?” | Plan browser, Progress, summary | `11-…` |
| `problem-backlog-triage` | “What’s P0 open in the backlog?” | Issues list, P0, issue detail | `12-…` |

Each story is a continuous shell-use UAT path + optional VHS media.

### What a viewer should learn

Every value film follows **HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION**.
Shell-use prints those beats; tapes mirror them in comments and hold the same
proof screens. Invariant anchors fail loudly. Project-dependent evidence such
as existing BRAIN/EXEC history, campaign rows, or open P0 issues becomes a
labeled soft beat when absent—never silent “success.”

The five rows above are ordered by marketing value. Surface probes remain
short regression harvests and do not inherit the longer story treatment.

The shell-use journeys branch safely on empty projects and print labeled soft
proof. VHS cannot branch: tape `09` requires a recent saved session, tapes `10`
and `13` require seeded lineage, and tape `12` requires an open P0 to reach
issue detail. Those tapes stop before a dangerous key action when their seed is
missing; capture value films against a project with the matching history.

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
bash journeys/problem-plan-loop-drive.sh
bash journeys/product-e2e-flow.sh
bash journeys/problem-ops-visibility.sh
bash journeys/problem-plan-progress.sh
bash journeys/problem-backlog-triage.sh

# Static narrative/safety/navigation contract
bash story-contract.test.sh

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
| agg speed | **1.15** (story) | `SPUR_AGG_SPEED` (use `2.5` for fast UAT cast review) |
| mp4 preview width | 1920 | `SPUR_CAPTURE_PREVIEW_WIDTH` |

```bash
# Re-stamp all demo tapes after editing geometry.env
scripts/e2e/demos/apply-geometry.sh

# One-off taller/sharper capture
SPUR_VHS_WIDTH=2560 SPUR_VHS_HEIGHT=1664 SPUR_VHS_FONT_SIZE=20 \
  SPUR_DEMO_COLS=210 SPUR_DEMO_ROWS=52 \
  ./render.sh
```

## Story pacing (slower, value-first film)

Shell-use UAT stays **fast** by default. Narrative labels and proof checks run
in both modes; only the marketing dwell is gated:

| Env | Effect |
|-----|--------|
| `SPUR_DEMO_STORY_PACE=1` | Enables `story_dwell` / longer hops in `lib.sh` (default **on** for `./capture-live-seed.sh` and `./render.sh`) |
| `SPUR_DEMO_DWELL_SCALE=1.2` | Multiplies dwell seconds |
| `SPUR_DEMO_STORIES_ONLY=1` | VHS only the five problem stories (`09`–`13`), skip surface probes |
| `SPUR_AGG_SPEED=1.15` | Cast→gif story speed (was 2.5; flattened narrative) |

```bash
# Value-story film only (recommended marketing path)
SPUR_DEMO_STORIES_ONLY=1 ./render.sh

# Live seed with readable high-res pacing
./capture-live-seed.sh

# Fast functional UAT (no film dwell)
SPUR_DEMO_STORY_PACE=0 bash journeys/problem-ops-visibility.sh
```

Full critique + storyboard: **[JOURNEY_STORY_REVIEW.md](./JOURNEY_STORY_REVIEW.md)**.

## Operator home: Session Detail

Value films center on **Session Detail** (`crates/spur-tui/src/views/session_detail`):

| Surface | Role |
|---------|------|
| Session · + INSERT + ReAct | Primary work (compose, watch brain, cascade) |
| Workers (Alt+d) | Delegated work for this session |
| Alt+p | Plan inspector when a plan is tracked |
| Ctrl+K Go to | Plans / Issues / Explore / Sessions hubs |
| Dashboard / Lineage | Optional ops overview only |

Shell-use: `start_live_tui` cold-starts `--dashboard` then `land_session_detail`.  
VHS tapes open Sessions → `n` after launch for a reliable attach.

## Safety

1. Cold start: `tui --dashboard`, then enter **Session Detail** as home.
2. Ops, plan, and backlog stories are observe-only. `product-e2e-flow` applies the selected Explore skill/agent to the local pool, but never sends by default.
3. Model/worker spend requires `SPUR_DEMO_ALLOW_AGENT_SEND=1` or `SPUR_DEMO_ALLOW_PLAN_LOOP=1`.
4. Plan mutation requires `SPUR_DEMO_ALLOW_PLAN_START=1` or the live loop gate.
5. Never deletes project files; not wired into CI `run-all.sh`.

## Authoring a new problem story

1. Write the **problem sentence** (persona pain from RCA/journey docs).
2. List **Session Detail features that answer it** (bond, don’t bolt-on).
3. Implement `journeys/problem-*.sh` using `story_session_land` + lib helpers; wait on **proof** strings.
4. Add a media tape that attaches Session Detail + `journeys.conf` row + `PROBLEM_STORIES.md`.
5. Run `bash story-contract.test.sh`, then UAT on both an empty and seeded project before claiming the film.

## Related

- Fixture / first-run: `scripts/e2e/demos/tui-journeys/`
- Journey catalog: `scripts/e2e/JOURNEYS.md`
- Personas: `docs/rca/2026-04-17-persona-journey-review.md`
- Brain/worker journeys: `docs/spur-brain-worker-collaboration.md`
