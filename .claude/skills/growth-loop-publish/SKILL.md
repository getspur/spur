---
name: growth-loop-publish
description: Use when publishing today's growth-loop X drafts to Buffer as TRUE drafts (never auto-published). Draft-only skill for the post-artifact publish handoff; never queues, schedules, or live-posts.
---

# growth-loop-publish

Thin wrapper around `scripts/publish-to-buffer.mjs`. The brain decides **whether** to invoke it and records the outcome; the script owns parsing, R2 upload, Buffer API calls, and artifact mutation.

## When to invoke

After `growth-loop` step 9 (`emit-summary`), and only when both are true:

1. Today's artifact has a `## Drafts — X` section.
2. Today's artifact does **not** already have `## Published as drafts`.

If either check fails, stop. This is the idempotency gate.

## Preflight checks

Before invoking the script, the brain MUST verify:

- `BUFFER_ACCESS_TOKEN` is set in env. Do not inspect, print, or echo its value.
- `R2_PUBLIC_BASE_URL` is set in env.
- Buffer X channel id is known from project-local config or `BUFFER_X_CHANNEL_ID`. Do not hardcode it in this skill.
- `wrangler` is installed and authenticated.
- Today's artifact exists at `resource/growth-loop/YYYY-MM-DD.md`.

`R2_BUCKET` is optional; the script defaults it.

## Invocation

Default artifact path is today:

```sh
node scripts/publish-to-buffer.mjs --channel-id $BUFFER_X_CHANNEL_ID
```

Dry run:

```sh
node scripts/publish-to-buffer.mjs --dry-run --channel-id $BUFFER_X_CHANNEL_ID
```

Use `--artifact <path>` only for an explicit non-today artifact. Do not add wrapper flags.

## Invariants

1. **Never read or echo `BUFFER_ACCESS_TOKEN`.** Presence check only.
2. **Queue mode is forbidden.** `saveToDraft: true` is enforced in `scripts/publish-to-buffer.mjs`; the brain must not bypass the script.
3. **On script failure, log the failure to today's artifact and STOP.** Do not retry. Do not fall back to MCP. Do not call Buffer GraphQL directly from the brain.
4. **The script is authoritative.** Do not duplicate its parsing, upload, or Buffer request logic in the skill or brain session.

## What to do with the result

- Success: the script appends `## Published as drafts` with Buffer draft ids and UI URLs. Print a short summary to the user.
- Failure: append a short failure note to today's artifact with the command, exit code, and redacted stderr/stdout. Then stop.

## Smoke test

For the Buffer token/channel smoke test, use the curl in `scripts/publish-to-buffer.README.md`. Do not duplicate it here.

## Failure modes

- **Preflight fails**: missing env, no channel id, unauthenticated `wrangler`, or no artifact. Stop before running the script.
- **R2 upload fails**: script exits `2`. Check Wrangler auth, bucket access, and image paths.
- **Buffer 401/403**: script exits `3`. Rotate `BUFFER_ACCESS_TOKEN` immediately.
- **Buffer 4xx with MutationError**: script exits `3`. Check channel id, draft payload shape, and Buffer response.
- **Network timeout**: script exits non-zero. Log once and stop; no retry in-session.
