# Live TUI problem stories

Each **problem story** is a continuous UAT + VHS journey on a real project.
Features are not demos of chrome — they answer a concrete pain.

| ID | Persona pain | Features exercised | Proof anchors |
|----|--------------|--------------------|---------------|
| `product-e2e-flow` | “I need a specialist persona + model + effort without reinventing agents.” | sessions, explore adopt/gate/pool, `@worker` cascade | `applied`, `agent=`, `model=`, `effort=` |
| `problem-ops-visibility` | “I can’t see what’s running or how to drive multi-agent work.” | lineage, activity, help overlay, palette | `Lineage`, `Activity`, `Dashboard —`, `Go to` |
| `problem-plan-progress` | “Where is my multi-task campaign? What’s awaiting review?” | plan browser, summary pane, navigate plans | `Plans`, `Progress`, `awaiting`/`complete`, `Work item` |
| `problem-backlog-triage` | “What’s on fire in the backlog?” | issue browser, P0 list, issue detail | `Issues`, `P0`, `open`, `bd-` |

## Contract

1. **Lead with problem** in journey header comments and README.
2. **Bond features to the problem** — every beat must move the user toward resolution.
3. **Prove with wait strings** that the user saw the answer (not just that a view opened).
4. **Safe by default** — no agent spend unless `SPUR_DEMO_ALLOW_AGENT_SEND=1`.
5. **Reuse lib helpers** — no fork of isolation/fixtures.

## Mapping from short surface probes

| Short probe | Absorbed into problem story |
|-------------|-----------------------------|
| `lineage-dashboard`, `help-overlay`, `palette-open` | `problem-ops-visibility` |
| `session-resume`, `sessions-picker` | `product-e2e-flow` (+ resume helpers) |
| `explore-*` | `product-e2e-flow` |
| plan/issue (shell-use e2e only) | `problem-plan-progress`, `problem-backlog-triage` |

Short probes remain for regression; problem stories are the **value demos**.
