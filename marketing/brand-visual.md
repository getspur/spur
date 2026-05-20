# Brand Visual Identity — SPUR

*Last updated: 2026-05-20 — initial draft, derived from `product-marketing.md` positioning. Confirm with design before high-volume use.*

Single source of truth for visual decisions across growth-loop images, blog hero art, OG images, social graphics, and pitch decks. **Read this file before any image generation.**

## North-star aesthetic

**"Control tower for a fleet of CLI coding agents."**

SPUR is a *distributed-systems kernel disguised as a TUI*. The visual identity must say:
- Terminal-native, not browser-native.
- Engineered, not magical.
- Power-user tool, not consumer app.
- Calm and precise, not loud and hype.

Imagine the cover of an O'Reilly book about distributed systems, not the cover of a B2B SaaS pitch deck.

## Core motifs (use these)

1. **Control tower / orchestrator** — the canonical category phrase from Beefin's verbatim quote. Schematic tower silhouettes, radar sweeps over a fleet, dispatch boards.
2. **Worktree fleet** — multiple parallel lanes, each a worker. Visual: parallel tracks, DAG nodes, branch-line diagrams.
3. **Event lineage** — timelines, NDJSON streams, replay scrubs. Visual: horizontal event streams with timestamped beads.
4. **Cost ledger** — small numeric readouts, ticker-style. Live-feel.
5. **Terminal frame** — monospace text, ANSI box-drawing characters (`┌─┐ │ └─┘`), cursor blink. The TUI is the product.

## Forbidden (do not generate)

- **Glowing brains.** AI cliché. Hard ban.
- **Photoreal humans** with laptops in coffee shops. Stock-photo energy.
- **Generic "futuristic" backgrounds** — purple nebulas, neon grids, flying particles.
- **Hand-shaking robots, robot heads, humanoid AI figures.**
- **Holographic UIs floating in air.**
- **Abstract "data flow" with rainbow swooshes** — say what the data is.
- **Skyscraper photos** (even if "control tower" is the metaphor — keep it schematic).
- **Logos of competitors** unless explicitly for a comparison piece (and even then, with proper marks).

## Color palette

Anchor on a terminal-dark base with one warm signal color (the "live ledger" feel).

| Role | Hex | Notes |
|---|---|---|
| Background — primary | `#0E1116` | Near-black, slight blue cast. Terminal-dark. |
| Background — secondary | `#1A1F26` | Card / panel surface. |
| Foreground — primary text | `#E6EDF3` | Off-white, never pure white. |
| Foreground — muted | `#8B949E` | Captions, timestamps. |
| Accent — primary (signal) | `#FFB454` | Amber. Use sparingly: cost ledger numbers, status pips, single hero glyph. |
| Accent — secondary (worker) | `#7EE787` | Terminal green. Worker/success states. |
| Accent — tertiary (review) | `#79C0FF` | Soft blue. Review/approve actions. |
| Accent — warning | `#FF7B72` | Soft red. Rate-limit, blocked, error. |

Use **one** accent per image unless you're explicitly showing a state legend. Heavy use of accents = loud = wrong.

## Typography (when text is in-image)

- **Headlines:** geometric sans-serif. JetBrains Mono Bold / Berkeley Mono Bold for headlines is fine when the piece is terminal-themed; otherwise Inter Bold.
- **Body / labels:** JetBrains Mono Regular. Always monospace when labeling code, diagrams, or terminal frames.
- **Never:** script fonts, slab serifs, all-caps display fonts.

## Composition rules

1. **Empty space is a feature.** Crowded images read as panicky; SPUR's promise is calm. Aim for ≥40% negative space.
2. **One subject per image.** A tower OR a fleet OR a lineage timeline — not all three.
3. **Off-center subject** following the rule of thirds, with breathing room for headline overlay on hero images.
4. **Axis-aligned, not skewed.** Schematic-clean beats artsy-tilted.
5. **Flat or near-flat shading.** Subtle gradients are okay; heavy 3D rendering is not.

## Style descriptor (paste into gpt-image-1 prompts)

For consistency across the daily growth-loop, **append this to every image prompt** unless overriding intentionally:

> Flat technical illustration in the style of a distributed-systems diagram. Terminal-dark background (#0E1116). One amber (#FFB454) signal accent. JetBrains Mono labels where applicable. ≥40% negative space. Off-center composition. No glowing brains, no humanoid robots, no photoreal humans, no generic futuristic backgrounds, no rainbow data flow swooshes. Calm, precise, engineered — like an O'Reilly book cover about distributed systems.

## Per-channel sizing

| Asset | Dimensions | Notes |
|---|---|---|
| X single post image | 1600×900 (16:9) | Use gpt-image-1 `1536x1024`, upscale if needed. |
| X thread cover | 1600×900 (16:9) | Same as above. Hero image only on the first tweet. |
| Reddit text post hero | 1200×630 | OG-style. Use gpt-image-1 `1536x1024` and crop. |
| OG / link preview | 1200×630 | Same as above. |
| GitHub social card | 1280×640 | Same dimensions logic. |

## Voice-in-image rules

When text appears inside an image:
- **Headline:** ≤8 words. Use phrasing from `product-marketing.md` "Language to use" — e.g. "Issue in, PR out", "One review surface", "Don't lose context", "Brain-swap mid-flow".
- **Subhead (optional):** ≤14 words.
- **Never put your own URL in the image** — it goes in the post copy, not the asset.

## Three reference compositions

If gpt-image-1 needs a starting point, use one of these:

1. **Control tower over a fleet.** Schematic tower glyph centered-left, 5 parallel worker lanes flowing right, each lane a worktree with a tiny status pip (amber/green/blue). For: hero shots about parallelism.
2. **Event lineage scrub.** Horizontal timeline, NDJSON-style event beads, one highlighted "resume here" marker. For: durability / session-resume content.
3. **Cost ledger card.** Single dark card, monospace cost readout with five vendor rows + a total. Amber on the total. For: cost-visibility / rate-limit content.

## When in doubt

Generate two variants and pick the **calmer** one.
