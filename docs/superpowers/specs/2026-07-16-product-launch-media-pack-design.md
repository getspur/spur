# Product Launch Media Pack Design

**Status:** Approved for implementation on 2026-07-16

**Surface:** Repository-native HTML media pack

**Audience:** Product Hunt reviewers, launch operators, technical evaluators, and social collaborators

**Primary source of truth:** Real SPUR TUI captures from `scripts/e2e/demos/tui-live/`

## 1. Objective

Produce a launch-ready SPUR media pack whose labels, screenshots, captions, and video claims are visibly supported by real product evidence. The pack must make three facts clear within the first viewport:

1. SPUR is a control tower for CLI coding agents.
2. Session Detail is the operator's home.
3. Plans, workers, and review state remain observable and recoverable.

The deliverable is `docs/product_launch/media_pack/html/index.html`, backed by adjacent media files in the same portable folder. It replaces the current inventory-style visualizer with a designed launch handoff and review surface.

## 2. First-principles constraints

### 2.1 Truth precedes polish

An asset is publishable only when the pixels prove the caption. A real capture with the wrong label is not product truth. Generated marketing media may attract attention, but it must never substitute for product evidence.

### 2.2 One asset, one job

- Product Hunt gallery assets prove product behavior.
- The hero video tells the plan and worker control story.
- The thumbnail identifies SPUR at 240 by 240 pixels.
- Social assets provide style and distribution hooks.
- The HTML artifact reviews, explains, and hands off the complete pack.

### 2.3 Fail closed

The build must not overwrite `ph_ready/` when a source file is missing, a proof timestamp is outside the source duration, required output dimensions are wrong, or an approved source checksum has changed. Stale output is safer than silently publishing mislabeled output.

### 2.4 Real UI only

No generated terminal chrome is allowed in Product Hunt screenshots or product-demo footage. Designed framing, captions, title cards, and crops may surround a real capture, but must not alter the product UI or invent a state.

## 3. Root cause and corrective model

The current pipeline has four independent sources of drift:

1. `story-contract.test.sh` validates source strings, not rendered screen state.
2. `refresh.sh` selects the largest PNG candidate by byte size, not the frame that proves the intended capability.
3. The hero render applies captions for specialists and resume to a single plan-loop source that does not contain those journeys.
4. The HTML page repeats asset facts in handwritten JavaScript arrays, so filenames, captions, and channel guidance can diverge from the build inputs.

The repair introduces one explicit proof manifest and makes every downstream artifact derive from it.

## 4. Information architecture

### 4.1 First viewport: launch thesis

The page opens with:

- SPUR wordmark and the line `Control tower for CLI coding agents.`
- A short proof statement: `Real sessions. Real workers. One review surface.`
- The approved hero video with a static poster frame.
- Three provenance facts: real TUI capture, capture date, and source journey.
- A compact link row for Product Hunt upload files, source captures, and rebuild instructions.

The first viewport must not lead with the Sessions picker, Dashboard lineage, generic architecture copy, or a grid of equally weighted cards.

### 4.2 Narrative gallery

Five large proof chapters replace the current seven-card inventory:

1. **Session Detail:** composer and ReAct transcript establish the operator home.
2. **Workers and plan loop:** DELEGATE or worker evidence is visible in the same session context.
3. **Plan progress:** campaign state is shown as a decision surface.
4. **Specialist routing:** Explore adoption and the `agent=`, `model=`, `effort=` cascade are visibly supported.
5. **Resume:** a saved session is reattached with history visible.

Backlog and lineage assets remain available in the technical appendix. They are not part of the core Product Hunt sequence unless their captures pass the same proof review.

Each chapter contains one dominant image, a problem sentence, a concise proof caption, source journey, timestamp, dimensions, and channel eligibility.

### 4.3 Launch handoff

The page includes a static upload matrix with:

- Product Hunt field
- Approved filename
- dimensions and duration
- file size
- source journey
- proof status
- direct relative link

The matrix is visible without JavaScript.

### 4.4 Marketing separation

Social and generated marketing assets live in a separate section labeled `Marketing treatment, not product proof.` They may reference real captures, but the page must make their channel restrictions impossible to miss.

### 4.5 Provenance and rebuild appendix

The appendix exposes the proof manifest, checksums, source tapes, capture commands, and validation commands. Native HTML `details` elements may collapse the appendix while remaining usable with scripts disabled.

## 5. Visual direction

### 5.1 Direction

Use a SPUR-native editorial utility direction: dark technical canvas, warm off-white text, cyan as the only primary accent, violet reserved for metadata, and real terminal imagery as the decisive visual flourish.

Use the existing SPUR palette as fixed tokens:

| Token | Value |
|---|---|
| Background | `#0B0E14` |
| Surface | `#11141C` |
| Text | `#E6E1CF` |
| Muted | `#8B8680` |
| Accent | `#7FB4CA` |
| Violet metadata | `#957FB8` |
| Border | `#2A2E38` |

### 5.2 Open Design dials

- `DESIGN_VARIANCE = 6`: asymmetric editorial layout with large proof frames and deliberate empty space.
- `MOTION_INTENSITY = 2`: hover and focus feedback only; no scroll choreography.
- `VISUAL_DENSITY = 6`: launch metadata stays compact, while core proof frames remain large and readable.

### 5.3 Typography and restraint

- Use local system fonts only. No remote font request is allowed.
- Use a system sans stack for display and body, with a monospace stack for paths, hashes, and capture metadata.
- Use square or lightly rounded geometry. Avoid pill-heavy chrome and card soup.
- Do not use gradients, generic feature icons, invented metrics, or decorative terminal mockups.
- Artifact-visible copy must contain no em dash or separator-style en dash.

## 6. Proof manifest

Create `docs/product_launch/media_pack/proof-manifest.json` as the machine-readable source for publishing decisions.

Each publishable asset records:

```json
{
  "id": "session-detail-home",
  "kind": "still",
  "source": "live_demos/13-problem-plan-loop-drive.mp4",
  "journey": "problem-plan-loop-drive",
  "timestamp_sec": 0,
  "expected_proof": ["Session Detail", "INSERT"],
  "caption": "Session Detail keeps the plan and worker loop in one place.",
  "channel": ["product-hunt-gallery", "html"],
  "approved_source_sha256": "",
  "status": "candidate"
}
```

Implementation replaces the example timestamp, checksum, and status with visually reviewed values. Only entries with `status: "approved"` may be copied into `ph_ready/` or listed as Product Hunt uploads.

The manifest also records hero segments. A segment caption may describe only the named source clip. Specialist and resume captions require specialist and resume source clips, respectively.

## 7. Capture and publishing flow

```text
TUI journey and tape
  -> rendered source capture
  -> proof manifest candidate timestamp or segment
  -> visual review plus source checksum approval
  -> staged derivative generation
  -> automated media contract
  -> atomic publish to ph_ready
  -> static HTML handoff
```

### 7.1 Capture requirements

- Re-render the five value journeys at 2560 by 1600 with story pacing.
- Required Session Detail assertions must not accept a failed initialization state as proof.
- The capture must hold each proof screen long enough to extract a readable frame.
- The live plan-loop seed may be used only when it is high-resolution and its token-spend gate has been intentionally enabled.

### 7.2 Semantic still selection

Remove byte-size ranking from `refresh.sh`. The script extracts the exact approved timestamp from the proof manifest. Changing a timestamp changes the manifest and requires a new visual approval checksum.

### 7.3 Hero video

The Product Hunt hero is 16:9, 1080p, and no longer than 60 seconds. It uses:

1. A two to three second thesis card.
2. A real plan-loop sequence whose captions match visible Session Detail, worker, and plan evidence.
3. Optional specialist or resume cuts only when sourced from their own approved journeys.
4. A two to three second install card.

The video must have a readable first frame, burned-in captions, no invented UI, and no caption that claims evidence absent from the current frame.

### 7.4 Gallery derivatives

Product Hunt gallery output remains 1270 by 760. Each frame uses a real capture and may include a restrained caption rail outside the product viewport. The crop must preserve readable text and the relevant proof region.

### 7.5 Thumbnail

The thumbnail uses a simple SPUR mark or bold SPUR wordmark on the approved palette. Terminal text is not relied on at 240 by 240 pixels. If a real TUI crop is retained, it is subordinate texture rather than the identifying content.

## 8. HTML implementation

`html/index.html` is hand-authored as a static baseline with inline CSS. Optional inline JavaScript may enhance filtering, video playback, or copy buttons, but it must not create the gallery, upload matrix, or core narrative.

The artifact has no remote scripts, stylesheets, fonts, images, or analytics. Relative references to adjacent pack media are permitted because the deliverable is a portable media-pack directory, not a single embedded notebook output.

The page remains meaningful when JavaScript is disabled:

- all headings and copy remain visible;
- every approved image remains visible;
- every video has a poster and direct file link;
- the upload matrix remains complete;
- provenance and rebuild guidance remain accessible.

## 9. Error handling

- Missing source media: fail before derivative generation.
- Timestamp outside duration: fail with asset ID, timestamp, and source duration.
- Unapproved manifest entry: omit from `ph_ready/` and the upload matrix.
- Source checksum drift: fail and request a new visual review.
- Wrong output dimensions or codec: fail before publishing.
- Missing HTML-relative asset: fail the media contract.
- Rebuild failure: preserve the last approved `ph_ready/` directory.

## 10. Testing and verification

### 10.1 Automated contracts

Add a media-pack contract test that verifies:

1. Every approved manifest source exists.
2. Every timestamp is within the source duration.
3. Every approved source checksum matches.
4. Every Product Hunt gallery image is exactly 1270 by 760.
5. The thumbnail is square, with a 240 by 240 derivative.
6. The hero video is H.264, 1920 by 1080, and at most 60 seconds.
7. Every relative media path in the HTML exists.
8. The HTML contains no remote resource URL.
9. The HTML contains no empty JavaScript-only gallery or film container.
10. Artifact-visible source contains no em dash, en dash, or their HTML entities.

Run the existing static journey contract as a separate check. It remains a source-contract test and is not treated as visual proof.

### 10.2 Visual review

Generate contact sheets for the hero timeline, core gallery, thumbnail at native size, and marketing assets. Review them against the Open Design five-dimensional critique:

- philosophy
- hierarchy
- execution
- specificity
- restraint

Any score below 3 out of 5 blocks publishing. Run the anti-slop checklist after the first visual review and revise once before final approval.

### 10.3 Required commands

Verification will include:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
docs/product_launch/media_pack/demo_render/build.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

The implementation plan may add a dedicated contact-sheet command, but it must not replace inspection of the rendered output.

## 11. File boundaries

| Path | Responsibility |
|---|---|
| `docs/product_launch/media_pack/proof-manifest.json` | Approved source, timestamp, caption, checksum, and channel mapping |
| `docs/product_launch/media_pack/refresh.sh` | Stage and publish semantic derivatives from the proof manifest |
| `docs/product_launch/media_pack/tests/media-contract.test.sh` | Media, HTML, and provenance acceptance checks |
| `docs/product_launch/media_pack/demo_render/` | Hero composition from approved segments |
| `docs/product_launch/media_pack/html/index.html` | Static Open Design media-pack artifact |
| `docs/product_launch/media_pack/MANIFEST.md` | Human handoff and rebuild instructions |
| `scripts/e2e/demos/tui-live/tapes/09-13*.tape` | Capture source contracts and proof dwell |

## 12. Acceptance criteria

The media pack is complete when:

1. No Product Hunt asset label contradicts its pixels.
2. The first gallery image visibly shows Session Detail, not the Sessions picker.
3. The workers frame visibly contains worker or DELEGATE evidence.
4. The hero captions match the visible source clip at every timestamp.
5. The thumbnail identifies SPUR at 240 by 240 pixels.
6. All Product Hunt derivatives meet current documented dimensions and format constraints.
7. The HTML tells the complete story with JavaScript disabled and makes product proof distinct from marketing treatment.
8. The media contract passes before and after a clean rebuild.
9. Contact-sheet critique passes all five Open Design dimensions and the anti-slop checklist.
10. The final repository commit contains only the intended media-pack, capture-contract, test, and documentation changes.
