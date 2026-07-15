# Live TUI journey story review

## Changelog for regression — 2026-07-15 (surface contracts)

The eight short probes now follow the session-first launcher contract instead
of waiting for Dashboard `Lineage` after startup. Dashboard, Sessions, palette,
and Explore probes explicitly navigate to and reassert their intended surfaces.
The composer probe now exercises the unsent-draft switch confirmation and
cancels without sending; `agent-send` remains opt-in.

The static story contract now guards every probe against the stale Dashboard
landing assumption and matches VHS proof anchors independently of their timeout
durations. Reliability-only timeout increases therefore remain contract-safe.

---

## Changelog for film — 2026-07-15 (session-first)

**Operator home revised to Session Detail** (`session_detail`), not dashboard:

| Surface | Role in film |
|---------|----------------|
| Session · / INSERT / ReAct | Primary work home |
| Workers Alt+d | Session-scoped delegated work |
| Alt+p | Plan inspector from session |
| Go to hubs | Plans / Issues / Explore |
| Dashboard Lineage | Optional ops overview only |

Journeys call `story_session_land` / `land_session_detail` after cold-start.
Plan-loop seed observes YOU/DELEGATE/workers in session, not dashboard first.
Value tapes open Sessions → `n` then prove `Session ·|INSERT`.

---

## Changelog for film — 2026-07-15 (story arcs)

This pass keeps the landed P0 pacing work and upgrades the five value films to
one explicit **HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION** contract.
The scorecard below is the pre-upgrade baseline.

| Film | Before | After / why it reads as value |
|------|--------|-------------------------------|
| `problem-plan-loop-drive` | Plan and lineage surfaces appeared adjacent | Campaign state now flows into BRAIN→EXEC, output/review tabs, Activity, then an optional cause→effect seed; missing history is labeled |
| `product-e2e-flow` | Session/explore/cascade could feel like three tours | Context continuity now supplies a trusted specialist, ending on strict `agent=` + `model=` + `effort=` dispatch proof |
| `problem-ops-visibility` | Help, palette, and lineage were useful but loosely connected | “What is running?” now distinguishes an empty system from hidden work, then advances through guidance to worker output and Activity when present |
| `problem-plan-progress` | Progress chrome was the main event | Lifecycle, objective, and task summary now answer the next campaign decision; empty state is labeled |
| `problem-backlog-triage` | P0 list and detail flashed as separate surfaces | P0/open/ID isolates the fire, then status/priority explains the response; an empty P0 queue is honest |

Shared helpers now distinguish hard invariant proof from labeled soft
project-dependent proof, preserve non-empty session drafts while finding a
clean composer or safely reuse a completed configured draft, and bind backlog
urgency to one selected detail pane. Tapes
`09`–`13` mirror the same beat order and dwell on the resolution screen. Spend
gates, fast-UAT pacing, Esc→Tab Agents navigation, and slow `@worker` typing are
unchanged. Re-capture remains pending.

---

**Prior review source:** Codex `content-marketer` / `gpt-5.6-sol` / `xhigh`

**Delegation:** `ec9249c9-f72c-48fd-9839-59ac25a59c3d`

**Branch (ephemeral worktree cleaned):** `spur/worker/v2/codex/4be0670bb4346207/eabdcf4c-…`

**Baseline status:** Review-only critique reconstructed from the prior worker; the changelog above records the implementation that followed.

Audience: multi-agent operators evaluating SPUR as a **control plane**. Capture canvas: Air/iTerm **2560×1600**, PTY **200×50** (`geometry.env`).

---

## 1. Executive summary

| | |
|---|---|
| **Overall grade** | **C+ marketing storytelling**, **B regression suite** |
| **Keep** | All five problem stories (`ops`, `plan-progress`, `backlog`, `product-e2e`, `plan-loop`) |
| **Demote** | Seven surface probes + gated `agent-send` → regression / component harvest only |

### Top wins
1. Problem headers already state persona pain → resolution (good contract in `PROBLEM_STORIES.md`).
2. Helpers centralize nav (`story_*`, `navigate_lineage_*`) — pacing can be fixed once in `lib.sh`.
3. Plan-loop journey correctly frames `submit_plan` as **brain MCP**, TUI as **control plane**.

### Top gaps (aligned with product feedback: slower / more value / storytelling)
1. **Loop arc incomplete on film** — observe path rarely correlates full `submit_plan → EXEC → result/review → brain` for non-experts; live seed accelerates playback (`SPUR_AGG_SPEED=2.5`).
2. **Tape 09 / product-e2e** — long cascade without enough dwell on **proof** (`applied`, `agent=`, `model=`, `effort=`); session resume often feels chrome-switchy.
3. **Proof dwell too short at high-res** — most story `sleep_ms` are **0.3–0.6s**; proof panes need **2–3×** readable time.
4. **Soft / broad waits** — many `soft_has_text … || true` allow false-positive “success” without on-screen proof.
5. **Surface probes still in marketing path** — `journeys.conf` interleaves probes with value demos; confuses storyboard priority.

---

## 2. Story scorecard

Scores: Hook / Value density / Pacing (5 = slow enough to read at 2560×1600).

| Journey | Hook | Value | Pace | Verdict | Notes |
|---------|-----:|------:|-----:|---------|-------|
| `problem-ops-visibility` | 4 | 4 | 2 | **Rework** | Strong arc (lineage→help→palette→agents); key spam / short dwell on Agents+stream |
| `problem-plan-progress` | 4 | 4 | 2 | **Rework** | Progress pane is the proof — needs longer hold + caption beat |
| `problem-backlog-triage` | 4 | 3 | 2 | **Rework** | P0 list good; issue detail often flashes |
| `product-e2e-flow` | 5 | 4 | 2 | **Rework** | Best problem statement; must **prove** persona/model/effort on screen |
| `problem-plan-loop-drive` | 5 | 5 | 2 | **Rework (P0)** | Highest strategic value; observe mode incomplete story; seed replay too fast |
| `lineage-dashboard` | 2 | 2 | 3 | **Regression** | Chrome only |
| `sessions-picker` | 2 | 2 | 3 | **Regression** | Absorbed by product-e2e |
| `palette-open` | 2 | 2 | 3 | **Regression** | Absorbed by ops |
| `session-resume` | 3 | 3 | 2 | **Regression** | Absorbed by product-e2e |
| `explore-browser` | 2 | 2 | 2 | **Regression** | Absorbed by product-e2e |
| `explore-agents-tab` | 2 | 2 | 2 | **Regression** | Absorbed by product-e2e |
| `composer-draft` | 2 | 2 | 2 | **Regression** | Safety demo only |
| `agent-send` | 3 | 3 | 3 | **Opt-in / gated** | Real spend; not default marketing |

---

## 3. Pacing playbook (marketing capture only)

Keep UAT **fast** (`soft_has` / short timeouts). Add **capture-mode dwell** via env, e.g. `SPUR_DEMO_STORY_PACE=1` or `SPUR_DEMO_DWELL_MS=…`, so regression UAT does not balloon.

| Beat type | Current typical | Target dwell (film) | Why |
|-----------|-----------------|---------------------|-----|
| Land / Lineage chrome | 0–1.2s | **2.5–3.5s** | High-res density; let eye find BRAIN/EXEC |
| Open overlay (Help, Palette, Plans) | 0.3–1.5s after Wait | **2.0–3.0s** after stable text | Read labels |
| Proof pane (Progress, stream, Gate applied) | 0.4–1.2s | **3.0–4.5s** | This is the “aha” |
| Tree nav (j/k between agents) | 0.3–0.55s per hop | **1.0–1.5s** per hop | Correlate brain vs worker |
| Detail tab switch (stream→attempts→task) | 0.45–0.8s | **1.5–2.0s** each | Operator literacy |
| Before/after action (Start, send, adopt) | often none | **2s before + 3s after** | Cause → effect |
| Live-seed agg speed | **2.5×** default | **1.0–1.25×** for story; idle-limit ok | Stop compressing drama |

Recommended `geometry.env` / capture defaults for story film:

```bash
# Marketing film (not UAT)
: "${SPUR_DEMO_STORY_PACE:=1}"
: "${SPUR_AGG_SPEED:=1.15}"   # was 2.5 — too fast for storytelling
: "${SPUR_AGG_IDLE_LIMIT:=2.0}"
```

---

## 4. Storyboard rewrites (problem stories)

### A. `problem-ops-visibility` (~55–70s film)
| # | Beat | ~s | On-screen proof | Caption |
|---|------|---:|-----------------|---------|
| 1 | Land dashboard | 3 | `Lineage` + `Activity` | “What’s running right now?” |
| 2 | Help | 3 | `Dashboard` help sheet | “Keys exist — not a black box” |
| 3 | Palette hub | 3 | `Go to` list | “One hub to every surface” |
| 4 | Focus Agents tree | 4 | `[Agents]` | “Brain and workers as a tree” |
| 5 | Open worker detail | 4 | `stream` / artifacts | “Inspect the worker, don’t guess” |
| 6 | Return + Activity | 3 | `Activity` events | “Ops timeline is live” |

### B. `problem-plan-progress` (~45–55s)
| # | Beat | ~s | Proof | Caption |
|---|------|---:|-------|---------|
| 1 | Land | 2 | Lineage | Campaign work is multi-task |
| 2 | Open Plans | 4 | `Plans` + `Progress` | “Where is the campaign?” |
| 3 | Select plan | 4 | `Work item` / awaiting | “Awaiting review vs running” |
| 4 | Hold summary | 4 | Tasks list | Decision surface, not chrome |

### C. `problem-backlog-triage` (~40–50s)
| # | Beat | ~s | Proof | Caption |
|---|------|---:|-------|---------|
| 1 | Land | 2 | Lineage | Backlog is drowning me |
| 2 | Issues | 3 | `Issues` | Priority surface |
| 3 | Filter/list P0 | 4 | `P0` + `open` | What’s on fire |
| 4 | Issue detail | 4 | `bd-` labels/status | Triage without leaving TUI |

### D. `product-e2e-flow` (~90–110s) — **must prove cascade**
| # | Beat | ~s | Proof | Caption |
|---|------|---:|-------|---------|
| 1 | Land | 2 | Lineage | Need a specialist, keep context |
| 2 | Sessions switch | 5 | Session titles / history | Context continuity |
| 3 | Explore catalog | 5 | Skills/Agents catalog | Specialists come from the pool |
| 4 | Gate + apply | 6 | **`applied`** | Gate is trust, not friction |
| 5 | @worker cascade | 12 | **`agent=` `model=` `effort=`** | Dispatch precision |
| 6 | Hold compose proof | 4 | Mentions row visible | “Ready to send” (no spend by default) |

### E. `problem-plan-loop-drive` (~75–120s observe; +seed variable)
| # | Beat | ~s | Proof | Caption |
|---|------|---:|-------|---------|
| 1 | Ops orient | 3 | Help / Lineage | Control plane, not a button |
| 2 | Plan browser | 5 | Progress / Start-Resume | Campaigns from submit_plan |
| 3 | Agents: brain row | 4 | BRAIN | Brain owns the loop |
| 4 | Agents: EXEC row | 4 | EXEC / Running | Workers appear here |
| 5 | Detail: stream | 4 | stream | Live output |
| 6 | Detail: attempts/task/review | 6 | tab labels | Full loop literacy |
| 7 | Activity | 3 | brain events | Auto-loop is observable |
| 8 | *(seed)* submit + wait EXEC | var | EXEC appears | Cause → effect |

---

## 5. Priority patch list (implementer-ready)

### P0 — storytelling / trust (do first)
1. **`lib.sh`**: add `story_dwell [seconds]` gated by `SPUR_DEMO_STORY_PACE=1` (default 0 → no-op). Call after every proof `wait_text` / successful `soft_has_text` in `story_*` and `navigate_lineage_*`.
2. **Proof dwell targets** in story helpers (when pace on): land 2.5s, overlay 2.5s, stream/progress 3.5s, cascade proof 4s.
3. **`capture-live-seed.sh` / `geometry.env`**: default `SPUR_AGG_SPEED` **1.15** for story (or separate `SPUR_STORY_AGG_SPEED`); keep 2.5 only if `SPUR_DEMO_UAT_FAST=1`.
4. **`product-e2e` tape + helper**: after cascade, hard-wait (or strict expect) `agent=` **and** `model=` **and** `effort=`; dwell 4s. Do not exit on first partial match alone.
5. **`problem-plan-loop` tape**: after Agents focus, explicit beats for BRAIN then EXEC with 2s dwell each; hold stream 3s; cycle attempts/task/review with captions in tape comments.
6. **`journeys.conf`**: move surface probes under a `# regression only` block; document that marketing `./uat.sh --mode capture` can run `SPUR_DEMO_STORIES_ONLY=1` (optional filter).

### P1 — value density
7. Cut redundant Esc/blur loops when already on Lineage (reduce “key spam” perception).
8. Plan-progress: if `No plans match`, show a **labeled skip beat** (“no campaigns yet — seed path”) instead of silent soft continue.
9. Backlog: dwell on issue detail body, not just list open.
10. Tape Sleep floor: replace sub-second Sleep after proof Wait with **≥2s** on value tapes `09–13`.

### P2 — polish
11. README storyboard one-pager per problem story (caption table above).
12. Optional VO/subtitle sidecar later; comments in tapes are enough for now.
13. Surface probe tapes: leave as-is for CI-adjacent harvest; do not re-film for marketing.

---

## 6. Anti-patterns found → safer alternatives

| Anti-pattern | Where | Safer story alternative |
|--------------|-------|-------------------------|
| Sub-second hop after open | `navigate_lineage_*` 0.3–0.55s | Dwell helper 1–2s per hop when `STORY_PACE` |
| Soft expect `|| true` as “proof” | plan-loop soft BRAIN/EXEC | Soft for UAT **plus** film wait on at least one strong anchor (`[Agents]`, `stream`) |
| Digit keys vs Tab | compose vs Agents | Always document/use **Esc → Tab Agents**; never digit for Agents focus in demos |
| Esc spam / return_to_dashboard loops | blur/return helpers | Max 2 Esc with 0.5s between; prefer known state |
| Uniform 2.5× cast speed | `capture-live-seed` agg | Story speed ~1.0–1.25×; idle trim OK |
| Chrome tourism (open/close) | surface probes in default storyboard | Demote to regression; bond into problem stories only |
| Partial cascade proof | product-e2e | Require all three of agent/model/effort on screen |

---

## 7. Timing evidence (current code)

**Story helpers (film-critical sleeps):**
- `focus_agents_panel`: 0.4s ×2  
- `navigate_lineage_brain_and_workers`: hops **0.3–0.55s**; stream soft wait up to 8s but no post-proof dwell  
- `story_ops_visibility`: Help/palette sleeps **0.3–0.6s**  
- `story_plan_progress`: **0.4–0.5s** between steps  
- `compose_live_worker_cascade`: better (**1.0–1.2s** between cascade steps) but still thin for 2560-wide UI  

**Live seed:**
- `SPUR_AGG_SPEED` default **2.5** in `geometry.env` / capture script — compresses the entire narrative.

**VHS value tapes:** many post-proof `Sleep 400–800ms`; a few good holds (1.2–3s) but inconsistent.

---

## 8. Recommended next action

**Implement P0 only** (story pace helper + tape dwell floors on `09–13` + agg speed for story + stricter product-e2e proofs). Re-capture **problem stories + live seed** at high-res with `SPUR_DEMO_STORY_PACE=1`. Leave surface probes and UAT-fast path unchanged.

Do **not** re-run full high-res “all tapes” until P0 pacing lands — otherwise you re-film the rushed story.

---

## 9. Implementation status (P0 + storytelling upgrade landed)

| Item | Status |
|------|--------|
| `story_dwell` / `story_hop` + `SPUR_DEMO_STORY_PACE` in `lib.sh` | Done |
| Proof dwells wired into story helpers + lineage BRAIN/EXEC beats | Done |
| Stricter cascade proof (`wait_text` agent=/model=/effort=) | Done |
| Value tapes `09`–`13` proof Sleep floors | Done |
| `SPUR_AGG_SPEED` default **1.15**; seed/render enable story pace | Done |
| `SPUR_DEMO_STORIES_ONLY=1` on `render.sh` | Done |
| Five-stage narrative markers in all value journeys | Done |
| Hard proof vs labeled soft-project proof helpers | Done |
| Tapes `09`–`13` aligned to journey beat order and resolution hold | Done |
| Static story/safety/navigation contract (`story-contract.test.sh`) | Done |
| Re-capture high-res film | **Pending** (run when ready) |
