# Product Hunt dual-duration kinetic hero design

**Status:** approved direction, awaiting implementation plan

**Decision:** A — Kinetic operator cut, delivered at 45 and 90 seconds

**Picture editor:** Palmier Pro

**Narration and music:** Higgsfield Inworld TTS + Sonilo Music

**Visual review surface:** `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`

## Goal

Create two truthful launch videos from the same reviewed SPUR TUI recordings:

- A 45-second Product Hunt overview that establishes the product without rapid cutting.
- A 90-second guided demo that explains the operating model, evidence, and current truth boundary.

Both versions retain Direction A's restrained motion system: compact numbered cues, one slow push toward the relevant evidence per beat, and a thin progress rail. Duration changes the communication job, not the visual identity.

## Truth boundary

- Use only the five checksum-reviewed TUI recordings already imported into the Palmier project.
- Authored framing, labels, captions, progress indicators, color, camera movement, narration, and music may clarify the recordings but may not change their product meaning.
- Do not claim Reject → Retry → Approve or operator-owned review. The D4 attempt remains diagnostic and non-promotable.
- Do not generate UI, product footage, title cards, logos, or motion graphics with an image or video model.
- Preserve the 23.2-second Palmier baseline and its timeline. Build both versions on new timelines.

## Deliverable 1 — 45-second Product Hunt overview

The overview is 1,350 frames at 30 fps.

| Beat | Frames | Duration | Picture | Communication job |
|---|---:|---:|---|---|
| Title | 0–90 | 3 s | SPUR ink matte | Category and promise |
| Session | 90–300 | 7 s | Session Detail, inspected extended span | One durable operator home |
| Workers | 300–540 | 8 s | Worker visibility, inspected extended span | Worker state stays visible |
| Plan | 540–750 | 7 s | Plan state, inspected extended span | Honest plan inventory state |
| Routing | 750–990 | 8 s | Specialist routing, inspected extended span | Explicit agent/model/effort routing |
| Resume | 990–1,200 | 7 s | Session resume, inspected extended span | Context survives interruption |
| CTA | 1,200–1,350 | 5 s | SPUR ink matte | Install Community free |

### 45-second narration

Use Higgsfield **Inworld Text to Speech**, voice **Simon (en)**:

> Running more coding agents is easy. Keeping their work visible, isolated, and recoverable is the hard part. SPUR gives Claude Code, Codex, Kiro, and Gemini one durable outer harness. Start from Session Detail. See every worker and its current state. Inspect what is planned without pretending an empty state is progress. Route each task to the right agent, model, and effort. Then resume the same conversation without losing context. SPUR keeps the operator in view while the agents do the work. Install Community free.

The narration should end before the final second of the CTA. If the generated read exceeds 43 seconds, shorten the copy and regenerate rather than accelerating the voice beyond natural delivery.

## Deliverable 2 — 90-second guided demo

The guided demo is 2,700 frames at 30 fps.

| Beat | Frames | Duration | Picture | Communication job |
|---|---:|---:|---|---|
| Hook | 0–150 | 5 s | SPUR ink matte | Name the coordination problem |
| Problem | 150–450 | 10 s | Reviewed session/worker montage | More agents create more hidden state |
| Session | 450–810 | 12 s | Session Detail, inspected extended span | Explain the durable brain session |
| Workers | 810–1,230 | 14 s | Worker visibility, inspected extended span | Explain isolation and evidence visibility |
| Plan | 1,230–1,590 | 12 s | Plan state, inspected extended span | Explain plan inventory without overstating progress |
| Routing | 1,590–2,010 | 14 s | Specialist routing, inspected extended span | Explain explicit routing controls |
| Resume | 2,010–2,310 | 10 s | Full resume recording plus a short authored hold | Explain recovery and synchronized context |
| Truth | 2,310–2,520 | 7 s | Authored evidence ledger on ink matte | Separate proven behavior from blocked D4 review |
| CTA | 2,520–2,700 | 6 s | SPUR ink matte | Install Community free |

### 90-second narration

Use Higgsfield **Inworld Text to Speech**, voice **Simon (en)**:

> Running multiple coding agents can make more work disappear into more terminals. The difficult part is not asking an agent to write code. It is knowing what is running, where the result lives, and whether you can recover the context when something stops. SPUR is a common outer harness for Claude Code, Codex, Kiro, and Gemini. Session Detail gives the operator one durable place to ask, steer, and inspect. Worker state remains visible while tasks run in isolated worktrees. Plans show what exists and what does not; an empty state stays labeled as an empty state. Routing is explicit, so the operator chooses the agent, model, and effort instead of hiding those decisions behind automation. Saved conversations can resume without discarding the working context. The current product proves session, worker, routing, and resume visibility. Continuous human ownership of Reject, Retry, and Approve is still a product direction, not approved launch proof. SPUR keeps agent work visible, isolated, recoverable, and reviewable from one session. Install Community free.

The narration should end between seconds 84 and 88 so the CTA has a clean finish. If the generated read falls outside that window, revise the script and regenerate rather than applying an obvious speed change.

## Shared music system

Generate one 90-second track with Higgsfield **Sonilo Music** using this prompt:

> Instrumental minimal electronic score for a precise developer tool demo; calm forward pulse, warm analog texture, restrained percussion, no vocals, no dramatic drops, clear space for narration, confident quiet ending.

- The 90-second demo uses the complete track.
- The 45-second overview uses a clean 45-second edit of the same track, preserving the opening identity and resolving on the CTA.
- Do not generate separate music identities for the two cuts.
- Do not add synthetic UI sound effects unless later review identifies a specific comprehension need.

## Audio mix and muted-view behavior

- Normalize narration consistently across both versions and keep music clearly beneath speech.
- Fade music under the opening sentence, truth-boundary sentence, and final CTA.
- Avoid hard music edits; use short constant-power fades around the 45-second cut.
- Generate captions from the imported narration, then correct them against the approved scripts.
- Captions and authored proof cues must preserve the complete story when playback is muted.
- Do not cover the terminal status bar, worker evidence, routing controls, or resume marker.

## Visual system

- Keep the SPUR palette: ink `#0B0E14`, ivory `#E6E1CF`, signal cyan `#7FB4CA`, routing violet `#957FB8`, and border `#2A2E38`.
- Use compact upper-left numbered cues and a thin progress rail above the lower safe area.
- Use direct cuts. Avoid ornamental transitions, glow effects, fake cursor motion, or synthetic interface animation.
- Establish each proof state before motion begins. Apply one restrained push from 100% to no more than 108%, anchored toward the real evidence region.
- Apply one shared, subtle color treatment across the five recordings. Preserve terminal legibility and dark-surface separation.
- The 90-second truth card must label **PROVEN NOW** and **NOT PROVEN YET**; it must not resemble product UI.

## Production flow

1. Inspect the extended source windows in Palmier before selecting the longer spans.
2. Generate the two narration files and one 90-second music file through the authenticated Higgsfield CLI.
3. Download stable local copies of the generated audio and import them into the Palmier project.
4. Create separate `Kinetic Operator Cut — 45s` and `Kinetic Operator Cut — 90s` timelines at 1920×1080, 30 fps.
5. Assemble picture from inspected source spans and authored matte cards.
6. Add narration, music, proof cues, progress rails, captions, shared color, and restrained keyframed pushes.
7. Inspect the title, every proof beat, truth card, and CTA in Palmier before export.
8. Export H.264 1080p candidates without replacing the baseline or proof-manifest hero.

## Output paths

- `ph_ready/hero-video-palmier-pro-45s.mp4`
- `ph_ready/hero-video-palmier-pro-90s.mp4`

Both files remain review candidates until explicitly promoted.

## Acceptance criteria

- The outputs are exactly 45.0 and 90.0 seconds at 30 fps.
- Both are H.264, 1920×1080, `yuv420p`, and fully decode with FFmpeg.
- Both contain the same five reviewed proof beats in the approved order.
- The 90-second version includes the explicit proven-versus-blocked truth boundary.
- Narration uses one consistent English voice and matches the approved scripts after caption correction.
- Music has no vocals and remains subordinate to narration.
- Every cue and caption is readable at 512-pixel inspection width without covering key TUI evidence.
- Camera motion is visible but restrained; no proof frame is cropped into ambiguity.
- No paid image/video generation, generated UI, unreviewed recording, or stronger D4 claim appears.
- The baseline video and timeline remain intact.

## Verification

- Inspect representative frames and audible transitions through Palmier.
- Poll every Higgsfield generation and Palmier export to terminal completion.
- Verify codec, audio/video streams, resolution, pixel format, frame rate, duration, and byte size with `ffprobe`.
- Decode both files completely with FFmpeg.
- Measure integrated loudness and confirm narration remains intelligible over music.
- Build and inspect contact sheets for both versions.
- Record SHA-256 checksums in the handoff.

## Non-goals

- Synthetic product footage, generated UI, presenter video, or avatar advertising.
- Product Hunt submission, YouTube publishing, or replacing the approved proof manifest.
- Claims about continuous human review or the blocked D4 flow.
