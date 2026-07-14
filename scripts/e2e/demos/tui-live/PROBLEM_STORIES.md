# Live TUI problem stories

Each **problem story** is a continuous UAT + VHS journey on a real project.
Features are not demos of chrome — they answer a concrete pain.

| ID | Persona pain | Features exercised | Proof anchors |
|----|--------------|--------------------|---------------|
| **`problem-plan-loop-drive`** | “submit_plan auto-loop is a black box — brain↔worker outputs, how do I drive it?” | plan browser (campaigns), lineage Agents j/k/Enter, detail tabs (stream/attempts/task/review), activity log; optional brain kick / plan start | `Progress`, `[Agents]`, `stream`, `Activity`; `BRAIN`/`EXEC` when history exists |
| `product-e2e-flow` | “I need a specialist persona + model + effort without reinventing agents or losing context.” | sessions, Explore adopt/gate/pool, `@worker` cascade | `TODAY`, `applied`, `agent=`, `model=`, `effort=` |
| `problem-ops-visibility` | “I can’t see what’s running or how to drive multi-agent work.” | lineage, activity, help, palette, **Agents tree focus + detail tabs** | `Lineage`, `Dashboard`, `Go to`, `stream`, `Activity` |
| `problem-plan-progress` | “Where is my multi-task campaign? What’s awaiting review?” | plan browser, summary pane, navigate plans | `Progress`; running/awaiting/complete and `Work item`/`Tasks` when present |
| `problem-backlog-triage` | “What’s on fire in the backlog?” | issue browser, P0 list, issue detail | `Issues`; `P0` + `open` + `bd-`, then `status:`/`priority:` when present |

## Contract

1. **Lead with problem** in journey header comments and README.
2. **Bond features to the problem** — every beat must move the user toward resolution.
3. **Prove with wait strings** that the user saw the answer (not just that a view opened).
4. **Safe by default** — no model/worker spend or plan mutation unless the
   matching `SPUR_DEMO_ALLOW_AGENT_SEND`, `SPUR_DEMO_ALLOW_PLAN_LOOP`, or
   `SPUR_DEMO_ALLOW_PLAN_START` gate is `1`.
5. **Reuse lib helpers** — no fork of isolation/fixtures.

### Beat and proof contract

Every value journey follows the same readable spine in both shell-use logs and
its matching tape:

1. **HOOK** — name the operator pain.
2. **ORIENTATION** — establish where the answer lives.
3. **ACTION** — exercise the feature that changes the situation.
4. **PROOF** — stop on a visible anchor long enough to read it in story pace.
5. **RESOLUTION** — restate the solved operator outcome, not merely “complete.”

`story_hard_proof` owns invariant UI anchors and fails UAT when they disappear.
`story_soft_proof` owns project-dependent history/state; it prints either a
labeled proof or a labeled soft beat. Optional evidence must never pass
silently. `story_dwell` remains film-only, so this narrative contract adds no
sleep when `SPUR_DEMO_STORY_PACE` is unset or `0`.

### submit_plan / auto-loop note

`submit_plan` is invoked by the **brain** (MCP tool), not a TUI button. The
operator journey is the **control plane** over the auto loop:

```text
Brain submit_plan → orchestrator dispatches workers → EXEC rows on lineage
  → worker results / review → brain continuation (auto loop)
```

TUI proof points: Plan browser progress · lineage BRAIN/EXEC tree · detail
tabs (stream/artifacts/attempts/task/review) · Activity events.

Opt-in mutation:

| Env | Effect |
|-----|--------|
| `SPUR_DEMO_ALLOW_PLAN_START=1` | Press `s` Start/Resume on selected plan |
| `SPUR_DEMO_ALLOW_AGENT_SEND=1` | Light brain kick then re-walk lineage |
| **`SPUR_DEMO_ALLOW_PLAN_LOOP=1`** | **Seed 1-task `submit_plan` via brain, wait for lineage EXEC/Running, re-walk brain↔worker + re-check plan browser** |
| `SPUR_DEMO_PLAN_LOOP_WAIT_S=180` | Max seconds to wait for EXEC/Running (default 180) |
| `SHELL_USE_TIMEOUT_MS=180000` | Brain-turn wait budget for the seed prompt |

Full live seed:

```bash
SPUR_DEMO_ALLOW_PLAN_LOOP=1 \
SPUR_DEMO_PLAN_LOOP_WAIT_S=240 \
bash journeys/problem-plan-loop-drive.sh
```

## Mapping from short surface probes

| Short probe | Absorbed into problem story |
|-------------|-----------------------------|
| `lineage-dashboard`, `help-overlay`, `palette-open` | `problem-ops-visibility` |
| `session-resume`, `sessions-picker` | `product-e2e-flow` (+ resume helpers) |
| `explore-*` | `product-e2e-flow` |
| plan/issue (shell-use e2e only) | `problem-plan-progress`, `problem-backlog-triage` |

Short probes remain for regression; problem stories are the **value demos**.
Their order in `journeys.conf` follows marketing value: plan loop, specialist
dispatch, operations visibility, campaign progress, then backlog triage.
