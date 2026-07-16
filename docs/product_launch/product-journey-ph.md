# Product Hunt product journey — crosswalk to live TUI demos

**Status:** Canonical narrative for PH film + gallery — **approved 2026-07-16**  
**Date:** 2026-07-16  
**Cross-checked against:** [`scripts/e2e/demos/tui-live/`](../../scripts/e2e/demos/tui-live/) (`journeys/`, `journeys.conf`, `PROBLEM_STORIES.md`, `README.md`, `JOURNEY_STORY_REVIEW.md`)  
**PRD:** [`SPUR_PRD.md`](../../SPUR_PRD.md) v2.3  
**Checklist:** [`producthunt-launch-checklist.md`](./producthunt-launch-checklist.md)

---

## 1. Operator model (do not invert)

Live demos and the story contract agree:

| Role | Surface | Notes |
|---|---|---|
| **Home** | **Session Detail** | Composer, ReAct transcript, workers, plan inspector, Go to |
| **Secondary** | Dashboard / Lineage | Ops *overview* only — not where the operator “lives” |
| **Hubs** | Plans / Issues / Explore / Sessions | Reached from Session Detail (Ctrl+K Go to), not as cold start |

```text
Session Detail (home)
  ├── ReAct transcript (YOU / THINK / ACT / DELEGATE)
  ├── INSERT composer (@worker cascade, prompts)
  ├── Inline workers panel (Alt+d)
  ├── Alt+p plan inspector (when plan tracked)
  └── Go to (Ctrl+K) → Plans / Issues / Explore / Sessions

Dashboard / Lineage = optional ops overview
```

**PH anti-pattern (old checklist / May package):** lead with Dashboard lineage tree as “the product.”  
**Correct pattern:** lead with Session Detail as the control plane for brain↔worker work.

---

## 2. Marketing value order (from `journeys.conf`)

Problem stories only — surface probes `01`–`08` are regression, not PH hero film.

| Rank | Journey script | VHS / media stem | User problem |
|---:|---|---|---|
| **1** | `problem-plan-loop-drive.sh` | `13-problem-plan-loop-drive` (+ live seed `14-…`) | “submit_plan loop is a black box — drive brain↔worker.” |
| **2** | `product-e2e-flow.sh` | `09-product-e2e-flow` | “Right specialist + model/effort without losing context.” |
| **3** | `problem-ops-visibility.sh` | `10-problem-ops-visibility` | “What’s running? How do I drive this?” |
| **4** | `problem-plan-progress.sh` | `11-problem-plan-progress` | “Where is my multi-task campaign?” |
| **5** | `problem-backlog-triage.sh` | `12-problem-backlog-triage` | “What’s P0 open in the backlog?” |

Each story uses the beat spine: **HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION**.

### What each story *is not*

| Journey | Not a story about… |
|---|---|
| plan-loop-drive | Dashboard tourism |
| product-e2e-flow | Free-form chat only (it is specialist pool + cascade) |
| ops-visibility | Every panel open/close for its own sake |
| plan-progress | Full auto-merge autonomy |
| backlog-triage | Team-only Issue Browser (Issues are Community) |

---

## 3. Hero journey for Product Hunt (recommended)

### Primary: `problem-plan-loop-drive`

**Why it wins on PH for Orchestrator ICP**

- Matches PRD differentiators: durable plans, workers, human-in-the-loop control plane.
- Explicit note in journey header: `submit_plan` is **brain MCP**; TUI is where you **observe and drive**.
- Highest strategic value in [`JOURNEY_STORY_REVIEW.md`](../../scripts/e2e/demos/tui-live/JOURNEY_STORY_REVIEW.md).

**Resolution the audience should walk away with**

> The operator drives `submit_plan` loops from **Session Detail**: compose, watch ReAct/workers, open plan inspector — campaigns don’t disappear outside the session.

**Optional live seed (real tokens)**

```bash
cd scripts/e2e/demos/tui-live
SPUR_DEMO_ALLOW_PLAN_LOOP=1 SPUR_DEMO_PLAN_LOOP_WAIT_S=240 \
  bash journeys/problem-plan-loop-drive.sh
# or packaged capture:
./capture-live-seed.sh   # → out/14-live-plan-loop-seed.{mp4,gif}
```

Safe default film can stay **observe-only** if seeded history exists; label missing history honestly (demo contract).

### Secondary cut (same 90s film or gallery GIF): `product-e2e-flow`

Shows Explore adopt + `@worker` cascade with `agent=` / `model=` / `effort=` without leaving the session — multi-agent *routing* story without claiming full autonomy.

### Durability beat: `session-resume` (probe)

Resume is **not** the #1 marketing story in `journeys.conf`, but it is the PRD “session immortality” proof. Use as a **gallery frame or 10s cut**, not the whole film, unless you re-film a kill→reopen arc on a live session.

---

## 4. Full journey → PH asset map

| Asset | Primary journey | Media to prefer | Caption seed |
|---|---|---|---|
| Hero video 60–90s | plan-loop-drive | `out/13-…` or `out/14-live-plan-loop-seed…` | Drive multi-agent plans from one session |
| Gallery 1 | Session Detail land | any `09`–`13` open beat | Home is Session Detail |
| Gallery 2 | Workers / DELEGATE | plan-loop | Brain↔worker is visible |
| Gallery 3 | Plans / Progress | plan-progress + plan-loop | Campaign inventory |
| Gallery 4 | Explore + cascade | product-e2e-flow | Specialists without context loss |
| Gallery 5 | Resume | session-resume / product-e2e attach | Session survives restart |
| Gallery 6 | Issues | backlog-triage | Backlog from same surface |
| Gallery 7 (optional) | Lineage overview | ops-visibility secondary | System map |
| Thumbnail | Still of Session Detail with clear hierarchy | crop from hero frame 1 | Readable at 240×240 |

### Surface probes — PH use only as fillers

| Probe | Role |
|---|---|
| `lineage-dashboard` | Optional system map still |
| `sessions-picker` | Multi-session still |
| `palette-open` | Power-user chrome (avoid leading) |
| `explore-browser` / `explore-agents-tab` | Prefer product-e2e story over raw probe |
| `composer-draft` | Safety (draft confirm) — not PH hero |
| `agent-send` | Token spend; not default marketing |

---

## 5. Align PH messaging with journey language

Prefer **problem sentences** from the catalog over abstract architecture slogans:

| Prefer (journey) | Avoid (old PH drafts) |
|---|---|
| “submit_plan loop is a black box” → drive it in session | “Dashboard lineage tree” as the product |
| “specialist + model/effort without losing context” | “AI-powered multi-agent platform” |
| “what’s running where I work?” | Chrome tour of every view |
| “multi-task campaign progress” | Fake `spur cost --today` as hero |
| Session resume as **proof cut** | Resume as only differentiator without plan/worker loop |

PRD one-liners still apply at brand level:

- *One brain, many workers, zero lost context.*
- *Issue in, PR out — across every agent…* (with ACP-compatible honesty)

…but **film order** follows problem stories, not tagline order.

---

## 6. Maker-comment journey paragraph (ship-ready)

```text
What you'll see in the demo: you live in Session Detail — not a dashboard tour.
Compose a plan, watch workers and DELEGATE in the same surface, open plan progress
when the campaign needs inventory, adopt specialists from Explore without losing
the session, and re-attach when you close the laptop.
```

---

## 7. Verification commands before claiming “journey-true” assets

```bash
cd scripts/e2e/demos/tui-live
export SPUR_BIN="$(command -v spur)"

# Static narrative/safety contract
bash story-contract.test.sh

# Value paths (safe defaults)
bash journeys/problem-plan-loop-drive.sh
bash journeys/product-e2e-flow.sh
bash journeys/problem-ops-visibility.sh
bash journeys/problem-plan-progress.sh
bash journeys/problem-backlog-triage.sh

# Marketing film set only
SPUR_DEMO_STORIES_ONLY=1 SPUR_DEMO_STORY_PACE=1 ./render.sh
```

VHS caveats (from demo README): tape `09` needs a recent saved session; `10`/`13` need seeded lineage for full path; `12` needs open P0 for issue detail. Shell-use labels soft proofs when seed is missing — PH copy must not claim seed-dependent moments if the film doesn’t show them.

---

## 8. Checklist delta (what changed after this crosswalk)

| Before (rev 1.1 draft demo) | After (journey-aligned) |
|---|---|
| Hero = Dashboard lineage | Hero = Session Detail + plan-loop drive |
| Review card as primary film beat | Review/workers/DELEGATE in-session; review gate still product truth but not the only film |
| Gallery led by dashboard tree | Gallery led by Session Detail → workers → plans → explore cascade → resume |
| Resume as co-equal 20s hero | Resume as support proof / gallery |
| Surface probes mixed into story | Explicitly demoted for PH |

See [`producthunt-launch-checklist.md`](./producthunt-launch-checklist.md) §3.2–3.3 and §4.2 gallery table.
