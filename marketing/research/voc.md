# Voice of Customer — SPUR

*Collected: 2026-05-20. Sources: public Hacker News threads on Claude Code rate limits, parallel agents, and multi-agent orchestration. All quotes extracted from indexed comment pages via web fetch.*

## Methodology & fidelity caveat

Quotes were retrieved by fetching public Hacker News comment pages and asking a summarizer model to return verbatim text plus the commenter's HN username. Two known limitations:

1. The summarizer occasionally trims surrounding sentence context. The wording inside the quote marks should be exact, but reviewers MUST spot-check at the source URL before any quote is used in marketing copy, a deck, or a landing page.
2. Role inference is heuristic from comment voice + adjacent posts in the thread; treat it as a hint, not a fact. Anything stronger (employer, seniority) requires the reviewer to click through to the commenter's HN profile.

Reddit / X / GitHub-issue sweeps planned for this batch hit either zero indexed results (`site:reddit.com r/ClaudeCode …`) or HTTP 429 from the fetch tool. Those are filed as TODOs at the bottom — not as silent gaps.

---

## Push — pain that drives them away from status quo

### Theme: Claude Code rate limits derail mid-flow

> "Paying $200 a month, I hit my weekly in 3 days last week."
> — esperent · HN item 47626833 · "Ask HN: What are you moving on to now that Claude Code is so rate limited?" · 2025
> Role: paying Max user, heavy daily driver.

> "I couldn't even get it to do simple tasks for me this week on the max plan. It feels like they're randomly rate limiting users."
> — adamtaylor_13 · HN item 44598254 · "Anthropic tightens usage limits for Claude Code without telling users" · 2025
> Role: Max subscriber, daily user.

> "Five minutes later I reached my limit and the engine performed poorer than before."
> — pragmatick · HN item 44598254 · 2025
> Role: new / occasional Claude Code user.

> "If I do hit the limit, that's it for the entire week — a long time to be without."
> — Wowfunhappy · HN item 44713757 · "Claude Code weekly rate limits" · 2025
> Role: subscriber commenting on weekly cap.

> "I was hitting Claude Code's rate limit pretty often while paying for their max subscription."
> — chrisischris · HN item 45879351 · 2025
> Role: Max subscriber.

> "Claude usage limit reached. Your limit will reset at 3pm. /upgrade to increase your usage limit."
> — geeksinthewoods quoting the CLI · HN item 45879351 · 2025
> Role: paying user, posting the literal error string.

> "You're locked out of the service … while still paying your subscription … It's ridiculous."
> — TheOtherHobbes · HN item 44713757 · 2025
> Role: paid subscriber.

### Theme: Cost is opaque even to people writing $200 / mo checks

> "I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"
> — buremba · HN item 44598254 · 2025
> Role: Max subscriber who reverse-engineered actual API-equivalent spend.

> "A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month."
> — roxolotl · HN item 44598254 · 2025
> Role: observer / colleague of a power user, likely IC engineer.

> "Extremely infuriating because if I could have a view into how close I was to being rate limited."
> — gorbypark · HN item 44713757 · 2025
> Role: developer, asking for a live usage gauge.

> "What's the cost between never using Claude, and using it and getting these lower limits?"
> — stavros · HN item 44598254 · 2025
> Role: cost-sensitive subscriber weighing churn.

> "OpenAI's 'PRO' subscription is really a waste of money IMHO for this and other reasons."
> — canada_dry · HN item 44713757 · 2025
> Role: comparison shopper across vendors.

### Theme: Context switching across multiple agents wrecks human flow

> "Context switching is painful, I will lose myself in ten minutes."
> — sukit · HN item 47573483 · "Ask HN: Is it actually possible to run multiple coding sessions in parallel?" · 2026
> Role: developer attempting parallel sessions.

> "Code can be logically separated, but my mind struggles to do the same."
> — sukit · HN item 47573483 · 2026
> Role: same OP, restating the cognitive ceiling.

> "I can only keep 3 threads like this going at once. Sometimes it's only 1 or 2, depending on complexity."
> — dontwannahearit · HN item 47573483 · 2026
> Role: experienced multi-agent operator.

> "I was spending more time switching between terminal tabs than actually building things."
> — Beefin (Amux author) · HN item 47104424 · "Show HN: Amux" · 2026
> Role: builder describing his own pre-tool pain.

> "I run 5-10 Claude Code agents at a time across different repos. Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos."
> — Beefin · HN item 47104424 · 2026
> Role: power user / tool author.

> "I needed a control tower."
> — Beefin · HN item 47104424 · 2026
> Role: same.

> "I've wanted exactly this for coordinating headless agent sessions across repos."
> — tumf · HN item 47104424 · 2026
> Role: prospective user, headless-agent operator.

### Theme: Parallel agents collide / merge pain

> "Yes if you operate with worktrees, it's actually possible to operate up to 5-10 — at least I've succeeded with that multiple times."
> — rox_kd · HN item 47573483 · 2026
> Role: advanced worktree user.

> "I think what's important is, that you keep atomical small tasks and increments, and whenever possible merge things."
> — rox_kd · HN item 47573483 · 2026
> Role: same; describing the DIY discipline this requires.

> "Instead I keep everything on the main branch and just make sure to keep tasks pretty separate in scope."
> — sprobertson · HN item 47573483 · 2026
> Role: developer who explicitly rejects worktrees because of overhead.

> "I do use worktrees occasionally … and run Claude and Codex side by side, but I rarely have them work on truly-different tasks simultaneously."
> — kevinsync · HN item 47573483 · 2026
> Role: multi-vendor user, ducks parallel work because coordination is hard.

> "Yes, worktrees with workmux. I expected this to become less necessary over time as models got faster, but the opposite has happened."
> — nojs · HN item 47573483 · 2026
> Role: long-time worktree user; explicit "this problem is getting worse."

---

## Pull — what they're moving toward

### Theme: A "control tower" / one surface for many agents

> "Running parallel agents is the obvious next step after single-agent works, but the coordination + cost tracking becomes the blocker."
> — AlexCalderAI · HN item 47104424 · 2026
> Role: practitioner commenting on tooling gap.

> "When you spin up 5–10 agents, you can immediately see which one is burning tokens or looping."
> — Beefin · HN item 47104424 · 2026
> Role: tool author articulating the pull.

> "I've been using this to be productive all day on my phone."
> — Beefin · HN item 47104424 · 2026
> Role: tool author — directly evidences mobile-orchestrator persona.

### Theme: Vendor-switching as escape valve

> "I run Claude Sonnet 4.6 via GitHub Copilot and it seems very reasonable to me there."
> — cableshaft · HN item 47626833 · 2025
> Role: switcher from Claude Code to Copilot.

> "Codex, it's much more generous. And doesn't lock you into using their CLI."
> — loveparade · HN item 47626833 · 2025
> Role: switcher; calls out CLI lock-in by name.

> "I moved to GLM-5.1 with their coding plan. It's better than both Opus and Sonnet."
> — brokegrammer · HN item 47626833 · 2025
> Role: switcher to alt-vendor.

> "synthetic.new with Kimi K2.5 works surprisingly well."
> — _lvbh · HN item 47626833 · 2025
> Role: multi-tool stacker (also uses Copilot Pro).

> "I prefer the Gemini CLI, I paid Google AI Pro for the year and it is perfect for me."
> — elC0mpa · HN item 47626833 · 2025
> Role: switcher to Gemini.

> "Turn off the 1M context that got enabled by default. Long sessions eat through tokens much faster."
> — MeetingsBrowser · HN item 47626833 · 2025
> Role: subscriber describing a hidden cost knob.

---

## Anxiety — what worries them about the switch / about doing nothing

> "If you aren't hitting the limits you aren't writing great prompts."
> — ChadMoran · HN item 44598254 · 2025
> Role: power-user posture — limits as a status symbol; useful as a counter-quote.

> "I also wouldn't consider my usage extreme. I never use more than one instance."
> — closewith · HN item 44713757 · 2025
> Role: single-instance user who *still* gets capped — anxiety that "even normal usage" is unsafe.

> "This is how I feel about the 100 msg/wk limit on o3 for the ChatGPT Plus plan."
> — el_benhameen · HN item 44713757 · 2025
> Role: ChatGPT user; evidence the anxiety generalizes across vendors.

> "Could probably have reduced context with a /clear after every file but then I would have to participate."
> — blitzar · HN item 44713757 · 2025
> Role: developer; resents the human-side overhead of staying under the cap.

---

## Habit — what keeps them stuck in DIY tmux land

> "Yes, worktrees with workmux."
> — nojs · HN item 47573483 · 2026 (also in Push above)
> *Note: appears in both buckets — habit is "workmux," push is "but it's getting worse."*

> "Instead I keep everything on the main branch and just make sure to keep tasks pretty separate in scope."
> — sprobertson · HN item 47573483 · 2026
> Role: habit pattern that resists worktree adoption.

> "I do use worktrees occasionally … and run Claude and Codex side by side."
> — kevinsync · HN item 47573483 · 2026
> Role: already running two vendors in parallel, no tool.

---

## TODOs — searches that returned nothing or got blocked

These are open gaps. A human follow-up should retry them from a logged-in browser or a different IP.

- [ ] `site:reddit.com r/ClaudeAI weekly limit usage frustrating` — 0 results. Retry directly on reddit.com search; try `r/ClaudeCode`, `r/ClaudeAI`, sort by Top / Past month.
- [ ] `site:reddit.com r/ClaudeCode "lost context" OR "session" closed terminal` — 0 results. Retry with broader terms: `claude code lost work terminal closed`, `claude code session resume`.
- [ ] `"Claude Code Max" weekly limit reddit complaint` — 0 results. Try `claude max weekly cap reddit`, then sort r/ClaudeAI by New filtered to "Discussion".
- [ ] HN item 47137470 (Clash — worktree conflict detection): page returned without comments. Refetch later for collision-pain quotes.
- [ ] HN item 44178216 (Run multiple Claude Code agents in parallel using Git worktrees): HTTP 429. Refetch in a few hours.
- [ ] HN item 46307563 (Maestro — run AI coding agents autonomously for days): HTTP 429. Likely highest-yield for the "overnight runs / closed laptop" pain — retry as priority.
- [ ] HN item 46680124 (Plural — explore multiple approaches with Claude Code simultaneously): HTTP 429. Retry.
- [ ] HN item 48126438 (New Claude Code programmatic usage restrictions): HTTP 429. Newest thread — retry.
- [ ] X / Twitter sweep for `"claude code" "rate limit" 5 hour`: not attempted (no X access in this session). Recommended next step.
- [ ] GitHub issue sweep on `anthropics/claude-code` and `openai/codex` for words like "lost context", "resume", "worktree", "merge conflict": not attempted; use `gh search issues`.
