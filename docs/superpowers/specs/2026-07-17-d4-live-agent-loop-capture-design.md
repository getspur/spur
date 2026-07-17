# D4 live agent-loop capture design

**Date:** 2026-07-17  
**Status:** Approved for implementation  
**Surface:** `scripts/e2e/demos/tui-live/`  
**Story:** `problem-plan-loop-drive`  
**Spend authorization:** Real brain and worker sends approved by the user for this scoped capture

## Purpose

Produce real SPUR TUI evidence for the D4 launch narrative: a human remains in
continuous control while a brain agent plans or delegates work, specialist
workers deep-dive, evidence returns for review, and the human requests changes
or approves the result without losing session context.

The current live seed proves that a brain can call `submit_plan` and a worker can
appear. Its worker replies only `ok`, so it does not prove a meaningful deep
dive, human steering, a second worker attempt, approval, or final brain
synthesis. The new capture closes that proof gap with existing SPUR controls.

## Product truth

The D4 film must be assembled from a real SPUR TUI session. Narration and later
Open Design framing may explain visible behavior, but they must not invent UI or
claim a state that the capture does not show.

Current real controls used by the story:

- Session Detail composer and ReAct transcript (`YOU`, `THINK`, `DELEGATE`).
- Session-scoped workers panel and plan inspector.
- Plan task status `awaiting_review`.
- Plan Inspector review keys:
  - `c` opens `Review Task` with `Decision: Request changes`.
  - `R` opens `Retry Task` with an appended human instruction.
  - `a` opens `Review Task` with `Decision: Approve`.
- Review confirmations require `Enter` and can be cancelled with `Esc`.
- Session Detail remains the operator home before and after the loop.

## Audience and problem

The viewer is a senior/staff engineer or tech lead already coordinating multiple
CLI coding agents.

Problem sentence:

> I can delegate work, but I need to see what the worker found, correct it, and
> approve the improved result without losing the brain session that holds the
> broader context.

Resolution:

> SPUR keeps the brain, workers, plan state, evidence, and human decisions in one
> inspectable loop.

## Story architecture

The capture follows the repository's required beat spine:

### Hook

From Session Detail, the human asks the brain a focused repository question.
The prompt is visible and explicitly requests a one-task, read-only
`submit_plan` campaign.

### Orientation

The session transcript records the brain turn. The plan task has a unique
per-run correlation ID so later worker, plan, and review states cannot be
mistaken for historical activity.

### Action: first worker attempt

The worker inspects existing TUI demo-story material and returns one focused
finding. The task forbids file changes. The capture may show the workers panel,
worker stream/task tabs, and plan progress while the attempt runs.

### Proof: continuous HITL

When the correlated task reaches `awaiting_review`:

1. The human opens Plan Inspector.
2. The human presses `c` and confirms `Decision: Request changes`.
3. After the task becomes retryable, the human presses `R`.
4. The human appends a concrete correction: include an exact source path and one
   recommendation, still with no file changes.
5. The second worker attempt completes and returns to `awaiting_review`.
6. The human presses `a` and confirms `Decision: Approve`.

These are hard proof beats. Missing review state or confirmation text fails the
D4 journey; it must not silently become a soft beat.

### Resolution: brain synthesis

The story returns to Session Detail. The human asks the brain to synthesize the
approved worker evidence without further delegation. The final frame preserves
the same session, with workers and plan state still reachable.

## Implementation boundary

Reuse `problem-plan-loop-drive`; do not create a duplicate marketing story.

- Keep the observe-only path unchanged and safe by default.
- Keep `SPUR_DEMO_ALLOW_PLAN_LOOP=1` as the existing minimal one-task seed.
- Add `SPUR_DEMO_ALLOW_HITL_LOOP=1` as a separate, higher-spend D4 branch.
- Add a dedicated capture wrapper and distinct output stem for the D4 film.
- Reuse shared navigation, pacing, proof, and session helpers in `lib.sh`.
- Keep the existing observe-only VHS tape as regression evidence. The gated D4
  path is captured through the live shell-use cast because model timing and
  review state are dynamic.

The new branch may add helpers local to `lib.sh` for:

- waiting for the correlated task to reach a hard status;
- opening the plan inspector on the one correlated task;
- submitting request-changes, retry, and approve decisions in order;
- returning to Session Detail and requesting final synthesis.

## Safety and spend controls

- The D4 branch runs only when `SPUR_DEMO_ALLOW_HITL_LOOP=1`.
- The worker prompt is evidence-only and forbids file writes.
- The retry prompt remains evidence-only and forbids file writes.
- The branch creates one plan task with at most two worker attempts.
- Final brain synthesis explicitly forbids further delegation.
- The capture wrapper prints the enabled gate, wait budgets, and output paths
  before starting.
- Cast and log output are always retained for audit, even if GIF/MP4 conversion
  is unavailable.
- No new path is wired into CI or the default `render.sh` flow.

The plan engine creates and records beads issues for the live task. The story
uses those real records; it does not create a parallel fake task ledger.

## Failure behavior

The D4 capture fails if any of these conditions is not met within its configured
wait budget:

- the brain turn is not visible;
- the unique task ID never appears;
- the correlated task never reaches `awaiting_review`;
- request-changes or approve confirmation text is absent;
- the rejected task does not become retryable;
- the second attempt never returns for review;
- the final session cannot be restored.

Worker slowness may use a configurable wait budget, but absence of proof is not
success. The existing observe-only story retains its softer behavior for empty
or historical projects.

## Static contract

Extend `story-contract.test.sh` before implementation so it requires:

- a distinct `SPUR_DEMO_ALLOW_HITL_LOOP` gate;
- unchanged safe-default and minimal-seed gates;
- a unique per-run D4 task ID;
- read-only first and retry prompts;
- hard waits for `awaiting_review`;
- request-changes -> retry -> approve ordering;
- explicit final brain synthesis with no further delegation;
- return to Session Detail before resolution;
- a dedicated capture wrapper/output stem.

The contract must fail before the D4 helpers exist, then pass after the
implementation lands.

## Runtime verification

Verification order:

1. Run the static story contract.
2. Run the safe-default `problem-plan-loop-drive` journey without spend gates.
3. Run the existing minimal plan-loop seed if regression confidence is needed.
4. Run the gated D4 capture with the approved spend flag.
5. Inspect the log and cast for task correlation and proof ordering.
6. Review the derived MP4 at normal speed for legibility and truthful framing.
7. Only after visual approval, copy the selected source into the media pack,
   bind its checksum/timestamps/crops in `proof-manifest.json`, and rebuild the
   media contracts.

## Media handoff

The D4 live film becomes the candidate source for the missing active-loop proof.
It does not automatically replace an approved Product Hunt asset. Promotion
requires:

- visual review of the real session;
- a stable source copy under the media pack;
- checksum approval;
- claim-bound timestamps and proof terms;
- refreshed gallery/video derivatives;
- passing media-pack contracts.

The Open Design artifact must continue to label conceptual loop diagrams as
explanation and manifest-bound TUI pixels as product proof.
