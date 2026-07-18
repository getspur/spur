# Live TUI problem stories

Each **problem story** is a continuous UAT + VHS journey on a real project.
Features answer a concrete pain. **Operator home is Session Detail**, not the
dashboard (`crates/spur-tui/src/views/session_detail`).

```text
Session Detail (home)
  ├── ReAct transcript (YOU / THINK / ACT / DELEGATE)
  ├── INSERT composer (@worker cascade, prompts)
  ├── Inline workers panel (Alt+d, Tab focus)
  ├── Alt+p plan inspector (when plan tracked)
  └── Go to (Ctrl+K) → Plans / Issues / Explore / Sessions

Dashboard / Lineage = optional ops overview (secondary)
```

| ID | Persona pain | Features exercised (session-first) | Proof anchors |
|----|--------------|-------------------------------------|---------------|
| **`problem-plan-loop-drive`** | “submit_plan loop is a black box — how do I drive it from my session?” | Session Detail, workers, Alt+p / Plans, optional seed | `Session ·`, `INSERT`, `YOU`/`DELEGATE`, workers; `Progress` when Plans opened |
| `product-e2e-flow` | “I need specialist + model/effort without losing session context.” | Session attach, Explore adopt, composer cascade | `Session ·`, `applied`, `agent=`, `model=`, `effort=` |
| `problem-ops-visibility` | “I can’t see what’s running where I work.” | Session help, workers, Go to; optional lineage overview | `Session ·`, `INSERT`, workers soft; `Go to` |
| `problem-plan-progress` | “Where is my multi-task campaign?” | Session → Plans / Alt+p | `Progress` or `No plans found` |
| `problem-backlog-triage` | “What’s on fire in the backlog?” | Session → Issues | `Issues`; `status: open` / `priority: P0` when present |

## Contract

1. **Lead with problem** in journey headers and README.
2. **Land Session Detail first** (`story_session_land` / `land_session_detail`).
3. **Bond features to the problem** — every beat moves toward resolution.
4. **Prove with wait strings** the operator saw the answer on (or from) session.
5. **Safe by default** — spend/mutation only behind `SPUR_DEMO_ALLOW_*` gates.
6. **Reuse lib helpers** — no fixture isolation fork.

### Beat spine

HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION  
`story_hard_proof` / `story_soft_proof` / `story_dwell` (film-only when pace=1).

### submit_plan note

`submit_plan` is brain MCP. Operator path:

```text
Session Detail compose → brain turn (YOU/THINK)
  → DELEGATE / workers panel → Alt+p or Plans hub
  → (optional) dashboard lineage as system map
```

| Env | Effect |
|-----|--------|
| `SPUR_DEMO_ALLOW_PLAN_LOOP=1` | Seed 1-task submit_plan in session; wait DELEGATE/Done |
| `SPUR_DEMO_ALLOW_HITL_LOOP=1` | Run the higher-spend four-worker Product Hunt audit |
| `SPUR_DEMO_ALLOW_AGENT_SEND=1` | Light brain kick in session |
| `SPUR_DEMO_ALLOW_PLAN_START=1` | Start/Resume on Plans |

The higher-spend D4 branch is separate from the minimal one-task seed and
remains opt-in. It submits four independent read-only tasks in the real `spur`
project: ACP positioning to Claude Code, TUI proof to Grok, launch readiness to
Codex, and media handoff to OpenCode. In Plan Inspector the operator approves
positioning, rejects the Grok proof once for a missing exact source window,
retries it with `SOURCE:`, `WINDOW:`, and `RECOMMENDATION:`, then approves the
proof, readiness, and handoff. All four are approved before the correlated
`PH AUDIT SYNTHESIS:` appears in the originating Session Detail. Missing any
task, state, summary, decision, retry evidence, or synthesis marker fails the
journey rather than softening the proof. Any worker transport fallback makes
the capture non-promotable.

The capture wrapper writes the new stable stem
`17-live-product-hunt-four-agent-loop`, requires the same explicit spend opt-in and
initialized `.beads/` backend, and requests a full-duration 2560x1600 encode
with preserved proof dwells. The shared capture path keeps its sampled-preview
encoder by default when full fidelity is not requested.

## Mapping

| Short probe | Absorbed into |
|-------------|----------------|
| `session-resume`, `sessions-picker`, `composer-draft` | session home + product-e2e |
| `explore-*` | product-e2e |
| `lineage-dashboard`, `palette-open` | ops (secondary overview) |
| plan/issue | plan-progress, backlog, plan-loop |
