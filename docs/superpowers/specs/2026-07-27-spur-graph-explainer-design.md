# spur-graph Four-Beat Explainer Design

**Date:** 2026-07-27
**Status:** Gate 1 approved
**Source revision:** `1dac840f2a5ebc81ad0862d4fe7dafcbf9c043e7`
**Companion notebook:** `/Users/kevintruong/.spur/scratch/Untitled28.ipynb`

## Goal

Produce a 48-second, 16:9 explainer that helps a new SPUR contributor
understand why `crates/spur-graph/` exists and name its four-part system model:
problem, parse, stabilize, query.

The viewer should leave with one durable takeaway:

> Files become facts. Facts become trustworthy answers.

## Audience and purpose

- Primary audience: engineers and contributors working in the SPUR repository.
- Purpose: contributor orientation, not product marketing.
- Vocabulary: exact implementation terms are allowed, including `GraphFacts`,
  stable IDs, incremental rebuild, BLAKE3 content hash, and `code_*` tools.
- Success condition: a new contributor can name the pipeline and identify the
  load-bearing symbols to read first.

## Approved delivery contract

- Runtime: exactly 48 seconds.
- Canvas: 1920 by 1080 at 30 fps.
- Frame count: 1440 frames.
- Primary aspect ratio: 16:9 landscape.
- Delivery style: silent and text-led with open captions.
- Audio: no narration, music, or sound effects.
- Export audio track: silent AAC for a broadly compatible H.264/AAC MP4.
- Run shape: HyperFrames Companion mode with notebook review checkpoints.
- Final editor: PalmierPro.
- Final package:
  - H.264/AAC MP4
  - editable PalmierPro project
  - interactive notebook
  - delivery manifest and validation report
- Paid media generation: not required by the approved design.

## Truth and rights

All claims come from owned repository code and documentation at the recorded
source revision. The explainer uses authored diagrams and typography rather
than third-party footage or generated product imagery.

| Claim | Source | Load-bearing symbol |
|---|---|---|
| A worktree is not itself a queryable typed fact graph | `crates/spur-graph/ARCHITECTURE.md` | architecture overview |
| The extraction layer emits uniform facts across the supported languages | `crates/spur-graph/src/extract/tree_sitter.rs` | `build_facts` |
| The current language enum covers 15 languages | `crates/spur-graph/src/extract/languages.rs` | `Language` |
| Symbol identity is deterministic | `crates/spur-graph/src/identity.rs` | `stable_symbol_id_for` |
| Graph freshness includes a BLAKE3 content hash over sorted path and OID pairs | `crates/spur-graph/src/content_hash.rs` | `compute_graph_content_hash` |
| Incremental rebuild re-extracts changed paths and reuses unchanged buckets | `crates/spur-graph/src/store/build.rs` | `artifact_from_facts_incremental` |
| The graph artifact stores files, symbols, edges, history, and tombstones | `crates/spur-graph/src/schema.rs` | `GraphIndexArtifact` |
| MCP exposes `code_*` queries with freshness metadata | `crates/spur-graph/src/mcp/mod.rs` | `GraphMcpModule::dispatch`, `GraphResponseMetadata` |

The delivery manifest must record the source revision, file paths, symbol
names, claim text, and owner-repository rights for every scene.

## Approved visual direction

The selected direction is **A, Graph Utility**.

### Palette

- Paper: `#F8FAF8`
- Ink: `#17201D`
- Graph green: `#38A969`
- Pale green: `#DFF4E6`
- Grid line: `#DFE9E2`
- Muted text: `#607068`
- Problem accent, used once in Beat 1: `#B84231`

The background is a light engineering grid. Green has semantic meaning: a path,
identity rail, or verified answer. Red appears only on the unresolved freshness
question in the problem beat.

### Typography

- Headline role: heavy grotesk, uppercase, compact tracking.
- Code and label role: monospace.
- Caption role: monospace, medium-to-semibold weight.
- Production pairing: Inter Black for headlines and IBM Plex Mono for code,
  labels, and captions.
- Captions use no more than two lines and one concise sentence at a time.

### Composition

The video behaves as one continuous left-to-right system pipeline. Each beat
inherits one visual element from the prior beat so the viewer never has to
rebuild the mental model.

- Beat 1 passes `tree_sitter.rs` into Beat 2.
- Beat 2 passes the outgoing `GraphFacts` path into Beat 3.
- Beat 3 passes the green identity rail into Beat 4.
- Beat 4 resolves the rail into a trustworthy answer card.

The focal object occupies roughly 40 to 60 percent of the active visual area.
Source paths and implementation names remain legible at embedded playback size.

## Frame ownership

The output frame has two explicit ownership zones:

- HyperFrames motion plate: top 896 pixels.
- Palmier open-caption band: bottom 184 pixels.

HTML Video owns the rendered HyperFrames plate. It reserves the lower 184
pixels and does not render captions there. Palmier owns the caption band,
caption timing, final compositing, silent audio track, and export.

This boundary keeps captions crisp and editable while preserving deterministic
seek-safe motion in the plate.

## Four-beat story

### Beat 1: Problem, 00:00 to 00:10

**Claim:** A worktree holds code, but it does not answer graph questions.

**Picture:** Five repository file tiles scatter from a shared repository point.
Three question chips appear after the scatter settles:

- `WHERE IS X?`
- `WHO CALLS IT?`
- `IS IT FRESH?`

**Primary on-screen text:** `A WORKTREE IS NOT A QUERYABLE GRAPH`

**Concept caption:** `A worktree holds code. It does not answer graph questions.`

**Motion rule:** `center-outward-expansion`

- Lead-in: 0.30 seconds.
- Expansion duration: 1.35 seconds.
- Ease: `power3.out`.
- Stagger: 0.06 seconds across five file tiles.
- Question chips appear after the expansion completes.
- No idle float or ambient motion after landing.

**Transition:** `tree_sitter.rs` remains framed while the other tiles clear. It
becomes the left source node of Beat 2.

### Beat 2: Parse, 00:10 to 00:22

**Claim:** Tree-sitter extraction turns supported source languages into a
uniform `GraphFacts` layer.

**Picture:** A supported-language cluster flows through `build_facts` and lands
in a `GraphFacts` card. The cluster names Rust, TypeScript, Python, Go, Java, C,
C++, and `+8`, communicating the current 15-language enum without crowding the
frame.

**Primary on-screen text:** `TREE-SITTER NORMALIZES THE WORKTREE`

**Concept caption:** `Tree-sitter extracts a uniform fact layer across 15 languages.`

**Motion rule:** `svg-path-draw`

- Inline SVG paths use measured `getTotalLength()` values.
- Each segment draws in 0.55 seconds with `power2.out`.
- A new segment starts at 75 percent of the prior segment duration.
- Paths use `fill: none` during the draw.
- The `GraphFacts` card appears only after the final connector settles.

**Transition:** The outgoing fact path continues across the beat boundary and
becomes the green identity rail in Beat 3.

### Beat 3: Stabilize, 00:22 to 00:36

**Claim:** Deterministic IDs, incremental rebuild, and the content hash preserve
identity and freshness through change.

**Picture:** `stable_symbol_id_for` pins an identity card on the left. A changed
path enters the re-extract lane while an unchanged path enters the reuse lane.
A BLAKE3 hash rail resolves beneath both lanes.

**Primary on-screen text:** `STABLE IDS KEEP THE GRAPH TRUSTWORTHY`

**Concept caption:** `Stable IDs and incremental rebuild preserve identity and freshness.`

**Motion rule:** `nudge-curve`

The facts group moves left by 288 pixels:

1. Ramp-in: 32 pixels over 0.14 seconds with `power3.in`.
2. Linear burst: 225 additional pixels over 0.12 seconds with `none`.
3. Tail: 31 additional pixels over 0.44 seconds with `power4.out`.

The changed and reused lanes reveal during the linear burst. The tail is more
than three times the ramp-in duration, preventing a hard stop.

**Transition:** The BLAKE3 rail grows into the input port of the Beat 4 query
surface.

### Beat 4: Query, 00:36 to 00:48

**Claim:** The graph artifact and MCP query surface turn facts into answers with
freshness metadata attached.

**Picture:** Four query states resolve into one answer card:

1. `code_resolve`
2. `code_read_symbol`
3. `code_callers`
4. `code_callees`

The answer card names files, symbols, edges, history, tombstones, and freshness
metadata.

**Primary on-screen text:** `ASK THE GRAPH, THEN VERIFY FRESHNESS`

**Concept caption:** `Files become facts. Facts become trustworthy answers.`

**Motion rule:** `dynamic-content-sequencing`

- Query state 1: 36.0 to 38.0 seconds.
- Query state 2: 38.0 to 40.0 seconds.
- Query state 3: 40.0 to 42.0 seconds.
- Query state 4: 42.0 to 44.0 seconds.
- Answer convergence: 44.0 to 45.5 seconds.
- Final system-model hold: 45.5 to 48.0 seconds.
- Timing is precomputed once.
- DOM content swaps occur only at entry transitions.
- The final 2.5 seconds contain no ambient motion.

The closing frame retains the four-part mnemonic:

`PROBLEM  ->  PARSE  ->  STABILIZE  ->  QUERY`

## HyperFrames production contract

The motion plate is one deterministic HyperFrames composition.

- `data-duration="48"`
- One paused GSAP timeline.
- Absolute timeline positions for every beat and transition.
- Seek-safe rendering at arbitrary timestamps.
- No `Math.random()`, `Date.now()`, infinite repeats, CSS keyframe production
  motion, page-load animation, or per-frame text replacement.
- Spatial motion uses transforms.
- SVG draw paths use measured geometry.
- The static DOM represents a meaningful frame before animation setup.
- Every animated target is scoped inside the composition root.
- Scene transitions preserve velocity and the shared left-to-right rail.

HTML Video renders the plate to an exact 48-second H.264 MP4 at 1920 by 1080
and 30 fps. The plate contains no captions and no audio.

## Palmier assembly contract

Palmier is the sole final editor.

- Create a new 48-second timeline and preserve unrelated timelines.
- Place the verified HyperFrames plate on the primary video track.
- Author open captions in the lower 184-pixel band.
- Use one or two caption lines, centered, with stable line breaks.
- Add a silent AAC audio stream.
- Do not add narration, music, sound effects, or decorative overlays.
- Export H.264/AAC at 1920 by 1080, 30 fps, and 48 seconds.

## Caption principles

Because the video is intentionally silent, captions are explanation cues rather
than a verbatim transcript.

- Each caption must state one complete idea.
- Captions must not duplicate a large headline word for word.
- Technical names may appear when they help contributor orientation.
- Reading speed and line wrapping are validated at 1920 by 1080 and at an
  embedded 960-pixel-wide preview.
- Caption timing belongs to Gate 2 approval.

## Notebook review flow

The companion notebook is the visual review surface.

1. Gate 1 brief and direction board.
2. Gate 1 four-beat concept board.
3. Gate 2 exact script, storyboard, source selects, and ownership map.
4. Render contact sheet and validation report.
5. Final Palmier assembly review.

Every notebook visual output is self-contained `text/html`, remains meaningful
with scripts disabled, and uses no external resource URLs.

## Validation

Automated validation must confirm:

- duration is 48.000 seconds within one frame;
- frame rate is 30 fps;
- frame size is 1920 by 1080;
- video codec is H.264;
- one AAC audio stream exists and is silent;
- no caption is baked into the HyperFrames plate;
- final captions occupy only the Palmier-owned lower band;
- the final 2.5 seconds are visually stable;
- every manifest claim resolves to the recorded source revision and path;
- all four approved motion rules are represented in the composition;
- arbitrary-time screenshots match fresh seeks at the same timestamps.

Manual review must inspect:

- embedded-size text legibility;
- caption contrast and line breaks;
- factual accuracy of paths, symbols, and the 15-language claim;
- continuity of the green rail between beats;
- motion restraint and absence of decorative idle movement;
- final mnemonic and takeaway hold.

## Non-goals

- Narration, music, or sound effects
- Paid media generation
- Fictional SPUR interfaces
- Photoreal footage or generic AI imagery
- Vertical or square variants
- A broad tour of every `spur-graph` module
- Editing `crates/spur-graph/` source code
- Publishing or deploying the final video before the final review gate
