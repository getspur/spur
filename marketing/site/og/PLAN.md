# SPUR OG-Image Set — PLAN

*2026-05-20. Plan only. No image generation in this artifact — generation is delegated to `growth-loop-media` or a Codex image-gen worker. Brand voice grounded in `marketing/product-marketing.md` V1.3 §Brand Voice (rigorous, pragmatic, terminal-native, developer-respectful, self-aware) and `marketing/messaging/positioning.md` (Hero A/B copy). Aesthetic constraints derived from the "Words to use / words to avoid" table in `positioning.md`.*

---

## Global constraints — apply to every image

These are non-negotiable. Any image that violates one of these fails review and is regenerated.

- **No photos of people.** No hands, no faces, no silhouettes.
- **No emoji.** Anywhere. Including the headline overlay.
- **No corporate-stock aesthetic.** No glassmorphism, no gradient-mesh blobs, no isometric "developer at laptop" illustration, no whimsical mascots, no 3D-rendered cubes/keys/locks.
- **No SaaS-y illustration** (Notion-style line art, Stripe-style abstract waveforms, Linear-style synthwave gradients).
- **Terminal-native palette only:**
  - Background: near-black `#0B0E14` to `#11141C` (one flat dark tone or one subtle vignette, no rainbow gradients).
  - Foreground primary: warm off-white `#E6E1CF` (the iTerm/Solarized-ivory feel, NOT pure `#FFFFFF`).
  - ANSI accents — use sparingly, one or two per image, never all six:
    - `#7FB4CA` (ANSI blue) — info / labels
    - `#76946A` (ANSI green) — approved / OK
    - `#C34043` (ANSI red) — rate-limited / rejected
    - `#DCA561` (ANSI yellow) — warning / cost
    - `#957FB8` (ANSI magenta) — brand accent for the wordmark only
- **Typography:**
  - Headline overlay in a monospaced typeface — JetBrains Mono, Berkeley Mono, or IBM Plex Mono. Tracking tight, no italics.
  - No serif. No sans-serif display fonts. No script.
  - Headline size scales so the longest line is ~28–36ch wide; no orphaned words.
- **Composition:**
  - Headline left-aligned, top-left or center-left. Safe zone: 80px inset from all edges.
  - The wordmark `spur` (lowercase, monospaced) appears bottom-left at ~24px size, optionally with `▎` block-cursor glyph in `#957FB8`.
  - Optional decorative motif from a constrained vocabulary: ASCII chart fragments (`▁▂▃▅▆▇`), a single TUI box-drawing border (`╭─╮│╰─╯`), a fake shell prompt line (`$ spur …`), or a stripped-down review-card frame. **Pick one motif per image, never two.**
- **No actual SPUR product screenshots in this set** — those go in `/screens/`. OG images are typographic + motif, not UI captures. (Rationale: the V1.3 PRD §Product Mockups note in the image skill explicitly warns against AI-generated UI; rather than gamble, OG cards skip UI entirely and let the headline do the work.)
- **Text rendering model:** Because gpt-image-1 has known weakness with long-form monospaced text, all headline + wordmark text is composited as a **post-process overlay** (HTML/CSS via Satori, or a single Figma export), NOT baked into the generated image. The generation prompt requests only the background + decorative motif and explicitly says "no text, no letters, no numbers." See "Fallback" in the summary.

---

## Image set

### 1. Homepage default OG — Hero A (cost ledger)

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/home.png` (also `home@2x.png`) |
| **Dimensions** | 1200×630 (1.91:1) |
| **Headline (overlay)** | `See what you'd be billed today,`<br>`across every agent, in one number.` |
| **Sub-overlay (small, bottom-right, optional)** | `$ spur cost --today` in `#7FB4CA` |
| **Motif** | A faint ASCII bar-chart fragment in the lower-right quadrant, rendered as if read from `spur cost --today`. Six bars, decreasing left-to-right, in `#DCA561` at 35% opacity over the dark background. |
| **Wordmark** | `▎spur` bottom-left, `#957FB8` cursor glyph + `#E6E1CF` wordmark. |

**Generation prompt (background + motif only, NO text):**

```
A near-black terminal-aesthetic background, flat color #0B0E14 with a very
subtle vertical vignette darkening toward the edges. In the lower-right
third, a sparse ASCII-style bar chart fragment composed of unicode block
glyphs (▁ ▂ ▃ ▅ ▆ ▇) at roughly 35% opacity in muted amber #DCA561,
six bars descending from left to right, each bar 1–2ch wide, anchored at
a faint horizontal baseline. No text, no letters, no numbers, no logos,
no people, no UI screenshots, no gradients other than the corner vignette.
Print-quality, 1200x630, 16:9-ish wide, flat 2D composition, no depth of
field, no rendered 3D objects.
```

---

### 2. /pricing OG

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/pricing.png` |
| **Dimensions** | 1200×630 |
| **Headline (overlay)** | `Community is free.`<br>`Pro is $19 / seat / mo —`<br>`below Claude Code Max on purpose.` |
| **Sub-overlay** | None. The headline carries it. |
| **Motif** | A four-row monospaced pricing rail rendered as a faint TUI table outline in `#7FB4CA` at 40% opacity — three columns (`tier`, `seat / mo`, `lifetime`) and four rows. The cells are empty; the headline overlay does the work. Box-drawing chars only: `╭┬╮ ├┼┤ ╰┴╯`. |
| **Wordmark** | `▎spur` bottom-left. |

**Generation prompt:**

```
A flat near-black background #0B0E14 with a faint TUI-style table outline
rendered in muted blue #7FB4CA at 40% opacity, occupying the right two-
thirds of the canvas. The table has three columns and four rows, drawn
with unicode box-drawing characters only (╭┬╮ ├┼┤ ╰┴╯ │ ─). All cells
empty. Subtle vignette in the corners. No text, no letters, no numbers,
no logos, no people, no UI screenshots, no gradients, no decorative
graphics other than the table outline. 1200x630, flat 2D, no depth of
field.
```

---

### 3. /quickstart OG

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/quickstart.png` |
| **Dimensions** | 1200×630 |
| **Headline (overlay)** | `Two commands.`<br>`Then close the laptop.` |
| **Sub-overlay** | A two-line fake shell session, monospaced, in the center-left: <br>`$ curl -sSL getspur.dev/install.sh | sh` <br>`$ spur init` <br>Prompt `$` in `#76946A`, commands in `#E6E1CF`. |
| **Motif** | A single block cursor `▎` blinking-implied (solid, not animated) at the end of the second line, in `#957FB8`. No other motif. |
| **Wordmark** | `▎spur` bottom-left. |

**Generation prompt:**

```
A flat near-black terminal background #0B0E14, completely empty composition
with only a very subtle vignette in the corners. No text, no letters, no
numbers, no logos, no people, no UI screenshots, no gradients, no chart
fragments, no decorative graphics. The image is intentionally minimal —
just the dark background with a faint vignette. 1200x630, flat 2D.
```

*(All visual content is the post-process text overlay. The generator only supplies the background. This is intentional — see Fallback note below.)*

---

### 4. /vs/claude-code OG

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/vs-claude-code.png` |
| **Dimensions** | 1200×630 |
| **Headline (overlay)** | `Claude Code is the best worker.`<br>`SPUR is the control tower above it.` |
| **Sub-overlay** | None. |
| **Motif** | An ASCII tree fragment in the right third, rendered in `#7FB4CA` at 50%: <br>`spur` <br>`├── claude-code (worker)` <br>`├── codex   (worker)` <br>`╰── gemini  (worker)` <br>Render this motif as part of the **overlay**, not the generated background — see Fallback. The generated layer is a flat background only. |
| **Wordmark** | `▎spur` bottom-left. |

**Generation prompt:**

```
A flat near-black background #0B0E14 with a very subtle vignette. No text,
no letters, no numbers, no logos, no people, no UI screenshots, no
gradients, no chart fragments. Completely empty dark composition. 1200x630,
flat 2D.
```

---

### 5. /vs/devin OG

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/vs-devin.png` |
| **Dimensions** | 1200×630 |
| **Headline (overlay)** | `Devin runs in Slack.`<br>`SPUR runs in your terminal —`<br>`with a human at the review gate.` |
| **Sub-overlay** | Optional faint label in lower-right, `#C34043` at 70%: `// not an autonomous-engineer alternative` |
| **Motif** | A single TUI box-drawing frame in `#7FB4CA` at 40% wrapping the review-gate metaphor — a small `╭─ review ─╮` … `╰──────────╯` frame in the lower-right quadrant, empty inside. The frame is the motif; the empty interior is the point. |
| **Wordmark** | `▎spur` bottom-left. |

**Generation prompt:**

```
A flat near-black background #0B0E14 with a faint TUI-style rectangular
frame in muted blue #7FB4CA at 40% opacity, drawn with unicode box-drawing
characters (╭ ─ ╮ │ ╰ ╯), positioned in the lower-right quadrant, roughly
220 by 100 pixels, empty inside. Subtle vignette in the corners. No text,
no letters, no numbers, no logos, no people, no UI screenshots, no
gradients, no chart fragments other than the frame. 1200x630, flat 2D.
```

---

### 6. /vs/cursor OG

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/vs-cursor.png` |
| **Dimensions** | 1200×630 |
| **Headline (overlay)** | `Cursor edits a file.`<br>`SPUR coordinates a fleet of agents`<br>`editing many files in parallel.` |
| **Sub-overlay** | None. |
| **Motif** | An ASCII DAG fragment in the right third — three sibling nodes converging on a single review node, drawn with `─ │ ╱ ╲ ◇ ○` glyphs in `#76946A` at 50%. The motif evokes parallel-dispatch + single-review-surface. |
| **Wordmark** | `▎spur` bottom-left. |

**Generation prompt:**

```
A flat near-black background #0B0E14. In the right third of the canvas, a
sparse ASCII-style DAG diagram in muted green #76946A at 50% opacity,
composed of unicode line and node glyphs (─ │ ╱ ╲ ◇ ○), showing three
parallel branches converging on a single node. Lines are 1–2 pixels thick.
Subtle vignette in the corners. No text, no letters, no numbers, no logos,
no people, no UI screenshots, no gradients other than the corner vignette.
1200x630, flat 2D, no depth of field, no 3D rendering.
```

---

### 7. Launch announcement OG — Hero B (control tower)

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/launch.png` |
| **Dimensions** | 1200×630 |
| **Headline (overlay)** | `The control tower for your CLI coding agents.`<br>`Now generally available.` |
| **Sub-overlay** | A faint date stamp lower-right, monospaced, `#7FB4CA` at 70%: `// GA · 2026` (replace year at publish time) |
| **Motif** | A four-pane TUI layout sketch covering the right half — four empty box-drawn rectangles arranged 2×2, each labeled internally with a faint placeholder glyph (`·`), suggesting "plan / workers / review / cost." Drawn in `#7FB4CA` at 35%. This is the densest motif in the set; it's allowed because launch is the moment we earn the "control tower" claim. |
| **Wordmark** | `▎spur` bottom-left. |

**Generation prompt:**

```
A flat near-black background #0B0E14. The right half of the canvas contains
a 2x2 grid of four empty TUI-style rectangular frames, each drawn with
unicode box-drawing characters (╭ ─ ╮ │ ╰ ╯) in muted blue #7FB4CA at 35%
opacity. Each rectangle is roughly equal in size, with a small gap between
them. Each rectangle contains a single faint dot (·) centered inside.
Subtle vignette in the corners. No text, no letters, no numbers, no logos,
no people, no UI screenshots, no gradients other than the corner vignette,
no decorative graphics other than the four frames. 1200x630, flat 2D, no
depth of field.
```

---

### 8. HN / X share card — square

| Field | Value |
|---|---|
| **Path** | `marketing/site/og/share-square.png` |
| **Dimensions** | 1200×1200 (1:1) |
| **Headline (overlay)** | `One brain.`<br>`Many workers.`<br>`Zero lost context.` |
| **Sub-overlay** | `▎spur` wordmark larger here (bottom-center, ~40px). |
| **Motif** | A single vertical ASCII chart bar in `#DCA561` running floor-to-near-ceiling on the right edge, decorative, at 35%. The square format lets us emphasize verticality (a single bar reads "ledger / measure" without being literal). |
| **Wordmark** | Already covered in sub-overlay. |
| **Use case** | The X Card meta (`twitter:card = summary_large_image` still renders 1:1 in some clients), Telegram link previews (which favor square), Discord embeds, Slack unfurls. **Not** for OG protocol consumers expecting 1.91:1 — those get image #1. |

**Generation prompt:**

```
A flat near-black square background #0B0E14, 1200x1200 pixels. On the right
edge, a single vertical unicode block-glyph bar (▇) extending from near the
bottom to near the top, roughly 80 pixels wide, in muted amber #DCA561 at
35% opacity. Subtle vignette in the four corners. No text, no letters, no
numbers, no logos, no people, no UI screenshots, no gradients other than
the corner vignette, no other decorative graphics. 1200x1200 square, flat
2D, no depth of field.
```

---

## Delegation handoff

When delegating image generation to `growth-loop-media` or a Codex image-gen worker:

1. Pass the **generation prompt** verbatim (background + motif only).
2. Specify model: `gpt-image-1` first, with Ideogram 3.0 as the documented fallback (Ideogram handles text better — relevant only if we ever choose to bake text into a generation, which this plan deliberately avoids).
3. Output: lossless PNG at the listed dimensions. Convert to WebP + JPEG fallback at publish time per `image` skill §Optimization.
4. Composite the **headline overlay + wordmark + motif (when motif is overlay-only)** in a second pass — Satori (HTML/CSS → PNG) is the path of least resistance and gives crisp monospaced text. Vercel OG can do this at the edge if we want per-request OG generation later, but at launch we ship eight static files.
5. Optimize per `marketing/marketingskills/skills/image/SKILL.md` §Optimization Checklist before commit to the public site.

---

## Summary — caller's three questions

### (a) Highest-CTR-upside OG image

**Image #1 — homepage default (Hero A cost ledger).** Three converging reasons: (i) it's the OG that appears on every shared link to `getspur.dev` that doesn't have a more specific override, so its impression volume dwarfs every other card in this set by 5–20×; (ii) the "$1k week" framing — though softened to "billed today" in the headline — taps the sharpest emotional language identified in `positioning.md` §(a) and `marketing/research/themes.md` synthesis #2 (cost opacity); (iii) it's the only card that promises a *quantitative reveal* (a number you don't know yet), which historically outperforms qualitative claims in scroll-stopping CTR on X and LinkedIn link cards. Second priority is image #7 (launch) — it gets one big moment of distribution and needs to carry weight. Third priority is image #4 (`/vs/claude-code`) because Claude Code Max users are the warmest cohort and any share of `/vs/claude-code` is by definition a high-intent forward.

### (b) Share-card aspect — square or 1.91:1?

**Both — ship square (image #8) as a distinct asset, keep 1.91:1 (image #1) as the OG default.** The 1.91:1 image is mandatory for the Open Graph protocol consumers (Facebook, LinkedIn, Slack, most Discord embeds in standard mode), and `twitter:card = summary_large_image` renders the same 1.91:1 well on X. Square earns its keep in three places: (i) Telegram link previews crop aggressively and 1:1 holds up better than 1.91:1 letterboxed; (ii) some Discord clients in compact mode render 1:1 cleaner; (iii) X's mobile timeline occasionally renders shared images larger when the source aspect is 1:1 vs 1.91:1, though this varies. Because we're a SPUR audience targeted heavily at Telegram (review-on-the-go is a core JTBD per `product-marketing.md:43-44`) and X (channel #1 per `product-marketing.md:217`), a dedicated square asset is worth the marginal cost. Do **not** swap the default OG to square — most OG consumers expect 1.91:1 and will letterbox or center-crop a square card in ways we can't control.

### (c) Fallback if gpt-image-1 produces low-quality terminal aesthetics

The plan already partially fallback-hardens by compositing all headline text + most motifs as **post-process overlays**, not as generated content — which is the single highest-yield mitigation (gpt-image-1's text rendering is the weakest link). If the *backgrounds + motifs* themselves come back wrong (gradient artifacts, accidental letterforms, "AI sheen," fake CRT-scanline noise we didn't ask for), the escalation ladder is:

1. **Regenerate with a stricter prompt** — drop the motif from the generation prompt entirely and ask only for `flat color #0B0E14 with corner vignette, no other content`. Then composite the motif via Satori/SVG. This is fully deterministic and removes the generator from the critical path for everything except the dark backdrop.
2. **Skip generation entirely — render the whole image with Satori (HTML/CSS → PNG) or `@vercel/og`.** Every motif in this plan is achievable in pure CSS using monospaced unicode glyphs. Backgrounds are flat hex. Vignettes are radial gradients. Total cost: 100 LOC of TSX, zero per-image API spend, perfect determinism, and we can render OG images per-page programmatically later (`/blog/[slug]`, `/changelog/[id]`) reusing the same template. This is the **recommended default** if even one image in the set comes back questionable — the loss-of-novelty from "AI-generated background" is small, and the gain in brand consistency is large.
3. **Last resort — Figma template + manual export.** Build one master frame, swap headline text per image, export eight PNGs. Ships in an afternoon, zero AI dependency, but loses the per-page generation option (#2 has).

Recommendation: try gpt-image-1 once per image with the prompts above; if more than two of the eight come back off-brand, **switch the whole set to path #2 (Satori)** rather than re-rolling. The aesthetic is deliberately so constrained — flat dark + ANSI accent + box-drawing glyphs — that Satori does it better than a diffusion model in most cases anyway.
