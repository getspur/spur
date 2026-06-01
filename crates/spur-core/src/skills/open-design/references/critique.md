# Open Design — Self-Critique & Anti-AI-Slop

## 5-dimensional critique (run before finalizing the artifact)

Score yourself silently 1–5 on each. Any dimension under 3/5 is a regression — go
back, fix the weakest, re-score. Two passes is normal.

1. **Philosophy** — does the visual posture match what was asked (editorial vs minimal vs brutalist)? Or did you drift back to your favourite default?
2. **Hierarchy** — does the eye land in one obvious place per screen? Or is everything competing?
3. **Execution** — typography, spacing, alignment, contrast — right, or just close?
4. **Specificity** — is every word, number, image specific to *this* brief? Or did generic stat-slop creep in?
5. **Restraint** — one accent used at most twice, one decisive flourish — or three competing flourishes?

## Anti-AI-slop checklist (audit before shipping)

- ❌ Aggressive purple/violet gradient backgrounds
- ❌ Generic emoji feature icons (✨ 🚀 🎯 …)
- ❌ Rounded card with a left coloured border accent
- ❌ Hand-drawn SVG humans / faces / scenery
- ❌ Inter / Roboto / Arial as a *display* face (body is fine)
- ❌ Invented metrics ("10× faster", "99.9% uptime") without a source
- ❌ Filler copy — "Feature One / Feature Two", lorem ipsum
- ❌ An icon next to every heading
- ❌ A gradient on every background

When you don't have a real value, leave an honest placeholder (`—`, a grey block, a
labelled stub) instead of inventing one. An honest placeholder beats a fake stat.

## Deck-specific checks (run for `kind: deck`)

Apply these in addition to the 5-dimensional critique:

- **One idea per slide** — if a slide makes two points, split it.
- **Readable from the back row** — headlines ≥ 36px, body ≥ 22px.
- **Theme rhythm** — no 3+ consecutive slides on the same layout; break up content slides
  with `section` covers.
- **Slide counter present** — the audience can always see position (native present mode shows it).
- **Speaker notes, not slide clutter** — move detail into `jute_deck.speaker_notes`, keep the
  slide sparse.
- **One accent, used sparingly** — same restraint as the anti-AI-slop checklist above.

<!-- test markers: one idea per slide; theme rhythm; slide counter -->
