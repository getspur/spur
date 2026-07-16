# SPUR Product Launch Buy-In Video Design

**Date:** 2026-07-16
**Status:** Approved direction, pending implementation plan

## Goal

Produce a 35-second Product Hunt hero that earns attention and confidence while keeping every product claim grounded in reviewed SPUR footage. Higgsfield supplies a short branded opening treatment and attention analysis. Real TUI captures remain the product proof.

The existing 25.2-second `hero-video-ph-ready.mp4` is technically correct but behaves like an evidence reel. It lacks a strong problem hook, gives several sparse frames too little context, and includes an empty-plan beat that weakens buy-in.

## Audience and outcome

The primary viewer is a technical Product Hunt visitor who operates multiple CLI coding agents. Within 35 seconds, the viewer should understand:

1. Multiple agents create an operational control problem.
2. SPUR keeps the operator in one session.
3. Worker state remains visible.
4. Agent, model, and effort are explicit before dispatch.
5. A prior conversation can be resumed.
6. SPUR is available to install now.

The desired action is to continue into the Product Hunt page, inspect the proof gallery, or install SPUR.

## Editorial model

The video is a hybrid, not an AI-generated product demo.

- Higgsfield-generated material occupies no more than the first five seconds.
- Generated material communicates the problem and brand atmosphere only.
- Generated frames must not be presented as the SPUR interface or as evidence of product behavior.
- All product claims use approved real captures from `docs/product_launch/media_pack/live_demos/`.
- The empty-plan segment is omitted from the buy-in edit.
- The current evidence reel is retained separately for technical review and provenance.

## Timeline

| Time | Role | Content |
|---:|---|---|
| 0 to 5s | Hook | Higgsfield brand motion: scattered agent activity resolves into one calm control plane. No invented legible UI. |
| 5 to 9s | Problem and promise | Kinetic SPUR title: `Your coding agents need an operator.` followed by `SPUR is the control tower.` |
| 9 to 15s | Session proof | Real Session Detail capture, cropped for the composer and mode state. Caption: `Run every coding agent from one session.` |
| 15 to 21s | Worker proof | Real worker visibility capture, cropped around the WORKERS result and state. Caption: `See what every worker is doing.` |
| 21 to 27s | Routing proof | Real specialist routing capture, cropped around agent, model, and effort. Caption: `Choose the right agent before dispatch.` |
| 27 to 31s | Resume proof | Real session-resume capture. Caption: `Close the terminal. Resume without losing context.` |
| 31 to 35s | Payoff | SPUR end card with `Run agents. See the work. Keep control.` and the install command. |

Each real proof beat gets at least four seconds of readable screen time. Crops and restrained push-ins may guide attention, but must not modify terminal content.

## Visual and audio direction

Use the existing SPUR launch system: near-black background, warm off-white type, cyan evidence accents, violet secondary accents, square geometry, and editorial rather than glossy SaaS composition. Avoid gradients, fake dashboards, generic robot imagery, and generated terminal text.

The opening may use abstract terminal light, command streams, and spatial convergence as a metaphor for consolidation. It should end in a composition that cuts naturally into the SPUR title card.

Sound should provide a clear opening pulse, restrained transitions, and a confident payoff. If the Higgsfield output includes usable generated audio, it may be used as sound design after review. Product understanding must not depend on audio; all claims and the CTA remain readable with sound off.

## Production architecture

1. Use Higgsfield Marketing Studio for the commercial opening, anchored by an approved SPUR reference image.
2. Score the current evidence reel with Higgsfield Virality Predictor to establish a baseline.
3. Build the 35-second candidate locally with deterministic HTML title/caption frames and ffmpeg cuts from approved sources.
4. Keep the Higgsfield clip in the marketing layer and label it as generated treatment.
5. Render the candidate to H.264, 1920 by 1080, 30 fps, with AAC audio when reviewed sound is available.
6. Score the candidate with Virality Predictor and visually inspect a timeline contact sheet.
7. Publish the candidate as `ph_ready/hero-video-ph-ready.mp4` only after it improves the hook and contains no misleading generated UI. Preserve the current reel as `ph_ready/hero-video-evidence-reel.mp4`.

## Truth and provenance

`proof-manifest.json` remains authoritative for real source checksums, proof terms, timestamps, and crops. The buy-in graph may omit approved assets but cannot introduce an unapproved product claim.

The generated hook must be recorded separately with its Higgsfield model, prompt, generation URL, and local source path. The launch handoff must clearly distinguish `generated brand hook` from `real product proof`.

## Failure handling

- If Higgsfield authentication is unavailable, stop and request login.
- If Marketing Studio produces fake or legible invented UI, reject the clip and regenerate with a more abstract prompt.
- If generated audio is distracting or contains inaccurate narration, discard it and ship the video mute-safe.
- If the final attention score does not improve, do not claim an improvement. Use visual review to decide whether to retain the candidate or the evidence reel.
- If any source checksum drifts, fail before rendering product proof.

## Verification

Automated checks must verify:

- output duration is 35 seconds within a 0.5-second tolerance;
- output is H.264 at 1920 by 1080 and 30 fps;
- the generated hook occupies at most the first five seconds;
- every real segment resolves to an approved manifest asset;
- the empty-plan asset is absent from the buy-in graph;
- all captions resolve to visible proof or bounded benefit copy;
- the preserved evidence reel still exists;
- the final HTML handoff points to the new buy-in hero and identifies generated treatment;
- the render remains understandable without audio.

Manual review must inspect the opening hook, embedded-size text legibility, product-truth boundary, pacing, transition continuity, and CTA hold. Virality Predictor results are comparative evidence, not a substitute for product-truth review.

## Non-goals

- Generating a fictional SPUR interface
- Claiming active delegation or plan progress absent from the reviewed captures
- Replacing the five Product Hunt proof stills
- Creating vertical social variants in this pass
- Running a live agent-spend capture
