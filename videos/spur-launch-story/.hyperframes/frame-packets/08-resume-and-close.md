# Frame packet: 08-resume-and-close

## Project inputs

- Project: /Volumes/Projects/spur/videos/spur-launch-story
- Design truth: /Volumes/Projects/spur/videos/spur-launch-story/frame.md
- RULES_DIR: /Volumes/Projects/spur/.agents/skills/hyperframes-animation/rules

## Assigned storyboard block

## Frame 8 — Resume with the thread intact

- scene: The session state changes around an unmoving context ledger, then resolves into the SPUR promise
- voiceover: ""
- duration: 7s
- poster: 6.0s
- transition_in: blur-crossfade 0.45s
- status: outline
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

## Selected blueprint: fixed-anchor-cycle

# fixed-anchor-cycle — Fixed Anchor, Cycling World

**intent**: One element is PINNED — a wordmark, a composer box, an anchor line that enters once and never moves again — while the adjacent region (or the entire surrounding theme) cycles through many discrete states around it, cadence often manipulated (steady stepping, a fast carousel, or a slow→accelerating flurry), resolving on an emphasis beat into a completed lockup or a muted freeze. The stillness of the anchor IS the claim: everything changes, this stays. Distinct from `kinetic-type-beats` sub-shape A, where a word-slot inside a centered line swaps and the sentence itself is the subject — there the anchor is a sentence frame on a bare type field; here the anchor is the PRODUCT identity and what cycles around it can be non-text (whole theme skins, chrome/logo swaps, textured label chips, a carousel list), the cycle asserts breadth ("everyone says / works everywhere / calling all X"), and the resolve completes the anchor into a lockup. Distinct from `ticker-takeover`, whose cycle ends in a collision — a hero crashes in and shoves the text aside; here nothing ever collides with the anchor: the cycle stops, and a final element quietly joins it.

**roles served**

- Brand_Outro (from `static-anchor-rapid-text-swaps`): when the sign-off is the brand name sitting immovable while praise quotes / tagline words cycle beside or beneath it — steady per-word highlight stepping, or a hard-cut chip flurry that accelerates — landing on the finished lockup ("bolt.new / prompt, run, edit, deploy / enjoy."; "Opus 4.6 by ANTHROP\C").
- Benefits: when "works everywhere" is shown literally — one product surface (a prompt composer with one verbatim string) pinned dead-center while its ENTIRE shell morphs in place through N product themes (background, typography, radii, chrome, logos all crossfading at once), ending in a washed-out freeze.
- Hook: when the opener is a roll-call — a static anchor line holds while an accent-colored line beneath it runs as a fast vertical carousel through an audience/option list, then the block clears into follow-up statement beats that land the brand line.

**duration**: 6.6–11.1s (Benefits shortest ~6.6s at 4 theme beats; Brand_Outro ~9–9.4s; Hook longest ~11s when the anchor-cycle block hands off to follow-up statement beats). The cycle engine itself occupies ~3–5s regardless of role.

**shot structure** (flat static frame — camera locked in every member; a `[bg]` field, solid or subtly drifting; two folded sub-shapes — **(A) adjacent-region cycle**: the anchor holds and a neighboring slot swaps through N states; **(B) whole-context morph**: the anchor holds and everything AROUND it re-skins in place)

- **Scene 1 (0.0–~2.0s) — the anchor lands and PINS.** The `[anchor: wordmark / product name / composer box / lead line]` enters once — fade/scale-in centered, word-by-word build, or already present at frame one — at a fixed position it will hold for the entire clip. Zero movement from here on: no drift, no breathe, no re-layout. If the anchor is a UI surface (sub-shape B), it carries a `[verbatim string]` with a blinking cursor.

- **Scene 2 (~2.0s–~70% of runtime) — the cycle engine (signature move).** The world changes around the unmoved anchor. Choose by sub-shape:
  - **Sub-shape A (adjacent-region cycle)**: a region beside/beneath the anchor steps through N discrete states — pick ONE swap mechanic and ONE cadence:
    - _swap mechanics_: instant hard-cut label replacement (a `[chip / tape label]` slaps over the old one, texture/highlight shifting slightly, chip width re-fitting each `[phrase]` — growing away from the anchor, never over it); sequential per-word highlight stepping (one word of the `[tagline]` snaps bright/bold while the rest sits dim grey, the highlight walking the line); or a fast vertical carousel (each `[list item]` slide/fades through the accent slot ~0.5s/phrase).
    - _cadences_: steady stepping (~0.5–1s/state), or **slow→accelerating flurry** — ~1s beats compressing to ~0.15–0.3s per swap, breadth escalating into a blur of states (12–16 states read as "everyone"; 3–8 read as a roll-call).
    - Geometry law: the cycling region NEVER overlaps, touches, or displaces the anchor; size the layout so the longest state still fits inside the frame with clear margins.
  - **Sub-shape B (whole-context morph)**: at ~1.3s intervals the entire theme — `[bg color]`, typography, corner radii, toolbar icons, footer `[brand logos]`, contextual lines — morphs in place via quick (~0.3s) crossfades through N `[product skins]`, every property blending simultaneously. No hard cuts, no wipes; the anchor's content string is identical in every skin (chrome details like a `> ` prefix may adapt per skin).

- **Scene 3 (~70–85%) — the emphasis beat.** The cycle resolves — it does not just stop:
  - _Variant — Brand_Outro (highlight stepping)_: the whole `[tagline]` snaps solid bright at once — full-line illumination after the per-word walk.
  - _Variant — Brand_Outro (flurry)_: the flurry halts and HOLDS on the `[longest / weightiest phrase]` — a beat of stillness after acceleration.
  - _Variant — Benefits (theme morph)_: the final beat mutes — a faint `[dot-grid]` fades in across the background while the UI drops to low opacity, a washed-out blueprint freeze.
  - _Variant — Hook (carousel)_: the anchor block clears, handing off to 1–3 centered word-by-word statement beats (kinetic-type-beats territory) that carry toward the close.

- **Scene 4 (final beat → end) — lockup completion and HOLD.** A final element joins the still-unmoved anchor and the finished composition holds static to the end: a `[closing word]` drops in below, aligned to the last cycled state ("enjoy."); the chip vanishes on a hard cut and the `[brand sign-off]` appears beside the anchor on a shared baseline ("by ANTHROP\C"); or the final `[brand line]` builds word-by-word dead-center and holds ("with Copilot."). Long static hold — the lockup is the payoff, give it 20–30% of the runtime.

**motion vocabulary**: anchor fade/scale-in entrance; permanently pinned anchor (zero movement, no idle breathe); instant hard-cut label/chip replacement (slap-over with subtle texture/highlight shift); chip width resize-to-fit per phrase (grows away from the anchor); sequential per-word highlight stepping through a line; dim-to-grey line state; whole-line illumination snap; fast vertical carousel slide/fade of one line under a static line; cadence acceleration (slow ~1s beats into a ~0.15–0.3s flurry); hold-on-longest-phrase emphasis beat; in-place theme morph crossfade (~0.3s) blending background/fonts/radii/icons simultaneously; per-beat chrome/logo swap; blinking text cursor; contextual line appearing/disappearing across beats; dot-grid backdrop fade-in; global opacity washout; end freeze; word-by-word phrase build; block clear between scenes; drop-in entrance of a final word; hard cut to final lockup; long static hold.

**rule mapping**

- instant hard-cut chip/label/phrase swaps at time thresholds; per-word highlight stepping (color/weight state swaps); dim-line → full-line illumination snap; per-state chip width set (a per-state layout property, set discretely — never tweened) → `discrete-text-sequence`
- fast vertical carousel of the accent line under the static anchor (slide/fade stepped swaps in a masked slot) → `vertical-spring-ticker` (its footer-reveal step unused — Scene 4's lockup takes its place)
- per-phrase state windows computed from a script of N states (praise quotes, audience list, theme beats) → `dynamic-content-sequencing` (Accelerating cadence — for the flurry, pre-compute the beat array with shrinking `hold` values, geometric decay over the state list)
- word-by-word phrase builds (anchor line, follow-up statements, final brand line) → `dynamic-content-sequencing` + `waterfall-entry` (or `kinetic-beat-slam` when the statements should land percussively)
- anchor entrance fade/scale-in; drop-in of the final closing word → `spring-pop-entrance` (restrained overshoot — the register here is editorial, not bouncy)
- blinking cursor in the pinned composer → `context-sensitive-cursor` (color adapts per theme skin at segment boundaries)
- whole-context theme morph → `theme-crossfade-morph` (N pre-styled full-scene layers stacked at the same geometry, opacity-crossfaded, the shared anchor string rendered once on top); the composer shell's radius/surface component alone → `card-morph-anchor`
- subtly drifting background field beneath the cycle → `sine-wave-loop` (bounded drift; the anchor itself gets none)
- dot-grid fade-in + global opacity washout freeze; long static hold → `gsap-effects` (plain opacity tweens) / static hold (no rule needed)

**camera modifier**: none — every member is fully camera-static; the cycle is the only motion, and the pinned anchor's stillness is load-bearing. Do not add a push-in "for energy"; it would break the anchor contract.

## Selected motion rule: theme-crossfade-morph

---
name: theme-crossfade-morph
description: Whole-theme in-place morph under a fixed anchor — background, typography, corner radii, icons, chrome and logos all blend simultaneously (~0.3s) through N pre-styled skins while one anchor element never moves. Recipe = stacked full layers + opacity crossfade, anchor rendered once on top. Seek-safe by construction.
metadata:
  tags: theme, skin, crossfade, morph, anchor, reskin, cycle, ui, stacked-layers
---

# Theme Crossfade Morph

The whole world re-skins while one thing holds still. A composer box cycles through four IDE themes; a checkout widget flips through brand skins — background, typography, corner radii, toolbar icons, footer logos all change **at once**, in place, in ~0.3s, N times — and through every flip one anchor element (the prompt string, the widget layout, the wordmark) **never moves**. The anchor's stillness is the rhetorical claim: _everything changes, this doesn't._

Boundary: [card-morph-anchor.md](card-morph-anchor.md) morphs **one container** between two shots — its dimensions, radius, and surface tween continuously. This rule re-skins an **entire scene** through **N discrete states**: nothing tweens property-by-property (fonts, icons, and logos can't interpolate); the "morph" is a fast simultaneous crossfade of complete pre-styled layers. ([scale-swap-transition.md](scale-swap-transition.md) swaps an element at center; here the surroundings swap and the element holds.)

## How It Works

1. **One skin = one complete layer.** Each theme state is a fully pre-styled, full-bleed layer (`position: absolute; inset: 0`) containing everything that changes: background, shell/chrome, toolbar icons, footer logos, typography. All `N_SKINS` layers exist in the DOM from `t=0`, stacked; skin 0 starts visible, the rest at `opacity: 0`.
2. **The morph is a crossfade.** At each boundary, two opposing opacity tweens run at the same timeline position over `MORPH_DUR` (~0.3s): outgoing `1 → 0`, incoming `0 → 1`. Because both layers are complete, every property "blends" simultaneously for free — including the un-tweenable ones (font families, icon glyphs, logos), which read as morphing precisely because everything else is mid-blend around them.
3. **The anchor renders once, on top.** The element that must not move lives in its own layer above all skins and is **excluded from every skin layer**. No transforms, no re-parenting, no per-skin restyle.
4. **Windows are precomputed.** `T_k = CYCLE_START + k × (SKIN_HOLD + MORPH_DUR)`. Steady cadence by default; hold the final skin longest when it's the resolve.

The only animated property is `opacity` — which is why this rule is seek-safe with zero special machinery.

## Recipe

```html
<!-- inside a standard scene clip (hyperframes-core) -->
<div class="theme-stage">
  <!-- One complete pre-styled layer per skin; skin-0 visible at t=0 -->
  <div class="skin skin-0"><div class="shell">…terminal chrome, mono type, footer badge…</div></div>
  <div class="skin skin-1">
    <div class="shell">…rounded composer, sans type, toolbar pills, logo…</div>
  </div>
  <div class="skin skin-2"><div class="shell">…dark shell, its own chrome and footer…</div></div>

  <!-- The anchor: rendered ONCE, above every skin. It never moves. -->
  <div class="anchor" id="anchor">{anchorText}</div>
</div>
```

```css
.theme-stage {
  position: absolute;
  inset: 0;
}
.skin {
  position: absolute;
  inset: 0;
  opacity: 0;
  /* Each skin fully self-styled: its own background, fonts, radii,
     icons, chrome, logos. Nothing inherited across skins. */
}
.skin-0 {
  opacity: 1; /* the opening state — matches the timeline's fromTo */
}
.shell {
  /* CRITICAL: shared geometry. The shell box (and any element that
     "persists" across skins — toolbar row, footer row) sits at the SAME
     coordinates in every skin, so mid-blend frames read as one UI
     changing clothes, not two UIs ghosting. */
  position: absolute;
  left: SHELL_LEFT;
  top: SHELL_TOP;
  width: SHELL_WIDTH;
  height: SHELL_HEIGHT;
}
.anchor {
  position: absolute;
  z-index: 10; /* above every skin */
  left: ANCHOR_LEFT;
  top: ANCHOR_TOP;
  /* No transforms, no transitions — the stillness is load-bearing. */
}
```

```js
const skins = gsap.utils.toArray(".skin");

// Boundary k→k+1 at T_k: outgoing fades down as incoming fades up —
// ONE simultaneous crossfade, everything blends at once.
skins.forEach((skin, k) => {
  if (k === 0) return; // skin-0 is the opening state
  const at = CYCLE_START + k * (SKIN_HOLD + MORPH_DUR);
  tl.fromTo(skin, { opacity: 0 }, { opacity: 1, duration: MORPH_DUR, ease: "power2.inOut" }, at);
  tl.to(
    skins[k - 1],
    { opacity: 0, duration: MORPH_DUR, ease: "power2.inOut" },
    at, // same position — the blend is simultaneous, never sequential
  );
});

// The anchor gets NO tweens. Its absence from the timeline is the point.
```

## Variations

- **Anchor-typography reskin (per-layer copies)** — when the anchor's own type treatment must change with the theme (mono in the terminal skin, sans in the editor skin), each skin carries its own copy of the anchor at **pixel-identical geometry** and there is no separate top layer; the invariant shifts from "one element" to "one geometry." Verify the copies overlay exactly (screenshot two skins at 50% opacity) — a 2px baseline drift reads as the anchor flinching, which breaks the whole claim.
- **Skin-cycle tour with logo relay** — a large brand logo outside the anchored shell crossfades **in the same windows** as the skins (logo k with skin k, same `MORPH_DUR`). The paired swap sells "same product, every brand."
- **Washout finale** — after the last skin, a final low-key layer (faint dot-grid, blueprint wash) fades in while the last shell drops to ~0.25 opacity — the cycle resolves into a held diagram of itself. One extra window; the anchor may fade with the shell or hold full-strength.
- **Emphasis brake** — steady cadence for `N−1` skins, then hold the final skin 2–3× `SKIN_HOLD`; the cycle demonstrates breadth, the brake lands the resolve. Precompute the hold array; don't drift the cadence without cause.

## Values

| token           | range                                       | notes                                                                                                |
| --------------- | ------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| N_SKINS         | 3–5                                         | two is a before/after (consider `card-morph-anchor`); past five the cycle pads                       |
| SKIN_HOLD       | 0.8–1.5s                                    | long enough to register the logo/footer identity, short enough to keep the churn rhetorical          |
| MORPH_DUR       | 0.25–0.4s, ~0.3s canonical                  | faster reads as a hard cut; slower reads as a mushy dissolve with lingering double-exposure          |
| CYCLE_START     | ≥ anchor settle + a beat                    | after the anchor and skin-0 have fully registered                                                    |
| SHELL geometry  | —                                           | shell / toolbar / footer coordinates identical across skins; contents inside the slots differ freely |
| ANCHOR position | —                                           | identical to the pixel across the scene (per-layer form: identical in every skin)                    |
| washout / brake | shell ~0.2–0.3 opacity; hold 2–3× SKIN_HOLD | —                                                                                                    |

## Critical Constraints

- **The anchor never moves.** No transforms, no opacity dips, no re-parenting, no restyle — the contrast between total churn and total stillness is the entire device; one flinch and the shot becomes a slideshow.
- **Nothing tweens but `opacity`** — no `borderRadius` / `background` tweens; radii and colors change by being different in the next layer. Visibility via `opacity` only, never `display` / `visibility` toggles (they can't blend mid-fade).
- **Pixel-align the shared geometry** — mid-blend both skins are partially visible; aligned shells read as one UI changing clothes, misaligned shells ghost into two UIs.
- **Pre-style everything** — each skin is complete and static; no class toggling, no runtime restyle mid-tween.
- **Outgoing and incoming tweens share one timeline position** — a staggered blend flashes the stage background between skins.
- **Adjacent windows only** — skin k crossfades with k+1, never k+2; at no frame are three skins partially visible.
- **Camera static — always.** A push-in on top of a theme cycle destroys the stillness that makes the anchor read.
- **Hard cuts are the cheaper sibling** — if the states should _snap_, that's `discrete-text-sequence` territory; the ~0.3s blend is specifically the "morph" read.

## See also

`context-sensitive-cursor` (caret color switches at each `T_k`) · `discrete-text-sequence` (type the anchor first; or the hard-cut alternative) · `card-morph-anchor` (the single-container sibling) · `spring-pop-entrance` (the lockup that joins the anchor at the resolve) · `sine-wave-loop` (drifting field under the cycle — never on the anchor).
