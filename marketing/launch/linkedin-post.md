# LinkedIn — launch-day post

Single long-form post (not a thread — LinkedIn's algorithm rewards dwell time on one post over carousels-of-text or comment-chains for B2B launches). Two drafts below; recommendation at the bottom.

Audience tilt vs. X: more Team Leads / EMs / VPs Eng, fewer hands-on-keyboard ICs. Both drafts lead with the same VOC anchor (`marketing/research/voc.md:54-56` — roxolotl, HN 44598254) but choose different POVs.

---

## Draft A — Team Lead pain-led (recommended)

~1,360 chars. POV: peer-to-peer, surfacing a quote and a thesis. Reads like someone sharing a finding, not announcing a product.

```
A coworker on Hacker News a few weeks back: "A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month."

Read that twice. The team thought they were spending $200/month on AI coding. One engineer was actually burning $1k/week. The gap between perception and reality was roughly 5x — and it took a casual Slack-style aside on a public forum to surface it.

That's the bet behind SPUR, which we're shipping today.

If your team runs Claude Code, Codex, Gemini, Kimi, or OpenCode from the terminal — and most teams I talk to are running two or three of them, often on Max-tier plans the company doesn't see itemized — you don't have a per-developer spend problem. You have a visibility problem. Every vendor bills separately. Every CLI reports its own usage. No one owns the rollup.

SPUR is the control tower for those CLI agents. One live cost ledger across all five vendors, durable plans that survive across sessions, and a review queue so the human stays in the loop. We surface spend; we don't gate it. Single Rust binary, BYO-key, no SaaS sign-up.

curl -sSL https://getspur.dev/install.sh | sh

If you manage 3–10 engineers running AI agents day-to-day — what would you want to know before recommending this to your team? Genuinely asking.
```

---

## Draft B — VP-Eng / "we just shipped" framing

~1,400 chars. POV: company voice, less personal, explicit Team-tier callout. Use this if the personal-essay tone in Draft A feels off-brand for the org's LinkedIn page (vs. a founder's personal feed).

```
We just shipped SPUR — a control tower for the CLI coding agents your team is already running (Claude Code, Codex, Gemini, Kimi, OpenCode) — and the thesis behind it is one we kept hearing in customer interviews: engineering leaders don't know what their teams are spending on AI tooling, and the gap between what finance sees and what's actually burning isn't 20 percent. It's closer to 5x.

One verbatim from the corpus that anchored the whole launch: "A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month." Not an outlier — structural. Every vendor bills separately. No one owns the rollup.

What SPUR does:

- Unified cost ledger across five CLI agents, live, in your terminal
- Durable plans + a review queue so context survives across sessions and seats
- Brain-swap across vendors when one hits a rate limit, without losing state

For Team and Enterprise tiers: RBAC, audit logs, and per-developer spend roll up to whoever signs the bill. We do not promise SOC 2 or HIPAA at launch — if either is a hard requirement, talk to us before you pilot.

Single signed Rust binary. BYO-key. No vendor lock-in.

curl -sSL https://getspur.dev/install.sh | sh

If you're a VP Eng or EM thinking about how to govern agent spend before it becomes a line item that surprises finance — what would you want to see before greenlighting a pilot?
```

---

## Visibility setting — `1st & 2nd connections` vs. `Public`

Set to **Public**. Reasoning:

LinkedIn's algorithm in 2026 still weights initial dwell-time-per-impression more than raw connection-graph proximity, and a launch post is the one moment where reshares from outside your immediate graph compound — a single EM 2 degrees away who reshares is worth more impressions than five 1st-degree likes. "1st & 2nd connections only" exists for safer, more candid posts (interviewing, hiring frustrations, controversial takes); a product launch is the opposite use case — you want the post to be re-shareable by anyone who finds it useful, including press, prospects, and lurking competitors. The downside of Public — strangers commenting in bad faith — is low at launch hour and easy to moderate. Public also makes the post citable in follow-up posts, email signatures, and the org's `vs/devin` and `vs/cursor` pages without auth-gated link previews. Default to Public; revisit if a specific follow-up post benefits from a smaller circle.

---

## Recommendation & meta-notes

**(a) Lead with: Draft A.** Reason: LinkedIn's B2B feed rewards first-person essays over corporate-voice announcements right now (the "we just shipped" opener is associative-fatigued — the algorithm sees it on every Series A launch). Draft A's hook ("A coworker on Hacker News a few weeks back…") opens with a quote, not a product, and the cost-opacity narrative is the sharpest emotional language in the corpus per `marketing/messaging/positioning.md:64-68`. Draft B should be posted from the SPUR company page 24–48 hours later as the "official" launch artifact — that way the company page benefits from the personal post's reshare velocity rather than competing with it.

**(b) Most-likely Team-Lead comment-objection:** *"How is this different from just exporting usage from each vendor's dashboard into a spreadsheet?"* Prepared response (post as a reply, not edited into the body):

> Fair question. Two reasons the spreadsheet doesn't hold: (1) Anthropic/OpenAI/Google bill on different cadences and different cost units (input vs. output vs. cache-read tokens vs. compute-seconds), so a flat spreadsheet conflates things that aren't comparable — SPUR normalizes per-vendor cost into a single per-task ledger entry; (2) the gap that matters is between *what a developer thinks they're spending* and *what's actually on the bill at the end of the month*. A spreadsheet built monthly catches that 30 days late. SPUR shows it live, while the agent is still running. Happy to walk through the extractors if useful.

That answer also tees up a follow-up DM, which is the conversion path on LinkedIn (DMs > link clicks for this audience).

**(c) Demo link — pin a comment, don't put it in the post body.** Reason: LinkedIn deprioritizes posts with outbound links in the body (well-documented through 2025–2026), but a *pinned first comment* with the demo link gets full distribution and the engagement signal of a "creator engaging in their own comments" boosts the parent post. Format the pinned comment as: *"For the curious — 90-second demo of the live cost ledger here: [link]. Happy to answer specifics in the thread."* The `curl`-pipe install staying in the body is fine — it's not a clickable outbound link, so the algorithm doesn't penalize it, and it doubles as proof that SPUR ships as a binary, not a SaaS sign-up funnel.
