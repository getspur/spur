---
name: growth-loop
description: Use when running the daily SPUR growth-loop workflow (cron-fired) — a deterministic DAG that researches X + Reddit + competitor moves, drafts posts and replies for review, and logs the day's artifact to resource/growth-loop/YYYY-MM-DD.md. Draft-only; never auto-posts.
---

# growth-loop

A **deterministic, cron-driven** marketing/growth workflow. Same DAG every run, fresh content. Sibling pattern to `submit_plan` but for recurring business-growth work rather than one-shot engineering tasks.

## Invariants (do not adapt away)

1. **Read product context first.** Always load `marketing/product-marketing.md` before drafting anything. Voice, ICP, positioning, and the "peers-not-competitors" framing live there. If it's missing, **stop and emit a signal** — do not proceed with guessed positioning.
2. **Draft-only.** Never call any tool that posts to X, Reddit, or any external surface. Output is markdown on disk for human review.
3. **One artifact per run.** Exactly one file written: `resource/growth-loop/$(date +%Y-%m-%d).md`. If today's file exists, append a `## Re-run HH:MM` section — never overwrite.
4. **Fixed DAG, fresh content.** Tasks and section headings are identical every run; only the underlying research and drafts change.
5. **Cite sources.** Every claim ("trending on X", "top post in r/rust today") gets a URL + a one-line excerpt. No unsourced trend claims.

## Inputs

- **Required:** `marketing/product-marketing.md`
- **Recommended:** `marketing/competitors/*.md`, last 3 days of `resource/growth-loop/*.md` (to avoid repeating yesterday's angle)
- **Channels (v1):** X (developer Twitter) + Reddit. Subreddits to scan: `r/rust`, `r/programming`, `r/ClaudeAI`, `r/LocalLLaMA`, `r/ChatGPTCoding`, `r/cursor`. Adjust the list in `templates/daily-prompt.md` — not here.

## The DAG (fixed)

Execute in this exact order. Each numbered step is one TodoWrite item.

1. **load-context** — Read `marketing/product-marketing.md`, the last 3 files in `resource/growth-loop/` (if any), and `marketing/competitors/` index. Note yesterday's angle so today's is different.
2. **research-x** — Use WebSearch / WebFetch to find: (a) trending posts in the AI-coding-agent space from the last 24h, (b) any mentions of SPUR or close peers (Claude Code, Codex, Aider, Cursor, Continue, Cline), (c) one underserved question SPUR is uniquely positioned to answer.
3. **research-reddit** — Same as above for the subreddit list. Specifically look for **questions with <5 replies** where SPUR's value prop is on-topic — these become reply candidates.
4. **research-competitors** — Skim `marketing/competitors/` and check each peer's X / blog / changelog for shipped-in-last-24h items. One-line each.
5. **synthesize-themes** — From steps 2–4, pick **one** theme of the day. Single sentence. Must be different from the last 3 days.
6. **draft-posts** — Produce: 1 X post (≤270 chars), 1 longer X thread outline (3–7 tweets), 1 Reddit text post (subreddit named). All in SPUR's voice from `product-marketing.md`. Peers-not-competitors framing.
6.5. **draft-media** *(optional)* — For each draft from step 6, decide if a visual meaningfully lifts engagement. If yes, invoke the `growth-loop-media` skill, which delegates image generation to a Codex worker via `mcp__spur-mcp__delegate_to_worker`. Brain never generates images itself. Skip silently for drafts where imagery would be filler.
7. **draft-replies** — Produce 3–5 Reddit reply drafts targeting the under-replied questions from step 3. Each names the subreddit + post URL + the reply body. **Never lead with the product** — answer the question first, mention SPUR only if naturally relevant.
8. **write-artifact** — Write everything to `resource/growth-loop/$(date +%Y-%m-%d).md` using the schema in `templates/run-template.md`.
9. **emit-summary** — Print a 5-line summary to the user: theme, post count, reply count, top opportunity, artifact path.

## What this skill does NOT do

- Does not post to any external surface (v1).
- Does not optimize ad spend, cold email, SEO, or paywalls — for those, use the dedicated `marketing-*` skills directly.
- Does not measure outcomes. After you post manually, log results in the next day's `## Yesterday's results` section (template handles this).

## Wiring (one-time, by the user)

To schedule this daily, the user runs (in Claude Code, not invoked by this skill):

```
/schedule — fire `templates/daily-prompt.md` once per day at 09:00 user-local time
```

Or directly via `CronCreate` with the daily-prompt body. **This skill never schedules itself.**

## Failure modes

- **Missing product-marketing.md** → stop, write `resource/growth-loop/YYYY-MM-DD.md` with only a `## BLOCKED` section explaining what's missing. Do not draft.
- **Web search unavailable** → write the artifact with `## Research: UNAVAILABLE` and stop before drafting. No invented trends.
- **Today's artifact already has 3+ re-run sections** → stop; the workflow is being fired too often.
