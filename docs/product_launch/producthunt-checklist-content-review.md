# Content-marketer review — Product Hunt checklist

**Source:** Codex worker `content-marketer` · model `gpt-5.6-sol` · effort `xhigh`  
**Delegation:** `f01fcd6f-4017-4425-b46f-22d2a8598f8f` · attempt 1 · status Success  
**Date:** 2026-07-16  
**Verdict:** Conditional — not execution-ready until P0s fixed.

---

## 1. Executive verdict

**Conditional; not execution-ready.** The checklist has the right strategic core—Orchestrator ICP, resilience-first story, Community-first adoption, honest Pro upsell, strong onboarding checks—but its “actionable” status is premature. Several Product Hunt mechanics are stale, the audience plan contains algorithm-gaming language, the launch time conflicts with the playbook, and the linked May submission package contains numerous claims that PRD v2.3 does not support. Assigning a launch date before fixing those P0s risks launching on the wrong PH day with false product, pricing, and platform claims.

## 2. What’s working

- The primary narrative correctly follows PRD v2.3: session survival, worktree isolation, review state, and local-first durability lead; cost is secondary and observational. That matches the PRD’s explicit “[what to market instead](../../SPUR_PRD.md)” decision.

- The checklist understands the Orchestrator. It speaks to agent-tab chaos, rate limits, worktrees, recovery, and review—not generic “AI productivity.”

- Community is presented as a real free product, not a crippled demo. The checklist mostly respects the signed tier truth: Community has one concurrent worker; Pro raises that to ten and adds Telegram and Insights.

- The product-readiness section is strong: clean install, first-session time, real review flow, restart recovery, privacy/security, capacity, and staffed support are appropriate launch blockers.

- The proposed gallery order is materially better than the May package: control tower → review → isolation → recovery → operational depth. That is the correct story hierarchy.

- Self-hunting, asking for feedback rather than votes, rejecting paid hunters, and prohibiting bots/vote rings are all correct. Product Hunt explicitly says there is no discernible advantage to a third-party hunter and encourages makers to hunt themselves. [Product Hunt Launch Guide](https://www.producthunt.com/launch)

- Treating installs and activation as more important than rank is strategically right. “Rank without activation is vanity” is one of the checklist’s strongest lines.

## 3. Critical gaps / risks — P0

### P0.1 — The Product Hunt mechanics are stale

The 500–800-upvote benchmark must be deleted. Product Hunt now ranks launches using **points**, combining upvotes, comments, and other authenticity signals; not all upvotes carry equal weight. Raw upvote thresholds are no longer a defensible planning metric. [Product Hunt points guidance](https://help.producthunt.com/en/articles/10275873-what-are-points)

“Coming Soon” is also obsolete. Product Hunt retired those pages and replaced them with product forum threads where people can follow and receive launch notifications. [Product Hunt changelog](https://www.producthunt.com/changes)

The checklist also says shoutouts are capped at three. Product Hunt’s newer changelog says that cap was removed, while its older launch guide still says three. Treat the live submission UI as authoritative and remove the hardcoded limit.

### P0.2 — The personal-network section reads like vote engineering

“Highest quality votes,” “prefer people with established PH accounts,” and activating 30–50 people steadily are the wrong framing. Product Hunt prohibits mass messaging, coordinated voting, and asking for upvotes. It permits organic sharing and genuine discussion. [Sharing guidance](https://help.producthunt.com/en/articles/2690626-how-do-i-share-my-post), [Community Guidelines](https://help.producthunt.com/en/articles/3615694-community-guidelines)

Delete the account-age instruction entirely. Product Hunt confirms votes have different weights, but it does not publicly validate “experienced accounts weigh more.” Screening supporters for algorithmic value looks manipulative even without an explicit upvote request.

Reframe this cohort as beta users invited to try one workflow and provide honest feedback if they choose—not “quality votes.”

### P0.3 — The linked May package is unsafe to execute

The older [Product Hunt package](../../marketing/launch/product-hunt.md) must be marked **superseded / do not ship** until audited. It contains:

- A cost-ledger hero that conflicts with PRD v2.3’s resilience-first decision.
- “Across every CLI coding agent,” “actual number,” and exact four-hour billing-lag claims not supported by v2.3.
- A nonexistent `spur cost --today` command.
- Incorrect keybindings such as Space-to-collapse and `r`-to-retry from the lineage tree.
- “No auto-merge, ever,” conflicting with Pro auto-approve and the actual review state model.
- Automatic cross-vendor failover claims not grounded in the PRD’s manual-failover baseline.
- AI-generated terminal mockups instead of screenshots of the real product.
- Unsupported pricing: `$19/mo`, `$290 lifetime`, `$49/seat`.
- Unsupported Team claims: shared ledger, audit log, basic RBAC.
- Unsupported WSL and roadmap-order claims.
- `cargo uninstall` language despite the public installation path being `curl | sh`.
- An open-source explanation that invents roadmap intent and disparages hypothetical forks.

Cross-linking this package as “asset production detail” creates direct trust debt with the exact technical audience SPUR wants.

### P0.4 — The launch time conflict can put SPUR on the wrong day

The checklist recommends 12:01 AM Pacific. The playbook schedules PH for **06:00 UTC**, which in July is 11:00 PM PDT—the previous Product Hunt day. The older package correctly notes 12:01 AM PDT is 07:01 UTC, then still introduces a misleading 06:00 UTC buffer.

There must be one canonical launch timestamp generated after the date is chosen:

- PDT: 12:01 AM PT = 07:01 UTC.
- PST: 12:01 AM PT = 08:01 UTC.

Do not maintain separate hardcoded UTC times in multiple documents. Product Hunt treats 12:01 AM Pacific as a rule of thumb, not a requirement; choose a different time only if team coverage or coordinated press makes it better. [Official preparation guide](https://www.producthunt.com/launch/preparing-for-launch)

### P0.5 — The business goals are not real targets yet

“Track raw + unique,” “first brain session + review card,” and “soft replies asking about Pro” are definitions, not targets. The 400–1,000 PH-sourced email target is ungrounded and conflates:

- A speculative pre-launch supporter heuristic.
- Email signups acquired after launch.
- Ranking performance.

Set actual denominators and thresholds:

- PH landing visits.
- Install starts and completed downloads.
- Install → `spur init`.
- `spur init` → first brain session.
- First session → first completed review.
- Community → explicit Pro-interest event.
- T+7 retained use, if measurable.

There is also an unresolved privacy problem: the checklist assumes remote activation telemetry, while local-first positioning may imply limited or opt-in data collection. The owner must define exactly what is collected, with consent and privacy disclosure, before activation can be a launch KPI.

### P0.6 — Product Hunt eligibility and pricing-field truth need explicit checks

Product Hunt’s 2026 featuring rules prioritize live products that people can use immediately; waitlist-only launches are generally not eligible for homepage featuring. [Featuring Guidelines](https://help.producthunt.com/en/articles/9883485-product-hunt-featuring-guidelines), [unreleased-product guidance](https://help.producthunt.com/en/articles/484932-can-i-submit-an-unreleased-product)

Add explicit blockers:

- Community binary is immediately obtainable without a paid-license gate.
- The launch URL leads directly to install and quickstart, not primarily an email form.
- Pricing status is set to **Paid with a free plan**.
- Every gallery image represents a real, reproducible product state.
- Any Pro screenshot is visibly labeled Pro.

The field checklist currently claims a 1:1 mapping but omits Product Hunt’s pricing field.

## 4. Important improvements — P1

- Replace “Featured = must-have” with “Featured = desired platform outcome.” It is editorially controlled, not an internal go/no-go criterion. Non-featured launches remain in the All feed. [Product Hunt All-feed guidance](https://help.producthunt.com/en/articles/484926-why-is-my-post-not-on-the-homepage)

- Update the featuring criteria to the current official language: **Useful, Novel, High Craft, Creative**. “Interesting” and “well-made” are older paraphrases.

- Remove or label as internal hypotheses: 8–12 weeks, 400 supporters, 30 comments, 10-minute response correlation, 4:00 AM ranking shift, 10–20% email click rate, early-vote weighting, and weekday vote ranges.

- Remove “editing fields may reset momentum.” There is no first-party support for that algorithm claim. Keep a content freeze as an operational safeguard, not a ranking hack.

- Reduce the 24-hour staffing theater. Staff the first several hours and the US business day well; Product Hunt itself recommends choosing a launch time when the team can genuinely participate.

- Use one launch email, not two T+0 pushes. A same-afternoon non-opener blast adds spam pressure without improving product learning.

- Make the landing destination source-identifiable without a query string—such as a stable `/product-hunt` path—or use referrer attribution. Product Hunt correctly rejects UTM parameters in the submitted product URL.

- Replace “deeper analytics” with the product name **Insights**, while noting internally that it is Pro, build-feature-dependent, and still maturing.

- Test the selected headline with actual Orchestrators before locking it. The Hero A/B/C framework was built for homepage A/B testing; PH needs one clear answer.

- Add the current relaunch rule: generally six months between launches for the same product/root domain and a significant product change—not merely a new UI or pricing. [Relaunch guidance](https://help.producthunt.com/en/articles/484934-can-i-relaunch-my-product)

## 5. Nice-to-haves — P2

- Use the new product forum thread for one technical pre-launch discussion, not a hype countdown.
- Build an interactive demo only if it faithfully represents the TUI; a good 60–90 second real capture is sufficient.
- Add captions and a text transcript to the demo.
- Prepare personalized response outlines rather than copy-paste macros.
- Include one real “restart and resume” proof in the video; static screenshots cannot prove the strongest differentiator.
- Conduct a live-submit-UI audit 72 hours before launch because Product Hunt’s documentation currently conflicts on description length and shoutout limits.

## 6. Message / positioning audit

| Option | Verdict | Reason |
|---|---|---|
| **A — Issue in, PR out — across every agent.** | Keep as conversion copy | Punchy and PRD-derived, but “every agent” is broader than the supported/ACP-compatible reality. Better in the gallery or CTA than as the category-defining tagline. |
| **B — One brain, many workers, zero lost context.** | Strong brand line | Ownable and emotionally relevant, but does not tell a cold PH visitor what SPUR is. |
| **C — Control tower for CLI coding agents.** | **Recommended PH tagline** | Clearest category + ICP fit, immediately understandable, and contains no tier or capability overclaim. |
| **D — Parallel agents. One review gate. Local-first.** | Secondary proof line | Strong triad, but “parallel” is awkward when Community is capped at one concurrent worker. |
| **E — One live cost ledger across every CLI coding agent.** | Delete | Conflicts with v2.3 message priority and overclaims coverage, freshness, and maturity. |

The character counts in the table also need QA. The listed numbers are inaccurate, although all five remain below 60.

The description is directionally good and currently 249 characters, safely under the stricter 260-character help-center limit. Product Hunt’s own pages conflict between 260 and 500 characters; use **260 as the production cap** and verify the live UI. [Launch guide](https://www.producthunt.com/launch/preparing-for-launch), [posting help](https://help.producthunt.com/en/articles/479557-how-to-post-a-product)

The first-comment template fits PH culture: human opener, clear ICP, concrete product behavior, explicit non-fit, and a request for feedback. Its weaknesses:

- The personal origin story must be literally true, not a synthesized persona anecdote.
- Replace “staff+” with “senior/staff engineers and tech leads.”
- State Community’s one concurrent worker and Pro’s ten-worker ceiling.
- Replace “not fully autonomous merge without you” with the safer “not a coding-agent replacement or set-and-forget autonomy.”
- Lead with the problem and user outcome; “Rust-native” is credibility support, not the lead.

## 7. Research findings audit

| Claim | Assessment | Required change |
|---|---|---|
| Self-hunt is fine | Solid | Keep; make it the default. |
| Celebrity hunter is unnecessary | Solid | Keep; retire the vanity hunter shortlist. |
| 12:01 AM PT gives a full PH day | Solid rule of thumb | Keep, but prioritize team coverage. |
| Schedule within 30 days | Solid | Keep. |
| First maker comment matters; 70% historical figure | Solid but correlational | Keep without implying causation. |
| Tagline 60, square thumbnail, two gallery images, 1270×760 recommendation | Solid | Keep. |
| Featured vs All distinction | Solid | Keep; remove the unsupported mobile statement. |
| 500–800 upvotes for Top 5 | Wrong for current points system | Delete. |
| 400+ supporters strongly improve ranking odds | Overstated third-party heuristic | Remove as a requirement; treat engaged beta users as product readiness. |
| Coming Soon page | Outdated | Replace with product forum thread/follow flow. |
| Three shoutouts maximum | Likely outdated; official sources conflict | Verify live UI; do not hardcode. |
| Experienced accounts weigh more | Unpublished inference presented as fact | Delete. |
| 4:00 AM ranking inflection | Unverified folklore | Delete. |
| Editing fields resets momentum | Unverified folklore | Delete. |
| Replies under ten minutes improve ranking | Unverified | Keep only as an internal service standard, not an algorithm claim. |
| Tue–Thu is best | Reasonable SPUR hypothesis, not PH fact | Base the decision on Orchestrator availability, competition, and staffing. |
| Video helps | Officially, 53% of historical PotD launches used one | Keep optional; do not treat correlation as a requirement. |

## 8. Consistency check: May package and playbook

The documents currently have no usable source-of-truth hierarchy.

| Document | Conflict | Resolution |
|---|---|---|
| [producthunt-launch-checklist.md](../../../../docs/product_launch/producthunt-launch-checklist.md) | Mostly v2.3-correct, but stale PH mechanics | Make this canonical for PH fields, policy, copy, assets, and PH-specific go/no-go. |
| [product-hunt.md](../../marketing/launch/product-hunt.md) | V1.3 cost-ledger lead, fabricated UI details, unsupported pricing/Windows/hunter claims | Mark superseded. Salvage only brand visual direction after a line-by-line product-truth audit. |
| [playbook.md](../../marketing/launch/playbook.md) | Hero A remains primary; PH scheduled for wrong UTC time; duplicates T−7/T−0 ownership | Keep it as the cross-channel operating plan, but import PH copy and launch timestamp from the canonical checklist. |
| [positioning.md](../../marketing/messaging/positioning.md) | Recommends cost ledger as Hero A under old product assumptions | Retain persona language, control-tower category, and words-to-use; supersede its hero recommendation with v2.3. |

Specific ownership split:

- **Checklist owns:** PH eligibility, current fields, tagline, description, first comment, gallery, PH policy, PH day procedures.
- **Playbook owns:** email, X, LinkedIn, HN, support coverage, broader T−14→T+7 sequencing.
- **PRD owns:** capabilities, tiers, quotas, maturity, gaps.
- **Positioning owns:** persona language and category vocabulary—not current product truth.

## 9. Prioritized action list

| Order | Owner | Task and done criterion |
|---:|---|---|
| 1 | Product owner | Produce a v2.3 claim matrix; approve every PH-facing capability, tier, platform, pricing, and roadmap claim. |
| 2 | Launch owner | Mark the May PH package superseded and remove it as an executable asset source until audited. |
| 3 | PH owner | Update platform mechanics: points, product forum, featuring criteria, pricing field, description cap, shoutouts, relaunch rule. |
| 4 | Growth/DevRel | Rewrite audience activation around relevant beta users and honest feedback; delete account-age and “quality vote” language. |
| 5 | Launch commander | Choose the date and publish one canonical PT/UTC timestamp; remove every competing hardcoded time. |
| 6 | Product + privacy owner | Define measurable, privacy-safe funnel events and numerical T+1/T+7 targets. |
| 7 | Product marketer | Lock Option C, the ≤260-character description, and a truthful maker comment with exact Community/Pro boundaries. |
| 8 | Creative/DevRel | Replace generated terminal mockups with real screenshots and a reproducible restart/resume demo; label Pro slides. |
| 9 | Engineering/support | Run a clean Community beta: install → init → first review in under 15 minutes, with one-worker quota visible and understandable. |
| 10 | Founder/launch commander | Perform a live PH draft dry run 72 hours before T−0 and sign the final go/no-go against product, policy, assets, measurement, and staffing. |

## 10. Exact copy patches

**Recommended tagline**

> Control tower for CLI coding agents.

**Description — exactly 260 characters**

> SPUR is a local-first control tower for Claude Code, Codex, Kiro, and Gemini. Workers run in isolated git worktrees; plans resume after restarts; every change hits one review surface. Community is free. Pro adds Telegram review, up to 10 workers, and Insights.

**First-comment core**

> Hi Product Hunt — Vu here, maker of SPUR.
>
> I built SPUR after [insert one true, specific incident]. The problem wasn’t getting an agent to write code; it was keeping multiple agents’ work visible, isolated, and recoverable.
>
> SPUR is a local-first control tower for Claude Code, Codex, Kiro, and Gemini. Workers run in git worktrees, plans and sessions resume after restarts, and every result reaches one review surface: approve, deny, modify, or retry.
>
> It’s for senior/staff engineers and tech leads already juggling CLI coding agents. It isn’t an agent replacement or set-and-forget autonomy.
>
> Community is a free solo daily driver with one concurrent worker. Pro adds up to 10 workers, Telegram review, and Insights.
>
> I’d value blunt feedback on the install-to-first-review path. Where did you get stuck?

**Policy-safe network language**

> Share the launch individually with beta users and peers who already use CLI coding agents. Ask them to try one workflow and offer honest feedback if they choose. Do not ask for votes, screen for account age, or coordinate voting times.