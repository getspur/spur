# Follow-up: SessionDetail shows "Retiring previous session…" on warm resume when nothing is retiring

**Discovered during:** Tranche 2 final code review (2026-04-24).
**Related commits:** `c61ee88`, `e0c85c7` (Tranche 2 Task 5 + Task 2 review fix).
**Scope:** Small UX polish, ~10-20 LOC.

## Defect (cosmetic)

On a warm resume where no prior brain exists to retire (e.g. cold start, or after a failed connect), `SessionDetailView` is constructed via `for_session` with `LoadState::Retiring` as the default initial state. If the orchestrator's first emitted milestone is `SessionLoading` (skipping `SessionRetireStart/Complete` because there was nothing to retire), the view shows "Retiring previous session…" for one or more frames before transitioning to "Loading session history…". The label is briefly inaccurate.

## Why not blocking

- The incorrect label is visible only for the brief window between view construction and the first milestone event — typically <16ms (one frame) to ~100ms worst case.
- No correctness impact; the resume still completes and hydrates correctly.
- Tranche 2's core invariant (no stuck spinner, no halted load) holds.

## Fix options

**Option A — Rename the default state:** Rename `LoadState::Retiring` to `LoadState::Pending` or `LoadState::Starting` and render a generic label like "Starting session…" until a more specific milestone arrives.

**Option B — Detect cold path client-side:** If `SessionDetailView` knew whether a prior brain existed, it could initialize to `Connecting` or `Loading` directly on cold paths. But this duplicates orchestrator state in the view, violating FP-4.

**Option C — Orchestrator always emits an initial event:** Have the orchestrator emit a generic `ResumeStarting { session }` event at the top of the resume pipeline BEFORE any phase-specific milestone. SessionDetail transitions to a neutral "Starting…" state immediately. Adds one event variant.

**Recommendation:** Option A. Lowest blast radius, no new event variants, no orchestrator changes.

## Priority

Low. Cosmetic, visible briefly, confuses no one who understands the system.
