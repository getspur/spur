---
format: 1920x1080
duration: 69s
message: "Work with one brain while SPUR plans, delegates, observes, and reviews the workers underneath"
arc: Outcome hook → Brain intake → Work shaping → Worker execution → Control tower → Evidence → Review gate → Durable close
audience: developers and engineering leads operating multiple coding agents
mode: companion
music: existing SPUR launch bed; restrained terminal percussion; no narration
---

# SPUR launch story

## Video direction

- **Palette system:** graphite `#0B0E14` holds the world; warm ivory `#F3EFE4`
  carries human intent; amber `#D6A85F` marks control and active work; green
  `#69C98D` marks verified evidence; terracotta `#C78074` is reserved for the one
  critical escalation; muted text is `#8C928F`; terminal surfaces are `#161B24`.
- **Type and surface:** Menlo by display/body role, thin operational rules, real
  terminal rows, small state chips, evidence counters, and restrained 12px corners.
  Important content stays in the top 83% of frame.
- **Motion grammar:** deliberate state changes on smooth long-tail settles
  (`power3` register); reveal the next row, label, or token only when its story beat
  arrives; use velocity-matched internal seams. Holds are truly still except for a
  finite cursor blink, live status indicator, or subtle low-amplitude jitter.
- **Rhythm:** Frames 1–5 rise from intent into breadth; Frame 6 slows enough to read
  evidence; Frame 7 uses a held review decision; Frame 8 spends its last three seconds
  on a calm, readable lockup.
- **Negative list:** no neon gradients, purple/blue AI glow, glossy glass, robots,
  brains, floating spheres, random particles, glitch language, bouncy defaults,
  decorative equal-weight card grids, front-loaded slideshow motion, or screensaver
  drift. No fake browser chrome or generic dashboards when a real SPUR TUI surface can
  carry the beat.

## Frame 1 — One place to think

- scene: Five agent names reduce to one calm brain-facing promise over a real SPUR terminal
- voiceover: ""
- duration: 8s
- poster: 6.2s
- transition_in: cut
- status: animated
- src: compositions/frames/01-one-place-to-think.html
- type: hook
- persuasion: Outcome framing
- beat: recognition → relief
- blueprint: kinetic-type-beats
- posture: Adapt
- focal: assets/product/01-launch-hook.jpg
- roles: 01-launch-hook = background, dim ~46%
- sfx: two dry terminal ticks; one low confirmation impact
- asset_candidates: assets/product/01-launch-hook.jpg — real SPUR launch terminal, wide

Adapt: keep the fixed-center phrase relay and final held payoff; replace playful
spring/glitch variants with restrained hard cuts, a thin amber marker rule, and a real
TUI surface.

Scene 1 (0.0–1.8s): graphite field, one line only — **FIVE CODING AGENTS.** The words
assemble in three measured chunks via per-word staggered reveal, centered at dominant
scale; no background motion.

Scene 2 (1.8–4.3s): the line exits through a leftward waterfall cut
(`cut-catalog.md`), and **ONE PLACE TO THINK.** continues the same velocity into center.
`ONE` snaps amber only after the rest is readable.

Scene 3 (4.3–6.2s): a thin crop of the real SPUR terminal rises behind the type and
settles as a dim full-width strip; the copy becomes **WORK WITH ONE BRAIN.** through an
in-place state swap (`discrete-text-sequence`). The TUI is evidence, not decoration.

Scene 4 (6.2–8.0s): the promise and terminal hold completely still. A small amber
status chip — `SPUR · READY` — draws its outline and locks.

narrativeRole: Open on the human outcome, not orchestration vocabulary.
keyMessage: Multiple coding agents should feel like one place to think.

## Frame 2 — Tell the brain

- scene: A human request types into the real brain session and becomes an owned plan
- voiceover: ""
- duration: 9s
- poster: 7.5s
- transition_in: zoom-through
- status: animated
- src: compositions/frames/02-tell-the-brain.html
- type: product_intro
- persuasion: Friction reduction
- beat: agency + clarity
- blueprint: prompt-type-submit-generate
- posture: Adapt
- focal: assets/hyperframes/01-brain-intake.png
- roles: 01-brain-intake = cutout; 02-direct-brain = supporting
- sfx: restrained typing; soft enter key; amber state tick
- asset_candidates: assets/hyperframes/01-brain-intake.png — brain intake composition; assets/product/02-direct-brain.jpg — direct brain TUI

Adapt: keep prompt → submit → response as the signature; use SPUR's real terminal composer
and a terse brain acknowledgement instead of generic chat bubbles or an AI orb.

Scene 1 (0.0–1.4s): a real SPUR session fills an asymmetric 70/30 frame. Only the
composer and the label **TELL THE BRAIN WHAT YOU WANT** are visible; the rest of the TUI
is low-contrast context.

Scene 2 (1.4–5.2s): `Refactor auth. Keep the API stable. Run the tests.` types behind a
block caret (`discrete-text-sequence`). The input expands downward on line wrap; camera
stays locked.

Scene 3 (5.2–6.3s): the submit control compresses once with the caret. The prompt docks
into the transcript via a slow-fast-slow group nudge; no cursor chase.

Scene 4 (6.3–9.0s): the brain's response arrives in two cues — **I'LL SHAPE THE WORK.**
then an amber `PLAN READY` chip. The chip outline draws and the session holds on a human-
readable decision, not a spinner.

narrativeRole: Establish the brain as the user's only working interface.
keyMessage: The user states intent once; the brain owns the next move.

## Frame 3 — Shape the work

- scene: The brain routes work into Plan, Loop, and Ad hoc paths on one operational canvas
- voiceover: ""
- duration: 9s
- poster: 8.4s
- transition_in: push-slide LEFT
- status: animated
- src: compositions/frames/03-shape-the-work.html
- type: product_intro
- persuasion: Mechanism clarity
- beat: control
- blueprint: spatial-pan-stations
- posture: Adapt
- focal: assets/hyperframes/02-delegation-routes.png
- roles: 02-delegation-routes = cutout; 03-worker-routing = supporting
- sfx: three quiet routing ticks; one line-draw sweep
- asset_candidates: assets/hyperframes/02-delegation-routes.png — brain-to-worker routes; assets/product/03-worker-routing.jpg — worker routing TUI

Adapt: keep the single virtual canvas and station-to-station traverse; replace the
milestone timeline with SPUR's three real delegation shapes and remove all playful
callout springs.

Scene 1 (0.0–1.8s): the brain terminal is the left station, anchored by a thin amber
rule. The label **THE BRAIN SHAPES THE WORK** reveals in two chunks; camera is static.

Scene 2 (1.8–3.8s): one lateral pan (`viewport-change`) lands on `PLAN`; its subline
`bounded milestones` arrives only after the station centers. A route line self-draws
from the brain (`svg-path-draw`).

Scene 3 (3.8–5.8s): the same camera continues left to `LOOP`; `recurring checks` lands
with a green cadence marker. The prior station remains in world space but no longer
competes.

Scene 4 (5.8–7.4s): the pan reaches `AD HOC`; `one-off investigation` appears under a
muted command chip. All moves share one `.world` and one motion direction.

Scene 5 (7.4–9.0s): a final short pan reveals the terminal station
**RIGHT SHAPE → RIGHT WORKER.** The routes converge into three clean worker lanes and
hold; no camera movement after landing.

narrativeRole: Explain the product's orchestration model without turning it into a feature list.
keyMessage: Plan, Loop, and Ad hoc are different shapes of work, deliberately routed.

## Frame 4 — The workers work

- scene: Specialist worker rows execute in parallel and leave visible receipts
- voiceover: ""
- duration: 10s
- poster: 8.4s
- transition_in: push-slide LEFT
- status: animated
- src: compositions/frames/04-workers-work.html
- type: feature_showcase
- persuasion: Show-don't-tell proof
- beat: momentum
- blueprint: agent-progress-theater
- posture: Adapt
- focal: assets/product/03-worker-routing.jpg
- roles: 03-worker-routing = background, dim ~34%; 02-delegation-routes = supporting
- sfx: dispatch key; finite status ticks; three verification stamps
- asset_candidates: assets/product/03-worker-routing.jpg — worker session rows; assets/hyperframes/02-delegation-routes.png — delegation route composition

Adapt: keep trigger → working theater → mutating receipt; render workers as terminal rows,
not cards, and reserve motion for status and evidence state.

Scene 1 (0.0–1.4s): an amber `DISPATCH 3` command lands at the top of a dense terminal
surface. One press triggers three lanes; the pointer leaves immediately.

Scene 2 (1.4–4.2s): `CODEX · implementation`, `CLAUDE · review`, and
`GEMINI · investigation` arrive row by row. Each gains a finite active indicator and one
concrete status phrase; no decorative loader loops.

Scene 3 (4.2–7.4s): receipts mutate in sequence: `patch written`, `tests running`,
`trace captured`. Numbered outlines swap to green checks as each worker finishes its
step.

Scene 4 (7.4–10.0s): the final column resolves into `3 ACTIVE · 2 VERIFIED · 1 WAITING`.
The active indicator stops at the exact state change. All lanes hold while a small
footer reads **THE RIGHT WORKER PICKS IT UP.**

narrativeRole: Turn delegation into visible labor rather than invisible agent magic.
keyMessage: Specialist workers act independently, but their work stays legible.

## Frame 5 — The control tower

- scene: A single worker status pulls back into the complete SPUR control tower
- voiceover: ""
- duration: 10s
- poster: 8.6s
- transition_in: zoom-through
- status: animated
- src: compositions/frames/05-control-tower.html
- type: benefit_highlight
- persuasion: Visibility and control
- beat: command
- blueprint: zoom-out-workspace-reveal
- posture: Adapt
- focal: assets/product/04-control-tower.jpg
- roles: 04-control-tower = background, dim ~38%; 02-delegation-routes = supporting
- sfx: two close status ticks; one restrained camera pull; soft lock tone
- asset_candidates: assets/product/04-control-tower.jpg — full SPUR control tower; assets/hyperframes/02-delegation-routes.png — orchestration diagram

Adapt: keep the close-up dwell → one decelerating zoom-out → locked-wide payoff. Rebuild
the magnified status row as crisp DOM; use the real control-tower capture only at the
wide, where it remains sharp.

Scene 1 (0.0–2.6s): extreme close-up on `tests 84/84` with its green evidence counter.
One new trace row enters below and the selection highlight steps to it. No surrounding
chrome is visible.

Scene 2 (2.6–4.2s): an adjacent row appears — `review pending` — and a small amber
operator marker advances. The camera remains at the same tightness.

Scene 3 (4.2–5.2s): ONE fast, heavily decelerating zoom-out
(`viewport-change`, `coordinate-target-zoom`) reveals the full dashboard: brain session,
Plan/Loop/Ad hoc lanes, worker state, evidence, queue, and review. The move ends fully.

Scene 4 (5.2–10.0s): the wide frame is locked. One blocked row turns amber, its owning
worker gains focus, and an evidence count increments. At 7.0s the line
**EVERY WORKER STAYS VISIBLE.** appears in the upper third and holds; no camera motion
returns.

narrativeRole: Deliver the breadth reveal and establish SPUR as the operator's control tower.
keyMessage: Parallel work remains observable as one coherent system.

## Frame 6 — Evidence comes back

- scene: The task feed traverses real work, then pivots into the diff, tests, and trace artifact
- voiceover: ""
- duration: 9s
- poster: 7.7s
- transition_in: crossfade
- status: animated
- src: compositions/frames/06-evidence-comes-back.html
- type: feature_showcase
- persuasion: Verifiable proof
- beat: trust
- blueprint: transcript-scroll-artifact-reveal
- posture: Adapt
- focal: assets/hyperframes/03-evidence-return.png
- roles: 03-evidence-return = cutout; 04-control-tower = supporting
- sfx: dry feed steps; one file-chip press; soft evidence reveal
- asset_candidates: assets/hyperframes/03-evidence-return.png — evidence return composition; assets/product/04-control-tower.jpg — control tower with evidence rows

Adapt: keep traverse → hinge → artifact as the signature. The surface is a dark SPUR
task feed, and the artifact is a concrete review bundle rather than a generic generated
document.

Scene 1 (0.0–3.8s): a full-bleed task feed scrolls upward as an element
(`3d-page-scroll`, flat variant): `4 files changed`, `84 tests passed`,
`trace attached`, `cost recorded`. Each row enters on its own reading beat; edge masks
keep the traversal clean.

Scene 2 (3.8–5.1s): the scroll settles on
`artifact://review/auth-refactor`. A file chip spring-settles in and receives the shot's
only click (`cursor-click-ripple`).

Scene 3 (5.1–7.2s): the chip expands into a three-part artifact:
`DIFF`, `TESTS`, `TRACE` (`anchored-layout-expand`). Green added lines, the passing test
counter, and the trace receipt reveal left-to-right, one at a time.

Scene 4 (7.2–9.0s): the headline **NOT UPDATES. EVIDENCE.** arrives above the artifact.
The frame locks on readable proof; only a finite caret blink remains.

narrativeRole: Convert worker activity into concrete review material.
keyMessage: SPUR returns evidence that can be inspected, not status prose to be trusted.

## Frame 7 — Review before escalation

- scene: The brain verifies the evidence, approves routine work, and sends only one critical decision to the human
- voiceover: ""
- duration: 7s
- poster: 6.1s
- transition_in: crossfade
- status: animated
- src: compositions/frames/07-review-before-escalation.html
- type: feature_showcase
- persuasion: Risk control
- beat: confidence + attention
- blueprint: agent-progress-theater
- posture: Adapt
- focal: assets/hyperframes/05-brain-review-proof.png
- roles: 05-brain-review-proof = cutout; 04-critical-escalation = supporting; 05-brain-review = background, dim ~36%
- sfx: three muted verification stamps; one restrained critical bell
- asset_candidates: assets/hyperframes/05-brain-review-proof.png — brain review proof; assets/hyperframes/04-critical-escalation.png — critical human escalation; assets/product/05-brain-review.jpg — real brain review TUI

Adapt: keep the receipt card's live state mutation, but pin the brain review as the
load-bearing actor. The human terminal stays small and inactive until the single
critical branch reaches it.

Scene 1 (0.0–1.6s): the evidence bundle enters the brain review lane. The title
**THE BRAIN REVIEWS FIRST.** builds in two cues (`dynamic-content-sequencing`) above a
left-weighted 70/30 terminal layout.

Scene 2 (1.6–3.9s): `API stable`, `tests passed`, and `trace complete` arrive and flip to
green checks in sequence (`scale-swap-transition`, `svg-path-draw`). Their labels dim
slightly after verification so the unresolved decision owns the eye.

Scene 3 (3.9–5.4s): one terracotta row appears:
`CRITICAL · rotate production key?`. It does not pulse. A thin connector self-draws
(`svg-path-draw`) from the brain lane to the small human terminal on the far right.

Scene 4 (5.4–7.0s): the human terminal wakes with one contained prompt:
**YOUR DECISION.** Beneath it, the supporting line lands:
`Everything else keeps moving.` The critical bell rings once; the frame holds.

narrativeRole: State the review hierarchy precisely and humanely.
keyMessage: Brain review is the default; human attention is reserved for critical choices.

## Frame 8 — Resume with the thread intact

- scene: The session state changes around an unmoving context ledger, then resolves into the SPUR promise
- voiceover: ""
- duration: 7s
- poster: 6.6s
- transition_in: blur-crossfade 0.45s
- status: animated
- src: compositions/frames/08-resume-and-close.html
- type: branding
- persuasion: Continuity assurance
- beat: peace of mind → resolve
- blueprint: fixed-anchor-cycle
- posture: Adapt
- focal: assets/product/06-closing-promise.jpg
- roles: 06-closing-promise = background, dim ~42%; 01-brain-intake = supporting
- sfx: session close tick; resume key; final low lock tone
- asset_candidates: assets/product/06-closing-promise.jpg — SPUR closing promise; assets/hyperframes/01-brain-intake.png — persistent brain context

Adapt: keep one identity/context anchor absolutely pinned while surrounding session states
change; shorten the cycle to three proof states and resolve into a quiet brand lockup.

Scene 1 (0.0–1.4s): a compact context ledger lands dead-center:
`brain: auth-refactor · evidence: 4 · decision: 1`. Once seated, it never moves.

Scene 2 (1.4–3.8s): the shell around the ledger cycles through three discrete terminal
states (`theme-crossfade-morph`): `SESSION CLOSED` → `RESUME` → `WORK CONTINUES`. The
context fingerprint remains pixel-identical in every state.

Scene 3 (3.8–4.8s): a green line appears beneath the pinned ledger:
**THE THREAD IS INTACT.** The outer chrome washes to muted graphite and holds.

Scene 4 (4.8–7.0s): the ledger clears on a hard state cut. `SPUR` locks center, followed
by three restrained line builds: **ONE BRAIN. EVERY WORKER. UNDER CONTROL.** The final
lockup holds through the last frame with no camera move.

narrativeRole: Use the extra nine seconds for durable-context proof, then close on the thesis.
keyMessage: SPUR preserves the working context and keeps the whole agent system under control.
