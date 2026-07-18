# Product Hunt real-repository hero redesign

**Status:** Approved; four-agent amendment approved 2026-07-18

**Date:** 2026-07-18

**Supersedes:** the narrative and proof model in `2026-07-17-product-hunt-kinetic-hero-design.md`; prior timelines and exports remain archival evidence and must not be overwritten.

## Goal

Produce truthful 45-second and 90-second SPUR launch videos that demonstrate one causal interaction in the real `spur` repository: an operator asks the brain to audit the Product Hunt media pack, the brain submits a populated plan, real workers perform read-only deep dives, the operator reviews their evidence, and the brain synthesizes the approved findings in the same durable session.

The 45-second Product Hunt hero shows a readable approval path. The 90-second explainer shows the full evidence-driven Reject → Retry → Approve loop. Both cuts derive from the same real campaign and preserve the same worker, task, plan, repository, and session identities.

## First-principles decisions

1. **Demonstrate the category claim.** A list of supported vendors is weaker than showing one harness coordinating real ACP-compatible workers.
2. **Use one cause-and-effect story.** A continuous prompt → plan → workers → review → synthesis loop is easier to understand than a montage of unrelated surfaces.
3. **Show real project state.** An empty plan is honest UI but not proof of orchestration. The launch videos require a populated plan inside the real `spur` project.
4. **Give proof time to breathe.** The 45-second cut omits the rejection detour; the 90-second cut earns the additional complexity with longer evidence dwells.
5. **Fail closed.** Missing plan identity, worker evidence, HITL correlation, repository identity, or synthesis makes a capture non-promotable.
6. **Preserve history.** Existing captures, Palmier timelines, audio, and exports remain intact. The redesign creates new versioned assets.

## Locked positioning

Primary line:

> SPUR gives any ACP-compatible coding agent—from Claude Code and Codex to Grok, OpenCode, and beyond—one durable outer harness.

The first expanded onscreen reference defines `ACP` as `Agent Client Protocol`. The videos do not claim that every named product has identical native transport behavior; they claim that SPUR supplies a common outer harness for ACP-compatible coding-agent operation.

## Audience and intended understanding

The primary audience is a Product Hunt viewer who knows coding agents but has not yet formed a mental model of SPUR. After the 45-second cut, the viewer should understand that SPUR gives the operator one place to plan, delegate, inspect, approve, and synthesize real agent work. After the 90-second cut, the viewer should also understand that evidence can be rejected, retried, approved, and returned to the brain without losing task or conversation identity.

## Source campaign

### Repository and isolation

Run the capture in an isolated checkout under `.spur/worktrees/` while pointing SPUR at the real beads-backed `spur` project. Keep the repository and branch identity visible in the TUI whenever the video makes a real-project claim.

### Visible user prompt

> Audit this Product Hunt media pack against the real `spur` repository. Submit a plan and delegate four read-only deep dives on ACP positioning, TUI proof, launch readiness, and media handoff. Route them to Claude Code, Grok, Codex, and OpenCode. Return findings for my approval before any edits.

### Required plan

The brain creates one populated plan with four independently visible tasks:

| Task | Worker | Question | Scope |
|---|---|---|---|
| ACP positioning | Claude Code | Is the category line accurate and understandable given SPUR's real ACP integration boundaries? | Read-only documentation and source inspection |
| TUI proof | Grok | Is each launch claim supported by a real captured TUI state and an identifiable source window? | Read-only media, manifest, and journey inspection |
| Launch readiness | Codex | Are the story, pacing, and accessibility complete and internally consistent? | Read-only launch-pack review |
| Media handoff | OpenCode | Do the manifest, source locks, filenames, and Product Hunt delivery notes form a truthful handoff? | Read-only manifest and delivery inspection |

The plan must expose the plan ID, all four task IDs, worker identity, model, effort, attempt, state, and result evidence. The Workers panel must show exactly four current-run workers and visibly attribute Claude Code, Grok, Codex, and OpenCode before review begins.

### Human review path

The operator inspects worker evidence in Session Detail and the plan/review surfaces.

- The 45-second derivation uses a task that reaches approval without showing a rejection detour.
- The 90-second derivation includes one real result that is insufficient for a specific, visible evidence requirement. The operator states the missing proof, rejects the attempt, retries the same task, inspects the new evidence, and approves it.
- If every initial result fully meets its evidence contract, the production must not fabricate failure. Stop and refine the audit question or capture another legitimate evidence-review case.
- All four findings must be approved before they feed the final brain synthesis.

## 45-second Product Hunt hero

### Storyboard

| Time | Beat | Required proof |
|---:|---|---|
| 0–4 s | ACP category hook | Stable title plate: `ANY ACP-COMPATIBLE CODING AGENT` / `ONE DURABLE OUTER HARNESS` |
| 4–11 s | User prompts the brain | Real Session Detail, visible prompt, real `spur` repository context |
| 11–19 s | Brain submits a populated plan | Named plan and four task identities; never an empty-plan substitute |
| 19–27 s | Workers perform deep dives | Four worker identities, model/effort, current state, and evidence remain legible |
| 27–34 s | Operator approves a grounded result | Selected task and approval action correlate |
| 34–40 s | Brain synthesizes | Final answer appears in the same durable session and references approved findings |
| 40–45 s | CTA | Stable SPUR plate with `beta.otobank.com` and `COMMUNITY FREE` |

### Narration

> More coding agents create more hidden work. SPUR keeps it visible, reviewable, and recoverable. SPUR gives any ACP-compatible coding agent—from Claude Code and Codex to Grok, OpenCode, and beyond—one durable outer harness. Inside the real SPUR repository, the operator asks the brain to audit this Product Hunt launch. The brain submits a populated plan and delegates four read-only deep dives. Worker state and evidence stay visible. The operator approves the result. The brain synthesizes the findings in the same durable session. Try SPUR Community free.

The script is 85 words. Narration should land in approximately 39–41 seconds, leaving a four-to-six-second visual/audio tail. Do not speed the voice to resolve an overrun.

## 90-second guided explainer

### Storyboard

| Time | Beat | Required proof |
|---:|---|---|
| 0–7 s | Problem and ACP positioning | Stable category plate and expanded `Agent Client Protocol` definition |
| 7–17 s | User prompts the brain | Same real Session Detail prompt and repository identity |
| 17–28 s | Brain submits the plan | Same populated four-task plan |
| 28–41 s | Workers deep-dive | Same campaign, all four real worker identities, states, attempts, and evidence |
| 41–53 s | Inspect and reject | Specific missing source proof is visible; rejection targets the correlated task/attempt |
| 53–65 s | Retry with evidence request | Same task produces a new attempt with the requested evidence |
| 65–75 s | Approve | Operator approves the grounded retry |
| 75–84 s | Brain synthesis | Approved findings return to the brain in the same conversation |
| 84–90 s | CTA | Stable SPUR plate with `beta.otobank.com` and `COMMUNITY FREE` |

### Narration

> More coding agents create more hidden work. The hard part is knowing what is running, where the evidence lives, and whether the operator can recover the context. SPUR gives any ACP-compatible coding agent—from Claude Code and Codex to Grok, OpenCode, and beyond—one durable outer harness.
>
> Inside the SPUR repository, the operator asks the brain to audit this Product Hunt launch. The brain submits a populated plan with four read-only deep dives: ACP positioning, TUI proof, launch readiness, and media handoff.
>
> Each worker keeps its agent, model, effort, attempt, and current state visible. The operator can clearly inspect plan and evidence without leaving the durable session.
>
> One finding arrives without enough source proof. The operator rejects it and explains what is missing. SPUR retries the same task instead of hiding the failure. The new attempt returns with the requested evidence, and the operator approves it.
>
> Only approved findings return to the brain. It synthesizes the launch recommendation in the same conversation, with the real project still in view.
>
> SPUR makes multi-agent work visible, reviewable, and recoverable. Try SPUR Community free.

The script is 179 words. Narration should land in approximately 84–86 seconds, leaving a four-to-six-second visual/audio tail.

## Visual grammar

### Product frames

- Keep the real TUI full-frame unless a crop preserves every proof term named by the beat.
- Preserve repository/branch context, plan/task identity, worker state, attempt, review action, and resume/synthesis markers at their claim moments.
- Use one upper cue and one bottom caption sentence per beat.
- Captions must not cover status rows, review controls, evidence, or repository context.
- Use the existing cyan progress rail to orient viewers without adding fake product UI.
- Use restrained 100→106/108% pushes only after proof is readable at the first frame.

### Typography and palette

- Long text plates: SF Mono, 76 px, ink `#0B0E14`, ivory `#E6E1CF`.
- Signal color: cyan `#7FB4CA`.
- Secondary review/accent color: violet `#957FB8`.
- Product captions use the approved high-contrast bottom-center style.
- Title and CTA are notebook-authored constant-frame H.264 plates to avoid Palmier editor-text instability.

### CTA

The final plate contains:

```text
SPUR
ONE DURABLE OUTER HARNESS
beta.otobank.com
COMMUNITY FREE
```

The domain remains onscreen; the narration ends with `Try SPUR Community free.`

## Audio

- Regenerate both narrations with the same approved Higgsfield voice identity because the scripts changed.
- Reuse the approved 45-second and 90-second instrumental identities.
- Preserve stereo 48 kHz delivery and narration-first ducking.
- Do not time-stretch narration. If timing misses the target, revise or regenerate it.
- Captions must exactly match the approved scripts, ignoring punctuation and case.

## Production flow

1. Run a bounded authenticated worker preflight for Claude Code, Grok, Codex, and OpenCode; advertised availability alone is insufficient.
2. Capture one real full HITL campaign at 2560×1600 with stable `.cast`, `.mp4`, and `.log` outputs.
3. Promote the capture only after repository, plan, all four tasks, workers, reviews, and synthesis identities correlate.
4. Lock the promoted source by SHA-256 and record exact proof windows in the media manifest and notebook.
5. Use the notebook as the review source of truth for scripts, frame windows, title/CTA plates, and contact sheets.
6. Create new Palmier timelines named `Real Repository Loop — 45s v2` and `Real Repository Loop — 90s v2`.
7. Export versioned files to `ph_ready/hero-video-real-repo-45s-v2.mp4` and `ph_ready/hero-video-real-repo-90s-v2.mp4`.
8. Decode and sample the actual exports externally; Palmier preview samples are advisory only.
9. Promote filenames in the media manifest only after both automated checks and human review pass.

## Capture pacing

- Capture geometry: 2560×1600, approximately 200×50 PTY.
- Story playback: 1.0–1.15×.
- Land/orientation dwell: 2.5–3.5 seconds.
- Plan, worker, and evidence dwell: 3.0–4.5 seconds.
- Before/after review action: at least two seconds before and three seconds after.
- The 45-second cut allocates 6–8 seconds per product state.
- The 90-second cut allocates 9–13 seconds per product state.

## Promotion gates

| Gate | Pass condition |
|---|---|
| Capture identity | Stable cast, video, and log exist and correlate to the real `spur` checkout |
| Plan truth | A named populated plan and all four task identities are visible |
| Worker truth | Claude Code, Grok, Codex, and OpenCode plus model, effort, attempt, state, and evidence are visible at claim time |
| HITL truth | Reject rationale, retry, approval, and final synthesis correlate to the same task/session |
| Source lock | SHA-256 and proof windows match the exact promoted source bytes |
| Copy lock | Corrected caption words equal the approved 85/179-word scripts |
| Export lock | H.264 1920×1080, 30 fps, exactly 1,350/2,700 frames, AAC stereo 48 kHz |
| Decode lock | Full external decode succeeds without truncation |
| Visual QA | Boundary/midpoint frames retain plates, captions, proof, and repository identity |
| Preservation | Baseline project, old timelines, prior exports, and reviewed source files remain unchanged |

## Failure handling

- Empty or unidentified plan: stop promotion and recapture; never substitute the old empty-plan footage.
- Unauthenticated or unhealthy requested worker: stop before the campaign prompt; do not rely on retry exhaustion or fallback as launch proof.
- Missing source evidence: use a specific real review request and retry the correlated task.
- No final synthesis: preserve diagnostics but do not promote the capture.
- Unreliable Palmier preview: inspect the exported file externally; replace unstable text with notebook-authored constant-frame plates.
- Narration overrun: edit or regenerate narration; never compress proof dwells below the locked pacing.
- Existing timeline/output collision: create a new versioned name; never overwrite without a separate explicit promotion decision.

## Approved design decisions

- Story anchor: launch-readiness audit inside the real `spur` repository.
- HITL depth: concise approval in 45 seconds; full Reject → Retry → Approve in 90 seconds.
- Worker scope: read-only.
- Worker roster: Claude Code for ACP positioning, Grok for TUI proof, Codex for launch readiness, and OpenCode for media handoff.
- Positioning: category-first ACP-compatible agent line.
- CTA: `beta.otobank.com` plus `COMMUNITY FREE`.
- Source strategy: one full real campaign feeds both cuts.
- Preservation: all prior media and Palmier timelines remain archival.

No unresolved design choices remain.
