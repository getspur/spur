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
- ❌ Filler copy: "Feature One / Feature Two", lorem ipsum
- ❌ An icon next to every heading
- ❌ A gradient on every background
- ❌ Em-dash (`—`) or en-dash-as-separator (`–`) in ANY artifact-visible string (see the hard ban below)

When you don't have a real value, leave an honest placeholder (a grey block, a labelled
stub, or `TBD`) instead of inventing one. An honest placeholder beats a fake stat. Do
not reach for a dash glyph as the placeholder; see the em-dash ban below.

## Em-dash ban (zero tolerance, artifact-visible text)

The em-dash (`—`) and the en-dash used as a separator (`–`) are the single most
reliable AI tell. They are banned from every user-visible string the artifact renders:
headlines, body copy, eyebrows, button labels, captions, quote attribution, alt text,
AND error / status strings. There is no "use sparingly" allowance. The rule is binary:
zero dashes of this kind in artifact output.

- Restructure instead: a regular hyphen (`-`), a comma, a period, parentheses, or a colon.
- Date and number ranges use a hyphen (`2018-2026`, `40-80k`), never an en-dash.
- The only dash characters allowed in artifact text are the hyphen `-` (compounds, ranges, dividers) and the math minus.
- This scopes to the rendered artifact, not this critique file's own prose.

**Mechanical check before finalizing:** scan the cell's rendered `text/html` output,
including JavaScript string literals and HTML entities (`&mdash;`, `&#8212;`, `&ndash;`,
`&#8211;`), for `—` / `–`. Any hit fails the artifact; fix it and re-read the cell.

## Self-contained and script-degradation (mandatory)

The artifact renders inside a sandboxed iframe, and active content (scripts) is **off by
default** (`output.activeContent = false`). With scripts off the iframe runs zero
JavaScript and shows only static HTML and CSS. The iframe also has no same-origin access
and the host sets no CSP, so anything fetched at render time (CDN scripts, `esm.sh`
imports, `cdn.tailwindcss.com`, a network-only web font) is unreliable.

Two hard rules:

1. **Self-contained.** All CSS and JS live inline in the one HTML document. No external
   `<script src>`, no CDN stylesheet, no network-loaded module, no remote font as the only
   source. If a build step produced the artifact (e.g. a Deno kernel bundling React /
   Tailwind / Motion), the emitted HTML must already have everything inlined.
2. **Meaningful with scripts off.** The static render must carry the design on its own.
   Interactivity (motion, click-to-inspect, cascades) is progressive enhancement that only
   lights up when active content is enabled. A blank or broken artifact with scripts off is
   a fail. For an inherently interactive piece, ship a static baseline (server-rendered
   markup or a representative still state) and tell the user that enabling active content
   unlocks the live behavior.

Check before finalizing: read the cell output, confirm there are no external resource URLs
in the HTML, and confirm the markup alone (no JS) still reads as the intended design.

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

## Artifact-deck checks (run for the artifact track)

In addition to the deck-specific checks above, for a `deck-skeleton.html` artifact:

- **Verbatim framework intact** — the framework `<style>`, chrome, and trailing `<script>`
  are byte-for-byte from `deck-skeleton.html`; only the `SLOT:` markers were edited.
- **Scale-to-fit unbroken** — every slide is a `<section class="slide">` inside the
  1920×1080 `.deck-stage`; nothing overflows the fixed canvas at 16:9.
- **Theme bound at `:root`** — the chosen theme's palette + fonts are set as `:root` tokens,
  not hard-coded per slide; one accent, used sparingly (anti-AI-slop checklist still applies).
- **slot discipline** — title, `:root` tokens, per-deck `<style>`, and slide bodies are the
  only edits; counter + nav still render outside the scaled stage.
- **No native-mode confusion** — if the user wants slide-by-slide editing, this is the wrong
  track; switch to native deck mode.

<!-- test markers: scale-to-fit; slot; 16:9; verbatim framework -->
