# Daily growth-loop prompt (verbatim cron payload)

This is the exact prompt body to schedule via `CronCreate` (or `/schedule`). The cron should fire it once per day. Edit subreddit list / X focus topics here — not in `SKILL.md`.

---

Invoke the `growth-loop` skill and execute its DAG end-to-end for today.

**Channels:** X (developer Twitter) + Reddit.
**Subreddits:** r/rust, r/programming, r/ClaudeAI, r/LocalLLaMA, r/ChatGPTCoding, r/cursor.
**X focus topics:** AI coding agents, Claude Code rate limits, multi-agent orchestration, agent cost tracking, parallel coding workflows.
**Peers to watch (not competitors):** Claude Code, Codex, Aider, Cursor, Continue, Cline, OpenCode, Kiro.

**Required reads before drafting:** `marketing/product-marketing.md`, last 3 files in `resource/growth-loop/`, `marketing/competitors/` index.

**Output:** Exactly one file at `resource/growth-loop/$(date -u +%Y-%m-%d).md` following `.claude/skills/growth-loop/templates/run-template.md`. If the file exists, append a `## Re-run HH:MM` section.

**Hard rules:**
- Draft-only. Do not call any posting tool.
- No invented trends — every trend claim needs a URL + excerpt.
- Theme of the day must differ from the last 3 days.
- Peers-not-competitors framing throughout.
- Reply drafts answer the question first; mention SPUR only if naturally relevant.

End with the 5-line summary specified in step 9 of the skill.
