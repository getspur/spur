# Live TUI problem stories

Each **problem story** is a continuous UAT + VHS journey on a real project.
Features are not demos of chrome — they answer a concrete pain.

| ID | Persona pain | Features exercised | Proof anchors |
|----|--------------|--------------------|---------------|
| `product-e2e-flow` | “I need a specialist persona + model + effort without reinventing agents.” | sessions, explore adopt/gate/pool, `@worker` cascade | `applied`, `agent=`, `model=`, `effort=` |
| `problem-ops-visibility` | “I can’t see what’s running or how to drive multi-agent work.” | lineage, activity, help, palette, **Agents tree focus + detail tabs** | `Lineage`, `Activity`, `[Agents]`, `stream`/`artifacts` |
| `problem-plan-progress` | “Where is my multi-task campaign? What’s awaiting review?” | plan browser, summary pane, navigate plans | `Plans`, `Progress`, `awaiting`/`complete`, `Work item` |
| `problem-backlog-triage` | “What’s on fire in the backlog?” | issue browser, P0 list, issue detail | `Issues`, `P0`, `open`, `bd-` |
| **`problem-plan-loop-drive`** | “submit_plan auto-loop is a black box — brain↔worker outputs, how do I drive it?” | plan browser (campaigns), lineage Agents j/k/Enter, detail tabs (stream/attempts/task/review), activity log; optional brain kick / plan start | `Plan`/`Progress`, `[Agents]`, `stream`, `attempts`, `Activity`, `BRAIN`/`EXEC` |

## Contract

1. **Lead with problem** in journey header comments and README.
2. **Bond features to the problem** — every beat must move the user toward resolution.
3. **Prove with wait strings** that the user saw the answer (not just that a view opened).
4. **Safe by default** — no agent spend unless `SPUR_DEMO_ALLOW_AGENT_SEND=1`.
5. **Reuse lib helpers** — no fork of isolation/fixtures.

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
| `SPUR_DEMO_ALLOW_AGENT_SEND=1` | Kick a brain turn then re-walk lineage |

## Mapping from short surface probes

| Short probe | Absorbed into problem story |
|-------------|-----------------------------|
| `lineage-dashboard`, `help-overlay`, `palette-open` | `problem-ops-visibility` |
| `session-resume`, `sessions-picker` | `product-e2e-flow` (+ resume helpers) |
| `explore-*` | `product-e2e-flow` |
| plan/issue (shell-use e2e only) | `problem-plan-progress`, `problem-backlog-triage` |

Short probes remain for regression; problem stories are the **value demos**.
