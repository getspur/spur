# Frame packet: 06-evidence-comes-back

## Project inputs

- Project: /Volumes/Projects/spur/videos/spur-launch-story
- Design truth: /Volumes/Projects/spur/videos/spur-launch-story/frame.md
- RULES_DIR: /Volumes/Projects/spur/.agents/skills/hyperframes-animation/rules

## Assigned storyboard block

## Frame 6 — Evidence comes back

- scene: The task feed traverses real work, then pivots into the diff, tests, and trace artifact
- voiceover: ""
- duration: 9s
- poster: 7.7s
- transition_in: crossfade
- status: outline
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

## Selected blueprint: transcript-scroll-artifact-reveal

# transcript-scroll-artifact-reveal — Transcript-Scroll Artifact Reveal

**intent**: The frame travels vertically along ONE long content surface — an agent transcript, a running task feed, an analysis document, a story draft — rendered full-bleed on a flat canvas (no device frame, no held mockup), by camera pan or element scroll; the traversal itself is the story ("look how much work happened / how much is here"), until ONE focal interaction — a file-chip click, a quote highlight, a collapsible-row expand — pivots the shot into an artifact/detail reveal: the deliverable behind the work.

**roles served**

- Key_Feature (modes: `pan-to-workspace` · `feed-rush` · `document-to-artifact` · `selection-pivot`): the x-viral AI-product grammar for "the agent did a lot of work → here's the deliverable." The long surface is the EVIDENCE (tool pills, checked progress items, task rows, headings, comps tables, story paragraphs), read at traversal pace; the artifact is the PAYOFF (full workspace with live mockup, spreadsheet with highlighted cells, inline ask-panel, sub-task stack). Reach for it when the feature's proof is the volume/depth of generated work and the beat should cash that in on one interaction — not a held device tour (`device-surface-showcase`), not a cursor-chased workflow (`cursor-ui-demo`).

**duration**: 5–11.8s (feed-rush 5.4s · pan-to-workspace 5.0s · selection-pivot 9.3s · document-to-artifact 11.75s)

**shot structure** One `[long content surface: agent chat transcript / task feed / analysis document / story doc]` sits full-bleed on a `[flat light canvas]` (goldens: warm off-white / cream / beige / plain white — the surface's own background IS the scene background); dark text with small `[accent]` marks (green verb highlights, model-tag pills, check circles, yellow cells). Three acts: TRAVERSE → HINGE → ARTIFACT. Camera discipline is the signature: at most TWO real camera moves in the whole shot, bracketing the hinge; everything else is element motion on a static frame.

- **Scene 1 (0.0–~40–60% of runtime) — establish + vertical traversal (the evidence).** The surface establishes with one small opener — a `[title]` types on / a centered `[title]` shrinks ~50% and glides to the top-left to dock as a fixed header / the frame opens tight on the `[chat panel]` — then the traversal begins: the frame travels DOWN the content (or the content streams UP through the frame), revealing progressive work in reading order: `[prompt → tool pills → checked progress items → typed summary]`, `[tagged task rows → muted tasks → checklist block]`, `[heading → paragraph → comps table → bullets]`, `[title → story paragraphs → dialogue]`. New rows may cascade in (staggered arrival) before the scroll takes over; a typed line may finish under the moving frame. Traversal texture varies by member: one continuous slow pan, a fast continuous feed rush, stepped scrolls decelerating at each stop (speed-blur between stops, content fading at frame edges), or one smooth scroll easing to a stop.
- **Scene 2 (~1–2s) — the hinge: ONE focal interaction.** The traversal settles and a single interaction pivots the shot: a `[file-attachment chip]` spring-pops in below a typed handoff line and a cursor glides in and CLICKS it; a `[sentence/quote]` gets a selection-highlight sweep and a `[tooltip pill]` spring-pops above it for the click; a `[collapsible row]` reaches the frame center and EXPANDS; or the typed `[verifier summary]` completes as the implicit trigger. This is the only interaction in the shot — the cursor (if any) appears here for the first time.
- **Scene 3 (rest) — artifact reveal + hold.** The hinge cashes in, choosing ONE reveal mechanic: a fast smoothly-DECELERATING zoom-OUT re-frames the whole `[workspace]` (the panel just traversed becomes a sidebar beside a `[live mockup]` and `[tool panel]`); an `[artifact window: spreadsheet]` scales up from small toward full frame, then a slow push-in + lateral pan settles on its `[highlighted cells]`; an `[inline panel]` expands below the highlighted line and a `[follow-up question]` types into it; or the row unfolds into a `[sub-task stack]` and the scroll settles on `[narration text]`. Optional coda: one cursor click instantly swaps a `[screen]` inside the revealed artifact (e.g. a phone tab click). Frame locks; element motion only to the end.

- Variant — _pan-to-workspace_ (001_claudeai, 5.0s): traversal is a REAL camera pan — opens tight on the chat panel, one single uninterrupted downward glide (never cutting away) over pills → checked list → typing verifier summary; hinge is the summary completing; reveal is ONE rapid decelerating zoom-out to the three-part workspace (chat-as-sidebar / phone mockup / tweaks panel); coda cursor click swaps the phone screen instantly. Exactly two camera moves total.
- Variant — _feed-rush_ (010_perplexity A, 5.4s): NO camera at all — title docks to header, five tagged rows cascade in, then a fast continuous upward ELEMENT scroll races through muted tasks and a checklist to a collapsible row; hinge is the row itself; reveal is the row expanding into a six-item sub-task stack, settling on narration. Cursorless.
- Variant — _document-to-artifact_ (010_perplexity B, 11.75s): traversal is a stepped ELEMENT scroll (static frame) — the document climbs in fast steps, decelerating at each stop, blur/fade between stops, clearing to blank canvas; hinge is a typed handoff line + file-chip pop + cursor click; reveal is the spreadsheet window scaling up then one slow continuous push-in + rightward pan onto the yellow-highlighted forecast columns.
- Variant — _selection-pivot_ (014_OpenAI, 9.3s): typed headline → document builds (bubble prompt + typed title + populating paragraphs) → one smooth upward element scroll eases to a stop; hinge is the selection-highlight sweep + the shot's ONE push-in framing the sentence + tooltip-pill click; reveal is the inline panel expanding below the line with the referenced quote and a rapidly-typed follow-up question. Camera locked at the pushed-in zoom to the end.

**motion vocabulary** continuous slow downward camera pan; fast continuous upward feed scroll; stepped document scroll decelerating at each stop; smooth scroll easing to a stop; speed-blur between scroll stops; content fade at frame edges; centered title shrinks ~50% and glides to a top-left header dock; task rows cascade in staggered; typed line / typed title / typed follow-up question (caret); green leading-verb highlights and model-tag pills riding past; checked-item strikethroughs riding past; file-attachment chip spring pop-in; tooltip pill spring pop; chat-bubble arrival; cursor glide-in + click; selection-highlight sweep across a sentence; ONE camera push-in onto the selection; fast decelerating zoom-out to the full workspace; artifact window scales up from small; slow push-in + lateral pan settling on highlighted cells; collapsible row expands into a sub-task stack; inline panel expands below the line; phone-screen instant swap on a coda tab click; frame-lock hold.

**rule mapping**

- vertical traversal by ELEMENT scroll — fast feed rush / stepped document scroll / smooth scroll-to-stop → `3d-page-scroll` (flat variant: tilt ≈ 0 — the surface's content `translateY`-scrolls to sections; the multi-phase scroll variant covers stepped stops; keep ONE ease family across all steps — `power3.out`/`power4.out` for UI-scroll feel)
- vertical traversal by CAMERA pan (transcript glide) → `viewport-change` (pan mode — the world translates up under a static frame; one continuous tween, no cuts)
- speed-blur between stepped-scroll stops → `motion-blur-streak` (blur peaks at max scroll velocity, resolves to 0 at each settle)
- which content each traversal beat reveals (stop-by-stop sequencing) → `dynamic-content-sequencing`
- centered title shrinks and glides to dock as a fixed header → `gsap-effects` (one simultaneous scale + translate tween; plain two-property move, no named rule required)
- task rows cascade in staggered before the scroll takes over → `waterfall-entry` (arrival cascade; goldens use fade + slide-up — the house rule prescribes binary-opacity whip-in, adopt the house form) or `spring-pop-entrance` (staggered group) for card-like rows
- typed lines — verifier summary, handoff line, document title, follow-up question, opening headline → `discrete-text-sequence` (+ `context-sensitive-cursor` for the trailing caret)
- file-attachment chip pop-in / tooltip pill pop / chat-bubble arrival → `spring-pop-entrance`
- cursor glides in, lands, clicks (hinge and coda) → `cursor-click-ripple` (+ `physics-press-reaction` to compress cursor and target together on the press)
- selection-highlight sweep across the sentence → `css-marker-patterns` (highlight sweep)
- ONE push-in onto the highlighted selection / slow push-in + lateral pan settling on highlighted cells → `coordinate-target-zoom` (measured off-center target — the lateral pan IS the counter-translate component), sequenced under `multi-phase-camera` when it follows the window scale-up
- fast decelerating zoom-OUT to the full workspace → `coordinate-target-zoom` (zoom-out variation: open at the zoomed-in framing, pull to scale 1 with `power3.out`/`power4.out`) or `viewport-change` (single continuous pull on the `cam` object)
- artifact window scales up from small toward full frame on the click → `spring-pop-entrance` (hero arrival scale-up; tune overshoot to ~0 / `power3.out` so the window reads weighty, not bouncy)
- collapsible row expands into a sub-task stack / inline panel expands below the highlighted line → `anchored-layout-expand` (in-flow accordion growth pushing subsequent content DOWN — never tween width/height) + `waterfall-entry` (or `spring-pop-entrance` stagger) on the arriving children
- phone-screen instant swap on the coda tab click → `discrete-text-sequence` (discrete whole-state swap; instant, no in-artifact camera move)
- green verb highlights, model-tag pills, check-circle strikethroughs, yellow forecast cells, edge fade masks → static styling of the surface content — no motion rule needed

**camera modifier**: The blueprint's camera law: **at most TWO real camera moves, bracketing the hinge** — the goldens are emphatic (their briefs carry CRITICAL camera notes). Pick the traversal mechanic first: camera pan (`viewport-change` pan — pan-to-workspace only) OR element scroll (`3d-page-scroll` flat — all others); never both at once. The reveal then spends the second (or only) move: one zoom-OUT to the workspace or one push-IN to the detail (`coordinate-target-zoom`, phases sequenced by `multi-phase-camera`), after which the frame LOCKS — all remaining motion is element-level (typing, expand, screen swap). The feed-rush variant spends zero camera moves: the whole shot is element scroll + expand. This restraint is what separates the shape from `cursor-ui-demo` (camera servos to every interaction) and from `device-surface-showcase` (a showcase camera presenting a held hero).

**Overflow (scrolled/panned surfaces — required for a clean `check`):** the traversal deliberately moves content past the frame edges. Clip at the scene (`overflow: hidden`) AND mark the moving inner layer (the `.page-content` / `.world` wrapper carrying the transcript/feed/document) with `data-layout-allow-overflow` — otherwise `check` reports `text_box_overflow` / `container_overflow` for every row that has scrolled off. The clip handles it visually; the attribute tells the layout audit it's intentional.

## Selected motion rule: 3d-page-scroll

---
name: 3d-page-scroll
description: Full webpage rendered as tilted 3D card that scrolls to reveal specific sections.
metadata:
  tags: 3d, page, scroll, webpage, tilt, product-demo, perspective
---

# 3D Page Scroll

A webpage (or long content) presented as a tilted 3D card. Spring-eased scroll reveals specific sections while the static 3D perspective adds physical depth. (For a camera that actually travels/tilts, see [3d-camera-flight.md](3d-camera-flight.md) — this rule's tilt never moves.)

## How It Works

Two independent transforms combine:

1. **3D tilt** — static `rotateY` + `rotateX` with `perspective` on the card. The angle does **not** change during the scene.
2. **Scroll** — the content inside the card translates vertically (`y` in GSAP) within a clipped container; spring-like deceleration via `power3.out` / `power4.out`.

Optional: **spotlight overlay** — a radial-gradient mask dims everything except a focal region after the scroll lands. It sits above the scrolling content, fixed relative to the card, never inside `.page-content`.

## Recipe

```html
<div class="tilt-card">
  <div class="page-content">
    <!-- Full {Brand} webpage recreation, taller than the card so scrolling
         matters. Each section is REAL DOM, not a screenshot — screenshots
         can't be individually highlighted or scrolled-to with precision. -->
    <section class="page-hero">{heroContents}</section>
    <section class="page-features">{featuresContents}</section>
    <section class="page-target" id="target-section">{targetContents}</section>
    <section class="page-cta">{ctaContents}</section>
  </div>
  <div class="spotlight"></div>
</div>
```

```css
.tilt-card {
  position: absolute;
  left: 50%;
  top: 50%;
  /* tilt + perspective in CSS only if no other transform tween touches this
     element — if GSAP also tweens scale on .tilt-card, set the tilt via
     gsap.set() instead to avoid matrix overwrites */
  transform: translate(-50%, -50%) perspective({perspectivePx}) rotateY({tiltYDeg}) rotateX({tiltXDeg});
  transform-style: preserve-3d;
  width: {cardWidth};
  height: {cardHeight};
  border-radius: 24px;
  background: {cardBackgroundColor};
  overflow: hidden; /* clip the scrolling content at the rounded corners */
  /* shadow X-offset sign must match tiltY sign (negative tiltY ⇒ positive X) */
  box-shadow: 40px 30px 80px rgba(0, 0, 0, 0.45);
}
.page-content {
  position: absolute;
  top: 0;
  left: 0;
  width: 100%;
  /* height intrinsic from sections — taller than the card */
}
.spotlight {
  position: absolute;
  inset: 0;
  pointer-events: none;
  opacity: 0;
  background: radial-gradient(ellipse 60% 35% at 50% 50%, transparent 50%, {spotlightDimColor} 100%);
}
```

```js
// SCROLL_DISTANCE is measured at design time from the real page layout
// (top of .page-content origin to vertical center of #target-section,
// accounting for card height) — NOT a free tunable.
tl.to(
  ".page-content",
  { y: -SCROLL_DISTANCE, duration: SCROLL_DUR, ease: "power3.out" },
  SCROLL_AT,
);

// Spotlight fades in on the target after the scroll settles.
tl.to(
  ".spotlight",
  { opacity: 1, duration: SPOTLIGHT_FADE_DUR, ease: "power1.inOut" },
  SPOTLIGHT_AT,
);
```

## Variations

**Multi-step scroll (scroll → pause → scroll)** — multiple `y:` tweens at different positions. Distances are both measured from the `.page-content` origin (NOT delta from the previous step); GSAP composes successive `y:` tweens on the same property, each starting from the value the previous one left:

```js
tl.to(
  ".page-content",
  { y: -SCROLL_DISTANCE_A, duration: SCROLL_DUR, ease: "power3.out" },
  SCROLL_AT_A,
);
tl.to(
  ".page-content",
  { y: -SCROLL_DISTANCE_B, duration: SCROLL_DUR, ease: "power3.out" },
  SCROLL_AT_B,
);
// SCROLL_AT_A + SCROLL_DUR ≤ SCROLL_AT_B — the two scrolls must not fight for y
```

## Values

| token              | range / rule                                                              | notes                                                                                 |
| ------------------ | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| tiltYDeg           | −12 to −4 (left-leaning) or 4 to 12                                       | bigger = more dramatic 3D; near 0 collapses to a flat panel                           |
| tiltXDeg           | 0–6                                                                       | positive tilts the top edge away                                                      |
| perspectivePx      | 800–2000 px                                                               | smaller = more foreshortening; larger = nearly orthographic                           |
| cardWidth / Height | card height < total content height                                        | otherwise the scroll has nothing to reveal                                            |
| sectionHeight      | Σ heights ≥ cardHeight + SCROLL_DISTANCE                                  | so the target section lands within frame                                              |
| SCROLL_AT          | ≥ end of prior tweens on `.page-content`                                  |                                                                                       |
| SCROLL_DUR         | 0.8–1.8 s                                                                 | shorter feels like a hard cut; longer feels programmatic                              |
| SCROLL_DISTANCE    | measured from the layout                                                  | from actual cumulative section heights — never estimated; don't overshoot content end |
| SPOTLIGHT_AT       | ≥ SCROLL_AT + SCROLL_DUR (or slightly earlier)                            | spotlight reveals the freshly-arrived section                                         |
| SPOTLIGHT_FADE_DUR | 0.4–0.8 s                                                                 |                                                                                       |
| Ease               | `power3.out` default; `power4.out` momentum; `power2.inOut` cinematic pan | pick ONE for all scrolls in the scene — mixing easings reads as jerky                 |

## Critical Constraints

- **Tilt is static** — the card holds its angle the whole scene.
- **Shadow direction matches tilt** — a left-leaning card casts shadow to the right (positive X offset); mismatch breaks the 3D illusion.
- **Page content is real HTML, not a screenshot**; scroll distances come from the real layout geometry.
- **`overflow: hidden` + `transform-style: preserve-3d` on `.tilt-card`** — clip at the rounded corners; preserve-3d for any 3D children / clean perspective composition.
- **Spotlight is an overlay above the scrolling content**, never inside `.page-content`.
- **Same easing across a multi-phase scroll**, and non-overlapping scroll windows.

## See also

[asr-keyword-glow.md](asr-keyword-glow.md) (on-page keyword highlight synced to VO) · [multi-phase-camera.md](multi-phase-camera.md) (camera zoom while the page scrolls) · [cursor-click-ripple.md](cursor-click-ripple.md) (cursor lands in the scrolled-into-view section) · [3d-camera-flight.md](3d-camera-flight.md) (when the camera itself should travel).

## Selected motion rule: anchored-layout-expand

---
name: anchored-layout-expand
description: Edge-pinned container grows (or collapses) along ONE axis and in-flow content reflows with it — a pill springs open downward into a dropdown, a panel grows a sub-task stack, an input card stretches as typed text wraps, a pane expands over a neighbor. Transform-only (mask + slide, or proxy-driven scaleY + counter-scale) because width/height tweens are forbidden; the push on subsequent content is a matched translate on the same tween.
metadata:
  tags: expand, collapse, anchored, dropdown, menu, accordion, panel, reflow, push, mask, counter-scale, layout
---

# Anchored Layout Expand

> The law: **author the layout at its final (expanded) state in CSS, then fake the collapsed state with transforms.** The container never changes size — the _visible_ region does — and everything downstream rides a matched translate. The browser computes layout ONCE; every intermediate frame is pure transform.

THE one-axis growth primitive: a container pinned at one edge appears to grow along a single axis, and the in-flow content after it moves in perfect contact with the traveling edge — dropdown, sub-task stack, growing composer card, pane widening over a neighbor. Growth and push are ONE motion: if the panel's bottom edge and the pushed content ever separate or overlap, the illusion dies.

Distinct from [card-morph-anchor.md](card-morph-anchor.md) (a free-floating two-shot morph with no neighbors to push — this rule's container is a live layout participant), [spring-pop-entrance.md](spring-pop-entrance.md) (arrival at a point, no edge travel or reflow), and [reactive-displacement.md](reactive-displacement.md) (displacement by a colliding intruder; here content moves because the container's edge reached it — layout causality, not collision).

## How It Works

1. **Mask** — a wrapper at the final body height (`BODY_H`), `overflow: hidden`. Never tweened.
2. **Sheet** — the panel surface + content inside the mask, starting at `y: -BODY_H` (tucked above the mask window, behind the pinned header).
3. **Below** — ONE wrapper holding everything after the container, also starting at `y: -BODY_H`.
4. **Grow** — ONE `fromTo` drives sheet AND below from `y: -BODY_H → 0`. Shared tween ⇒ the descending bottom edge and the pushed content stay in exact contact by construction. Collapse = the same pair tweened back.

When the surface must visibly **stretch in place** (rows revealed top-first, or a pane growing sideways), use the proxy counter-scale variant below instead.

## Recipe

```html
<!-- inside a standard scene clip (hyperframes-core) -->
<div class="stack">
  <div class="expander">
    <div class="expander-head">{headerLabel}</div>
    <div class="expand-mask" id="expand-mask" data-layout-allow-overflow>
      <div class="expand-sheet" id="expand-sheet">
        <div class="expand-row">{rowA}</div>
        <div class="expand-row">{rowB}</div>
      </div>
    </div>
  </div>
  <!-- EVERYTHING that must be pushed lives in this one wrapper -->
  <div class="below" id="below">{followingContent}</div>
</div>
```

```css
/* Layout is the EXPANDED end state — no collapsed geometry exists in CSS. */
.expander-head {
  position: relative;
  z-index: 2; /* the sheet slides out from UNDER the header */
}
.expand-mask {
  height: BODY_H; /* authored final height — NEVER tweened */
  overflow: hidden;
}
.expand-sheet {
  height: BODY_H;
  border-radius: 0 0 SHEET_RADIUS SHEET_RADIUS; /* bottom-only — header + sheet read as one grown card */
  will-change: transform; /* + on .below */
}
```

```js
// BODY_H must equal the mask's CSS height exactly — measure once at build.
// (Montage caveat: per the contract, in a multi-scene master use an authored
// CSS-matched constant instead — later clips may not be laid out yet.)
const BODY_H = document.querySelector("#expand-mask").offsetHeight;

// The grow: ONE tween, BOTH sides of the seam.
tl.fromTo(
  ["#expand-sheet", "#below"],
  { y: -BODY_H },
  { y: 0, duration: GROW_DUR, ease: GROW_EASE },
  GROW_AT,
);

// Garnish: rows already ride the sheet; the fade stagger makes them read as "options arriving".
tl.fromTo(
  ".expand-row",
  { opacity: 0 },
  { opacity: 1, duration: ROW_FADE_DUR, stagger: ROW_STAGGER, ease: "power2.out" },
  GROW_AT + GROW_DUR * 0.25,
);

// Collapse — same machinery back; faster (closing is a snap decision).
tl.fromTo(
  ["#expand-sheet", "#below"],
  { y: 0 },
  { y: -BODY_H, duration: COLLAPSE_DUR, ease: "power3.in", immediateRender: false },
  COLLAPSE_AT,
);
```

## Variations

- **Proxy counter-scale — surface stretches in place** (rows revealed top-first holding their screen positions; the "payload card expands from the tool-call line"). Drive mask `scaleY` and the sheet's exact inverse from ONE proxy — two independent tweens are wrong: eased midpoints of `s` and `1/s` are not inverses and the content squashes mid-grow. Net content scale is `s × 1/s = 1` every frame; seek-safe because everything derives from the one interpolated proxy.

  ```js
  const grow = { h: COLLAPSED_H }; // 0 for fully collapsed
  tl.fromTo(
    grow,
    { h: COLLAPSED_H },
    {
      h: BODY_H,
      duration: GROW_DUR,
      ease: GROW_EASE,
      onUpdate: () => {
        const s = Math.max(grow.h / BODY_H, 0.0001); // clamp: no divide-by-zero
        gsap.set("#expand-mask", { scaleY: s, transformOrigin: "50% 0%" });
        gsap.set("#expand-sheet", { scaleY: 1 / s, transformOrigin: "50% 0%" });
        gsap.set("#below", { y: grow.h - BODY_H });
      },
    },
    GROW_AT,
  );
  ```

- **One-axis pane expand (X)**: same machinery rotated 90° — pin the left edge, sheet from `x: -PANE_W` (or proxy `scaleX` + counter-scale, origin `0% 50%`). Decide the neighbor's fate explicitly: **overlap** (pane paints over it, no neighbor tween) or **push** (neighbor rides the same tween). Never both.
- **Typed-wrap growth** — the composer card gets taller as typed text wraps. Quantize: one short step per wrap boundary, each moving the pair by one `LINE_H`; wrap times come from the deterministic typing schedule ([discrete-text-sequence.md](discrete-text-sequence.md)), never measured at render time. Two battle-tested traps:
  - **Composer cards have no pinned header** — a composer grows from its TOP edge (the send-button footer stays put), so a plain y-step clips the card's top out of the mask. Combine the proxy counter-scale with the wrap quantization (step the proxy by `LINE_H` at each wrap time) and split the surface into a **sheet** (carries the top radius) + **footer** (carries the bottom radius) so the growth seam stays invisible.
  - **Wrap TIME vs wrap POSITION are two different authorities** — the typing schedule decides _when_ a wrap fires, the browser's line-breaking decides _where_ text actually wraps, and with proportional fonts they silently disagree. Author an explicit `\n` in the typed string (with `white-space: pre-wrap`) at the chosen split point so both derive from the same authored fact.
- **Springy open** (rare, explicitly-playful): `back.out(1.2)` — the edge overshoots a few px; the pushed content bounces with the panel (correct — they're in contact). Default stays `power3.out`.
- **Row grows a sub-task stack**: the row is the pinned header, the stack is the sheet, every later row lives in `#below`; chain several scopes for progressive disclosure.
- **FLIP hand-off**: if the container also TRAVELS to a new layout slot while resizing (prompt promoted to heading, card docking into a sidebar), that's a FLIP problem — `/hyperframes-keyframes` (FLIP recipes). This rule stays the in-place one-axis specialist.

## Values

| token                    | range                       | notes                                                                 |
| ------------------------ | --------------------------- | --------------------------------------------------------------------- |
| BODY_H                   | measured / authored         | drift from the CSS height = visible gap or overlap at full open       |
| GROW_AT                  | trigger beat + 0–0.1s       | growth needs a cause (click / wrap / status beat) or it reads haunted |
| GROW_DUR                 | 0.35–0.6s                   | below ~0.3s the pushed content appears to teleport                    |
| GROW_EASE                | `power3.out` default        | `back.out(1.1–1.3)` only for the playful register                     |
| ROW_STAGGER / \_FADE_DUR | 0.04–0.08s / 0.2–0.3s       | start rows ~25% into the grow so none flash inside a closed panel     |
| COLLAPSE_DUR             | 0.2–0.35s, `power3.in`      | faster than open                                                      |
| STEP_DUR / LINE_H        | 0.12–0.2s / CSS line-height | typed-wrap variant; WRAP_TIMES from the typing script                 |

## Critical Constraints

- **NEVER tween `width` / `height` / `top` / `left` / `margin` / `padding`** — the mask's height is a CSS constant; only its children transform. Tweening the mask IS the forbidden move this rule replaces.
- **`data-layout-allow-overflow` on the mask** — the collapsed phase parks the sheet outside the mask's box by construction, which trips the `hyperframes check` layout gate (`container_overflow`). The flag is the sanctioned waiver: this overflow is the technique working as designed, not a bug.
- **Sheet + below share one tween (or one proxy)** — matched-but-separate tweens on the two sides of the contact edge are the classic seam bug.
- **Everything downstream rides `#below`** — content outside the wrapper is overlapped at t=0 and orphaned during the grow.
- **`overflow: hidden` on the mask** — without it the tucked sheet is visible above the header at t=0.
- **Counter-scale needs a proxy**, clamped `s ≥ 0.0001` (a fully-collapsed body divides by zero).
- **Deterministic sizes** — `BODY_H`, `LINE_H`, `WRAP_TIMES` are build-time constants or one-time measurements, never per-frame layout reads.

## See also

`cursor-click-ripple` (the igniting click) · `spring-pop-entrance` (richer per-row arrivals) · `discrete-text-sequence` (the typing that drives stepped growth) · `scale-swap-transition` (the grown menu's exit) · `/hyperframes-keyframes` FLIP (grow + travel).

## Selected motion rule: cursor-click-ripple

---
name: cursor-click-ripple
description: Animated mouse cursor moves to target, clicks with scale depression and expanding ripple rings.
metadata:
  tags: cursor, click, ripple, interaction, mouse, button
---

# Cursor Click Ripple

An animated cursor moves to a target element, performs a click with visual depression, and emits expanding ripple rings from the click point. Three sequential phases on one timeline: **move** (eased translation to the target's center) → **click** (scale depression on cursor + target together, yoyo back) → **ripple** (1–3 staggered rings expand and fade from the click point). This is a _point event at one location_ — a sustained hold across space is [cursor-drag.md](cursor-drag.md).

## Recipe

```html
<button class="target-button">{ctaLabel}</button>
<div class="cursor"><!-- arrow SVG, positioned at the entry corner --></div>
<!-- Rings live in DOM from t=0 at the click-target CENTER, scale 0 + opacity 0 -->
<div class="ripple ripple-1"></div>
<div class="ripple ripple-2"></div>
<div class="ripple ripple-3"></div>
```

```css
.ripple {
  position: absolute;
  left: 50%;
  top: 50%; /* click-target center */
  width: 100px;
  height: 100px;
  border-radius: 50%;
  border: 2px solid {rippleColor};
  transform: translate(-50%, -50%) scale(0);
  opacity: 0;
  pointer-events: none;
}
```

```js
// Phase 1 — Move: eased, not linear
tl.to(".cursor", { x: TARGET_X, y: TARGET_Y, duration: MOVE_DUR, ease: MOVE_EASE }, 0);

// Phase 2 — Click: cursor + target depress together, then return
tl.to(
  ".cursor",
  { scale: CURSOR_PRESS_SCALE, duration: PRESS_DUR, ease: "power2.in", yoyo: true, repeat: 1 },
  CLICK_AT,
);
tl.to(
  ".target-button",
  { scale: TARGET_PRESS_SCALE, duration: PRESS_DUR, ease: "power2.in", yoyo: true, repeat: 1 },
  CLICK_AT,
);

// Phase 3 — Ripple burst, N rings staggered from the click point
tl.set([".ripple-1", ".ripple-2", ".ripple-3"], { opacity: 1 }, RIPPLE_AT);
tl.to(
  [".ripple-1", ".ripple-2", ".ripple-3"],
  {
    scale: RIPPLE_SCALE,
    opacity: 0,
    duration: RIPPLE_DUR,
    ease: RIPPLE_EASE,
    stagger: RIPPLE_STAGGER,
    immediateRender: false, // holds scale 0 / opacity 0 until the click moment
  },
  RIPPLE_AT,
);
```

## Variations

- **Single ring** — one `.ripple`, no stagger; more elegant when the rest of the scene is busy.
- **Keyframed attack-decay** — a `keyframes` block ramps opacity 0 → peak → 0 across the duration; a clearer "energy radiates and dissipates" envelope.
- **Multi-ring expanding pulse** — 3 rings at 0.08 s stagger when the click is the scene's climactic moment.

## Values

| token                       | range                       | notes                                                                                                                                  |
| --------------------------- | --------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| MOVE_DUR                    | 0.4–1.0 s                   | short darts; long reads as a "considered click." Must end before CLICK_AT or it reads as a misclick                                    |
| MOVE_EASE                   | discrete choice             | `power2.inOut` calm · `power3.out` decisive · `back.out(1.2–1.4)` settles onto the button with a tiny recoil (higher reads cartoonish) |
| CLICK_AT                    | `MOVE_DUR + 0–0.3 s`        | zero pause reads as autopilot; >0.3 s reads as hesitation                                                                              |
| PRESS_DUR                   | 0.06–0.12 s (half; yoyo ×2) | short crisp, long mushy; must finish before the next phase needs normal scale                                                          |
| CURSOR / TARGET_PRESS_SCALE | 0.80–0.90 / 0.92–0.97       | cursor compresses MORE than the target — the cursor is the actor, the target the recipient                                             |
| RIPPLE_AT                   | `CLICK_AT + 0–0.08 s`       | simultaneous feels causal; slight delay feels acoustic                                                                                 |
| RIPPLE_DUR                  | 0.5–1.0 s                   | sharp ping vs soft sonar; must complete before anything that needs the ring gone                                                       |
| RIPPLE_SCALE                | 3–6                         | 3 stays near the click site; if the ring would exit the frame before fading, lower it                                                  |
| RIPPLE_STAGGER              | 0.06–0.12 s (or 0)          | below ~0.06 s reads as one thick ring; above ~0.12 s as separate events                                                                |
| RIPPLE_EASE                 | discrete choice             | `power2.out` standard ping · `power3.out` sharper attack · `expo.out` strong distant pulse                                             |
| TARGET_X / TARGET_Y         | layout-derived              | must match the target's visual centroid — a 4 px miss reads as missing the button                                                      |

Reference values: `../../examples/cta-orbit-collapse.html` — 0.5 s move on `back.out(1.3)`, click +0.2 s, press 0.08 s at 0.85/0.95, single ring to 5× over 0.7 s `power2.out`.

## Critical Constraints

- **Move before click** — trigger the click only after the move tween settles; clicking mid-motion reads as unintentional.
- **Rings live in DOM from t=0** at the click-target center with `scale: 0` + `opacity: 0` — never conditionally rendered; `immediateRender: false` on the expand so they hold invisible until the trigger.
- **Ripple from the click point** — the button's visual center, not any element's bounding-box origin.
- **Synchronized depression** — cursor + target depress at the same position with the same duration, and both yoyo back.
- **Cursor above all content** (high z-index) for the whole sequence; `pointer-events: none` on cursor + ripples.

## See also

`orbit-3d-entry` (click as the pivot that collapses orbiters) · `center-outward-expansion` (click triggers an outward burst) · `press-release-spring` (stronger physical feel on the target) · `scale-swap-transition` (the button's post-click state change).
