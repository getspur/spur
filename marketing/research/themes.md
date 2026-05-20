# Top Pain Themes — SPUR

*Synthesis of `voc.md`. 2026-05-20. Frequency counts = number of distinct verbatim quotes in this batch that map to the theme; not population-level prevalence. Treat as a directional signal pending the Reddit/X/GitHub sweeps still queued in `voc.md` TODOs.*

---

## 1. "I'm paying — and locked out" (rate-limit ambush)

**Frequency in this batch:** 8 quotes
**Why it stings:** the lockout arrives *during paid use*, often well before the user's mental model of "fair use" — and the reset clock is days, not minutes.

Evocative phrases pulled verbatim:

- "Paying $200 a month, I hit my weekly in 3 days last week." — esperent
- "You're locked out of the service … while still paying your subscription … It's ridiculous." — TheOtherHobbes
- "Five minutes later I reached my limit and the engine performed poorer than before." — pragmatick

**SPUR resonance:** maps to the brain-swap / multi-vendor failover promise. The marketing claim worth testing on this audience is *"don't wait out the window — keep working on Codex/Gemini while the Claude clock resets, then come back."*

---

## 2. "I have no idea what I'm actually spending" (cost opacity)

**Frequency in this batch:** 5 quotes
**Why it stings:** the cap-versus-API gap is enormous and invisible. Users only discover it by writing their own scripts against the API or comparing notes with a coworker.

Evocative phrases:

- "I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!" — buremba
- "A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month." — roxolotl
- "Extremely infuriating because if I could have a view into how close I was to being rate limited." — gorbypark

**SPUR resonance:** direct hit for the unified cost ledger / status-bar live spend. Strongest single landing-page claim candidate: *"see what you'd be billed today, across every agent, in one number."*

---

## 3. "Switching tabs costs me more than switching tools" (multi-agent juggling)

**Frequency in this batch:** 7 quotes
**Why it stings:** people *already* run 2-10 agents in parallel. The pain isn't dispatch — it's keeping track of which one is waiting, which is looping, which is done. This is the load-bearing JTBD for SPUR's "one review surface."

Evocative phrases:

- "Context switching is painful, I will lose myself in ten minutes." — sukit
- "I run 5-10 Claude Code agents at a time across different repos. Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos." — Beefin
- "I needed a control tower." — Beefin

**SPUR resonance:** the orchestrator's review-lane + status grid. Note: "control tower" is the strongest in-the-wild metaphor we have — consider lifting it directly into copy.

---

## 4. "Worktrees are the workaround, and the workaround is getting worse" (parallel-agent collision tax)

**Frequency in this batch:** 5 quotes
**Why it stings:** worktrees are the de-facto answer, but they push human-coordination overhead onto the developer — atomic tasks, scope-policing, manual merges. As models get faster, parallel work increases, and the DIY tax compounds.

Evocative phrases:

- "Yes, worktrees with workmux. I expected this to become less necessary over time as models got faster, but the opposite has happened." — nojs
- "I do use worktrees occasionally … but I rarely have them work on truly-different tasks simultaneously." — kevinsync
- "I keep everything on the main branch and just make sure to keep tasks pretty separate in scope." — sprobertson

**SPUR resonance:** worktree-per-worker is table stakes; the differentiator is *automatic* DAG-ordered cherry-pick + review-gated merge. Don't sell "we use worktrees" (everyone does) — sell "we collapse the post-worktree merge tax."

---

## 5. "Limits keep moving, so I keep moving" (vendor-switching fatigue)

**Frequency in this batch:** 6 quotes
**Why it stings:** the dominant coping mechanism right now is *churn to another vendor's CLI*. Each switch resets context, breaks muscle memory, and re-establishes a new pricing surprise.

Evocative phrases:

- "Codex, it's much more generous. And doesn't lock you into using their CLI." — loveparade
- "I moved to GLM-5.1 with their coding plan. It's better than both Opus and Sonnet." — brokegrammer
- "I run Claude Sonnet 4.6 via GitHub Copilot and it seems very reasonable to me there." — cableshaft

**SPUR resonance:** the *meta* observation here is that users are *already* polyamorous about agents — they just lack a place to live where the polyamory is cheap. Reposition: SPUR isn't a Claude Code replacement; it's the layer that makes vendor-switching free.

---

## Synthesis: surprising findings

1. **"Control tower" is a folk-term that already exists.** A power user (Beefin, Amux author) reached for that exact phrase unprompted. SPUR's marketing has been calling this "orchestrator" — "control tower" tests better as a metaphor on a cold prospect.

2. **The opacity quotes are sharper than the rate-limit quotes.** The strongest emotional language in the batch is about *not knowing what you're spending*, not about *being throttled* — even though throttling has more total quotes. This is a reordering signal: the cost ledger may deserve top billing in the hero section over the rate-limit-failover story.

3. **The DIY users are the most-qualified leads, not the most-resistant.** People already running `workmux + worktrees + Claude + Codex side-by-side` are explicitly telling us "this is getting worse, not better" (nojs). The habit is not a moat against SPUR — it's a wedge *for* SPUR. The marketing should target "you already built half of this in tmux — let us finish the other half" rather than "stop using tmux."
