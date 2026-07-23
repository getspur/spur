# Expanded production prompt — SPUR launch story

Create a 69-second, 1920×1080 HyperFrames launch film for SPUR. The audience is
developers and engineering leads who operate several coding agents but want one coherent
place to think. The central promise is: work directly with one brain while SPUR shapes,
delegates, observes, and reviews the worker agents underneath.

This is a warm terminal documentary, not a generic AI promo. Build from real SPUR TUI
language: terminal rows, thin rules, state chips, evidence counters, plan/loop/ad-hoc
lanes, worker receipts, review bundles, and one small human decision terminal. Avoid
robots, brain icons, purple/blue gradients, neon glow, glossy glass, floating particles,
glitch effects, fake dashboards, and equal-weight feature cards.

## Brand and visual system

- Canvas/background: `#0B0E14`
- Foreground/human intent: `#F3EFE4`
- Active control: `#D6A85F`
- Verified success: `#69C98D`
- Critical escalation only: `#C78074`
- Muted operational text: `#8C928F`
- Terminal surface: `#161B24`
- Type: Menlo for display, body, and operational labels
- Corners: 12px, used sparingly
- Composition: horizontal operational flow, brain as the load-bearing anchor, worker
  lanes as real terminal rows, human review terminal small and far right
- Caption keep-out: keep meaningful content above the bottom 17%

## Motion system

Use one paused, seek-safe GSAP timeline per composition. Every entrance has explicit
from/to states. Keep motion deterministic and finite: no randomness, autoplay CSS,
infinite repeats, yoyo loops, or wall-clock behavior.

The house motion register is smooth and deliberate. Prefer long-tail `power3`-class
settles; avoid bounce and overshoot unless a single contained press or status stamp truly
earns it. Reveal content sequentially across each shot, including the back half, rather
than loading the whole canvas in the first quarter. Once a shot resolves, let it hold.
During holds, allow only meaningful state motion, a finite caret blink, or subtle jitter.
No lazy breathing and no late camera drift.

Within a shot, use velocity-matched seams: waterfall cuts for phrase changes,
cut-the-curve for directional scene handoffs, and zoom-through only for genuine state
changes. Between shots, use the storyboard's small transition family: push-slide for the
mechanism run, zoom-through for section changes, crossfade within the operational world,
and one restrained blur-crossfade into the close.

## Story and shot treatment

### 1. One place to think — 8 seconds

Cold-open on the human outcome. Adapt `kinetic-type-beats`: “FIVE CODING AGENTS.” becomes
“ONE PLACE TO THINK.”, then “WORK WITH ONE BRAIN.” A dim crop of the real SPUR terminal
rises only after the promise lands. Hold on a small `SPUR · READY` chip. Use
`dynamic-content-sequencing`, `discrete-text-sequence`, `svg-path-draw`, and a restrained
waterfall cut. No product vocabulary before the benefit is understood.

### 2. Tell the brain — 9 seconds

Adapt `prompt-type-submit-generate` around the real brain session. Type:
“Refactor auth. Keep the API stable. Run the tests.” The terminal composer expands on
wrap, the submit compresses once, and the brain answers: “I’LL SHAPE THE WORK.” followed
by `PLAN READY`. Use `discrete-text-sequence`, `context-sensitive-cursor`,
`anchored-layout-expand`, `physics-press-reaction`, and `nudge-curve`. The keyboard is the
actor; do not chase a decorative cursor.

### 3. Shape the work — 9 seconds

Adapt `spatial-pan-stations` onto one oversized operational canvas. Traverse the brain,
then `PLAN · bounded milestones`, `LOOP · recurring checks`, and
`AD HOC · one-off investigation`, landing on “RIGHT SHAPE → RIGHT WORKER.” The single
camera pans laterally with `viewport-change`; connectors self-draw with `svg-path-draw`.
No second camera system and no playful callout springs.

### 4. The workers work — 10 seconds

Adapt `agent-progress-theater` as dense terminal lanes. One `DISPATCH 3` trigger hands
control to `CODEX · implementation`, `CLAUDE · review`, and
`GEMINI · investigation`. Their receipts mutate from numbered states into green checks:
`patch written`, `tests running`, `trace captured`. Kill each active indicator when its
state resolves. Use `cursor-click-ripple`, `svg-icon-enrichment`,
`scale-swap-transition`, and `svg-path-draw`. End on
`3 ACTIVE · 2 VERIFIED · 1 WAITING`.

### 5. The control tower — 10 seconds

Adapt `zoom-out-workspace-reveal`. Start extremely tight on a crisp DOM
`tests 84/84` evidence row, then add `review pending`. Make exactly one fast,
heavily-decelerating zoom-out with `viewport-change` and `coordinate-target-zoom` to
reveal the full SPUR control tower: brain, Plan/Loop/Ad hoc, worker lanes, evidence,
queue, and review. Lock the camera completely after the reveal. Let one blocked row turn
amber and an evidence count increment. Hold on “EVERY WORKER STAYS VISIBLE.”

### 6. Evidence comes back — 9 seconds

Adapt `transcript-scroll-artifact-reveal`. Traverse a full-bleed SPUR task feed:
`4 files changed`, `84 tests passed`, `trace attached`, `cost recorded`. Stop on
`artifact://review/auth-refactor`, click the chip once, and expand it into a readable
three-part bundle: `DIFF`, `TESTS`, `TRACE`. Use flat `3d-page-scroll`,
`cursor-click-ripple`, `anchored-layout-expand`, and ordered state reveals. End on
“NOT UPDATES. EVIDENCE.” with the artifact held long enough to read.

### 7. Review before escalation — 7 seconds

Adapt `agent-progress-theater` around the review receipt. The evidence enters the brain
lane under “THE BRAIN REVIEWS FIRST.” Verify `API stable`, `tests passed`, and
`trace complete` into green checks. Then reveal one non-pulsing terracotta row:
`CRITICAL · rotate production key?`. Draw a thin connector to the small human terminal,
which wakes with “YOUR DECISION.” and `Everything else keeps moving.` One restrained
critical bell only. The brain remains the primary reviewer; the human is an exception
path, not a co-equal lane.

### 8. Resume with the thread intact — 7 seconds

Adapt `fixed-anchor-cycle`. Pin a context ledger at center:
`brain: auth-refactor · evidence: 4 · decision: 1`. It must not move while the shell
crossfades through `SESSION CLOSED`, `RESUME`, and `WORK CONTINUES`. The fingerprint
remains pixel-identical. Resolve on “THE THREAD IS INTACT.”, then clear to the quiet SPUR
lockup: “ONE BRAIN. EVERY WORKER. UNDER CONTROL.” Hold through the final frame.

## Assets

Use the staged assets under `assets/product/` and `assets/hyperframes/` as real product
reference and supporting texture. Do not generate substitute dashboards. When a shot
requires magnification, reconstruct the needed TUI region as DOM so type and rules remain
crisp; bring the full-resolution capture in at the wide view.

## Audio

No narration and no captions. Reuse the existing SPUR launch music identity, extended to
69 seconds without an audible loop seam. Add restrained, diegetic terminal SFX only:
typing, enter, route ticks, finite status ticks, evidence click, review stamps, one
critical bell, and a low final lock tone. The music stays underneath the UI; SFX should
clarify state, never turn the piece into a trailer.

## Delivery constraints

Use a modular composition with eight sub-compositions and continuous root-level audio.
The assembled runtime must be exactly 69 seconds. Lint and check must pass, and the
midpoint contact sheet must show clear visual hierarchy, real SPUR language, readable
evidence, and the correct brain-first review flow before rendering.
