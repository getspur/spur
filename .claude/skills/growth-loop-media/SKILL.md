---
name: growth-loop-media
description: Use when the growth-loop needs visual assets (X thread cover, Reddit post hero, OG image) — delegates image generation to a Codex worker via SPUR's delegate_to_worker, which calls gpt-image-1 in an isolated worktree. Returns file paths into the day's growth-loop artifact. Never generates images in the brain session.
---

# growth-loop-media

Delegates **image generation only** to a Codex worker. The brain (this session) writes the brief; Codex executes it via OpenAI's `gpt-image-1` and returns file paths. This keeps the brain's context small and parallelizes media work alongside the rest of the growth-loop DAG.

## When to invoke

From inside `growth-loop` step 6 (`draft-posts`), after you have the post copy. For each draft, ask: *would a visual meaningfully lift engagement?* If yes, build a brief and delegate. If no, skip — never generate filler imagery.

Typical yes-cases:
- X thread cover (single hero image for a 3–7 tweet thread)
- Reddit text post with a complex concept that benefits from a diagram
- OG / link-preview image for a linked blog post or landing page

Typical no-cases:
- Single X reply
- Reddit comment reply
- Short text-only X post

## Inputs (the brief)

Every delegation must include all of these. Missing fields = stop and ask the brain to fill them in. No defaults.

- **purpose**: which draft this serves (e.g. "X thread cover for theme: rate-limit failover")
- **dimensions**: one of `1024x1024` (square), `1536x1024` (landscape), `1024x1536` (portrait). X covers → landscape. Reddit hero → landscape. OG → landscape.
- **count**: 1–3 variants. Default 2 for hero images, 1 for OG.
- **style guide**: **mandatory inline.** Read `marketing/brand-visual.md` from the brain's tree, then paste its "Style descriptor" block (and any relevant motifs / forbidden list / palette) **verbatim into the task brief**. Do NOT pass by file path only — the worker may be based on a commit that predates the file. If `marketing/brand-visual.md` is missing, **stop and signal** rather than guessing style. Optionally also skim `marketing/product-marketing.md` for voice/positioning context to inline.
- **prompt**: the actual gpt-image-1 prompt. Be explicit about what's *in* and what's *not* in the image. Forbid: stock-photo people, AI-cliché glowing brains, generic "futuristic" backgrounds.
- **output_dir**: always `resource/growth-loop/YYYY-MM-DD/images/`
- **filename_prefix**: short slug derived from purpose, e.g. `x-thread-failover`

## Delegation call

Use `mcp__spur-mcp__delegate_to_worker` with:
- `worker`: `codex`
- `worktree`: isolated (default — do NOT share the brain's worktree)
- `task_prompt`: the template below, filled in

```
You are generating marketing images for SPUR's daily growth-loop.

Use OpenAI gpt-image-1. Do not write code unless needed to call the API; a single shell/python invocation is fine. Do not generate any other content. Do not edit any other files.

Brief:
- Purpose: <purpose>
- Dimensions: <dimensions>
- Count: <count>
- Style guide: <style guide text or reference to marketing/brand-visual.md>
- Prompt: <prompt>
- Forbidden: stock-photo people, AI-cliché glowing brains, generic "futuristic" backgrounds, unless explicitly requested.

Output:
- Write each image to resource/growth-loop/<YYYY-MM-DD>/images/<filename_prefix>-<n>.png
- Return a JSON block listing each generated file path, its dimensions, the exact prompt sent to gpt-image-1, and the model version used.
- Do not post the images anywhere. Do not commit.

Cost cap: 3 images per delegation. If the brief asks for more, generate the first 3 and report the rest as skipped.
```

## What the brain does with the result

1. Verify each returned path exists.
2. Append an `## Images` subsection under the relevant draft in `resource/growth-loop/YYYY-MM-DD.md`, listing each path + the prompt used + the model version.
3. If Codex returned an error, log it under `## Images — FAILED` and continue. Do **not** retry in-session — note it for next-day diagnosis.

## Invariants

1. **Never generate images in the brain session.** Always delegate. The brain writes briefs, not pixels.
2. **One delegation per draft.** Do not bundle multiple drafts' image needs into one Codex task — keep blast radius small.
3. **Draft-only, never publishes.** Codex writes files; no upload, no posting.
4. **Cost ceiling: 3 images per draft, 3 drafts per day → 9 images max per growth-loop run.** If the brief wants more, stop and ask the user.
5. **No retries.** A failed delegation logs and moves on. The artifact records the failure for human review.
6. **Workers see only tracked files.** SPUR workers base their worktree on the last committed HEAD (`BaseSpec::RepoMain`), not the brain's working tree. Anything the worker needs to read — `brand-visual.md`, competitor profiles, voice docs — MUST be committed to the repo before the brain delegates. The brain MUST also inline any critical guidance directly in the task brief so the delegation succeeds even if the file is renamed, moved, or missing from the worker's base commit. **Path references are advisory; inlined content is authoritative.**

## Failure modes

- **Codex worker not configured** → write `## Images — codex-unavailable` to the artifact, skip image work for the run, continue.
- **OpenAI quota / API error** → Codex reports it; brain logs under `## Images — api-error` with the message.
- **Output file missing despite success report** → treat as failure; log path that was promised.
