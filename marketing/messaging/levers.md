# SPUR Psychological Levers — V1

*2026-05-20. Phase-2 deliverable. Maps cognitive levers from `marketing/marketingskills/skills/marketing-psychology/SKILL.md` onto the value-prop matrix in `marketing/messaging/positioning.md`. Every recommendation cites (a) a verbatim VOC quote or competitor signal as evidence the lever is in-context, and (b) the specific P1 value prop it amplifies. The "NOT to pull" lists are load-bearing — developer audiences are hostile to obvious dark patterns, and the wrong lever erodes the brand-voice posture defined in `marketing/product-marketing.md:162-165` faster than it converts.*

---

## Framing constraints

Two non-negotiables shape every recommendation below:

1. **Brand-voice prohibitions** from `marketing/product-marketing.md:141-147`: no "revolutionary", no "next-gen", no "synergy", no superlative claims. Any lever that would require "AI-powered" framing to land is rejected on sight.
2. **Audience posture.** SPUR's three personas are all senior or staff-level developers (Orchestrator) or the EMs who manage them (Team Lead) or the same Orchestrator on the move (Mobile Operator). This is a population that *writes its own scripts to reverse-engineer vendor billing* (buremba, `voc.md:50-52`). Any lever that depends on the user not noticing the mechanism is a lever that backfires within a week of launch.

Combined effect: SPUR's persuasion budget is best spent on **specificity, asymmetric honesty, and self-aware understatement** — not on urgency, scarcity, or mimetic-desire mechanics that are standard in B2C and SMB SaaS.

---

## Persona 1 — The Orchestrator (Sr/Staff eng, tmux-native)

### Levers to pull

#### Lever 1A — Loss aversion on accumulating worktree-coordination tax

- **Principle:** Loss Aversion / Prospect Theory (`SKILL.md:262-266`). Losses feel ~2× as painful as equivalent gains.
- **VOC evidence:** *"Yes, worktrees with workmux. I expected this to become less necessary over time as models got faster, but the opposite has happened."* — nojs, HN 47573483 (`voc.md:118-120`). This is loss-framing already in the audience's own words: a workflow they invested in is **getting worse over time**, not better. The Orchestrator is already experiencing the loss — SPUR doesn't have to manufacture it; SPUR has to name it.
- **Value prop amplified:** Worktree-per-worker + automatic DAG-ordered cherry-pick + review-gated merge (`positioning.md:31`, `themes.md:63`). The lever lets us say "the tax you're paying is compounding — here's where it stops" without resorting to fear-of-missing-out.
- **Copy example (hero subhead, complement to Hero C):**
  > Worktrees got you to five parallel agents. They won't get you to ten — the coordination tax compounds faster than the models do. SPUR collapses the post-worktree merge tax into a review queue.

#### Lever 1B — IKEA Effect + Status-Quo Bias (defused, not fought)

- **Principle:** IKEA Effect (`SKILL.md:146-149`) — people value what they've built — combined with Status-Quo Bias (`SKILL.md:162-164`) handled per `SKILL.md`'s explicit recommendation: *reduce friction to switch; make the transition feel safe and easy*.
- **VOC / competitor evidence:** The DIY synthesis in `themes.md:88` is unambiguous — *"the DIY users are the most-qualified leads, not the most-resistant"*. Beefin (`voc.md:84-94`) is the canonical example: an engineer who built his own coordination layer (Amux) **and still reaches for "control tower" unprompted**. The right move is to honor the work, not displace it.
- **Value prop amplified:** "Bring your own agent" + "any ACP-speaking agent" (`product-marketing.md:185`); + Hero C's "you already built half of this in tmux — let us finish the other half" (`positioning.md:81`).
- **Copy example (onboarding email, day 1 after install):**
  > Subject: SPUR's first job is to not break your tmux session.
  >
  > You've already built scaffolding around Claude Code and Codex. The first time `spur init` runs, it reads your existing worktrees and your installed agents — it doesn't replace either. If after fifteen minutes you decide SPUR isn't pulling its weight, `cargo uninstall spur-cli` leaves no daemons, no config residue, and no orphaned worktrees. Try the smallest plan first: one brain, one worker, one review.

### Levers to NOT pull

#### Anti-lever 1A — Manufactured scarcity / urgency on installs

- **Principle being rejected:** Scarcity / Urgency Heuristic (`SKILL.md:247-250`). The skill itself flags this: *"Only use when genuine."*
- **Why it backfires here:** SPUR is a tool people will live in for **years**, on the same machine as a `cargo`-installed binary. Any "only 200 founding installs" or "lifetime price ends Friday" framing reads as the exact opposite of the "production-hardened, not world-changing" voice (`product-marketing.md:163`). The Orchestrator persona is the most dark-pattern-allergic segment in the funnel; one whiff of synthetic urgency on the homepage forfeits their first impression.
- **What to do instead:** Anchor scarcity on **the durable artefact** — e.g. "lifetime license at $290 mirrors the `personal_lifetime` plan key already shipped in the license crate" (cite `product-marketing.md:18`). Real, verifiable, no countdown clock.

#### Anti-lever 1B — Mimetic desire / FOMO ("join 10,000 devs using SPUR")

- **Principle being rejected:** Mimetic Desire + Bandwagon Effect (`SKILL.md:131-133, 211-214`).
- **Why it backfires here:** (a) Pre-launch, the number is zero or low-three-digits — using it invites mockery. (b) The Orchestrator persona explicitly resents bandwagon framing; the brand-voice avoid list (`product-marketing.md:141-147`) was built against exactly this register. (c) Bandwagon claims contradict the "self-aware about being early-stage" tone (`product-marketing.md:163`). The Orchestrator wants to be the **earliest** sophisticated user, not part of a crowd.
- **What to do instead:** Authority via *peer*, not crowd. Quote Beefin verbatim ("I needed a control tower" — `voc.md:93`) and the Amux author attribution. One named senior-engineer voice from the audience's own watering hole (HN) outperforms any crowd-count claim for this persona.

---

## Persona 2 — The Team Lead (EM over 3–10 devs)

### Levers to pull

#### Lever 2A — Loss aversion on undiscovered cost discrepancy (the "5× ratio" anchor)

- **Principle:** Loss Aversion (`SKILL.md:262-266`) + Anchoring (`SKILL.md:267-270`). The combination is the lever — neither alone is as sharp.
- **VOC evidence:** *"A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month."* — roxolotl, HN 44598254 (`voc.md:54-56`). And: *"I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"* — buremba (`voc.md:50-52`). The same ~5× discrepancy appears twice in the corpus, independently — that's the anchor. The Team Lead's loss frame is not "we're spending too much" but "we don't know what we're spending, and the gap between perception and reality is **5×**."
- **Value prop amplified:** Five live cost extractors + DuckDB analytics engine reading vendor JSONL/SQLite in place (`positioning.md:41`, `product-marketing.md:170-174`). This is also the *one differentiator no peer can match by design* (`marketing/competitors/_summary-indirect.md:44`) — so the lever amplifies the strongest underlying moat in the deck.
- **Copy example (pricing-page subhead, above the Team tier):**
  > Two engineers in the same VOC corpus described agent spend that was ~5× what their finance team thought it was. SPUR Team shows you the actual number, per developer, per repo, this week. $49 / seat / month is what you spend to stop discovering the gap by accident.

#### Lever 2B — Pratfall Effect on explicit anti-claims (the non-obvious lever)

- **Principle:** Pratfall Effect (`SKILL.md:191-194`). *Competent people become more likable when they show a small flaw.* Admitting a weakness can **increase** trust and differentiation.
- **VOC / competitor evidence:** The Team Lead's environment is saturated with AI-vendor overpromise — Devin's "autonomous engineer in Slack" positioning at $73M ARR (`marketing/competitors/_summary-indirect.md:65`) sets the maximalist baseline. The buying posture is *defensive*. The audience reflex by mid-2026 is "what is this tool **not** going to do that the demo implies?" SPUR already has a fully-formed anti-claim list in `positioning.md:33, 43, 53` — those anti-claims are currently treated as defensive footnotes. Reframed, they are the **trust accelerator** for this persona.
- **Value prop amplified:** Indirectly amplifies every value prop, because pre-emptive honesty about scope buys credibility for the claims that *do* land — most pointedly the unified cost ledger (because the EM's natural rebuttal is "what's the catch?").
- **Copy example (pricing-page footer, Team tier):**
  > What Team **doesn't** do: it doesn't enforce a per-developer budget cap (it shows spend; it doesn't gate it). It doesn't include SOC 2 or HIPAA at launch — those are Enterprise. It doesn't replace your existing observability stack. If your EM mandate is "stop spend over $X/dev/week automatically", SPUR Team will not satisfy that mandate today, and we'd rather you find out now.

### Levers to NOT pull

#### Anti-lever 2A — Authority bias via unearned logos / "trusted by" walls

- **Principle being rejected:** Authority Bias (`SKILL.md:232-235`).
- **Why it backfires here:** Pre-launch, SPUR has no real enterprise references (`product-marketing.md:176` calls out the launch-blocker). Faking, padding, or vaguely implying "trusted by [Stripe/Shopify/Linear]" — including via "as seen on HN" tropes that overstate a single comment — is the single fastest way to lose Team Lead trust in this segment. EMs *check*. They click through. They Slack their network. A single unearned logo on the pricing page erases every honest claim on every other page.
- **What to do instead:** Hold the logo wall until 5+ named-user quotes land per the `product-marketing.md:176` plan. Replace with the Pratfall lever (2B) in the interim. Self-aware emptiness ("no customer logos yet — here's the GitHub commit log and the open-source license") outperforms fake fullness for this persona.

#### Anti-lever 2B — Default effect / dark-pattern pricing pre-selection

- **Principle being rejected:** Default Effect (`SKILL.md:166-169`) when applied at the **purchase** step rather than the *configuration* step.
- **Why it backfires here:** Pre-selecting the Team tier on the pricing page, auto-checking annual-billing radio buttons, or hiding the Community tier below the fold are mechanics that EMs detect within seconds and screenshot to engineering Slack channels. Devin and other cloud-agent peers have already trained this audience to be hostile to procurement friction theater. Worse: SPUR's *entire wedge against Devin* is "we keep humans in the loop on purpose" (`positioning.md:96`) — using dark-pattern defaults at the checkout step undermines the load-bearing trust differentiator.
- **What to do instead:** The Default Effect is fine, even good, at the *product onboarding* layer — pre-selecting sensible config in `spur init`, defaulting review-gate to strict, defaulting to one worker before asking about parallelism. Apply choice architecture to *reduce setup friction*, never to *bias billing*.

---

## Persona 3 — The Mobile Operator (dev away from desk)

### Levers to pull

#### Lever 3A — Hyperbolic discounting / present bias on the "train ride" frame

- **Principle:** Hyperbolic Discounting / Present Bias (`SKILL.md:156-159`). *Emphasize immediate benefits over future ones.*
- **VOC evidence:** *"I've been using this to be productive all day on my phone."* — Beefin, HN 47104424 (`voc.md:136-138`). This is a present-tense pull quote, not a future-tense promise. The Mobile Operator wants *today's* commute to be productive, not "ROI in 6 months." The lever lets us pitch concrete and immediate, in the voice the audience already uses.
- **Value prop amplified:** Telegram bot sharing the same review lane and event bus as the TUI — *same state machine, not a parallel surface* (`positioning.md:51`, `product-marketing.md:80`).
- **Copy example (onboarding email, day 3, sent only to users who linked Telegram):**
  > Subject: The diff your laptop ran while you were on the train is waiting.
  >
  > You linked Telegram on Monday. Tomorrow morning, dispatch one plan before you close the laptop. By the time you hit the second stop, you'll have a review card on your phone with the diff, the cost, and three buttons. Approve from the platform, and the cherry-pick lands on the staging branch before you reach the office. That's the whole loop.

#### Lever 3B — Zeigarnik Effect on review-queue depth (carefully scoped)

- **Principle:** Zeigarnik Effect (`SKILL.md:186-189`). *Unfinished tasks occupy the mind more than completed ones. Open loops create tension.*
- **VOC evidence:** *"Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos."* — Beefin (`voc.md:88-90`). The Mobile Operator's open loops are already a source of cognitive tension — SPUR's lever is to **close them**, not to manufacture new ones. The Zeigarnik lever here is *defensive*: surface the open loop, then make closing it trivially fast.
- **Value prop amplified:** Review gate as first-class state machine with timeout/retry/merge gating (`positioning.md:52`, `product-marketing.md:77`).
- **Copy example (Telegram push notification copy):**
  > Worker #3 (Codex) finished. 2 diffs pending review · 41 LoC · est. cost $0.23.
  > [ Approve ] [ Reject ] [ Open in TUI ]
  >
  > *No countdown. No "auto-merges in 10 min." The notification surfaces an open loop and provides the close-it action. That's the Zeigarnik lever in its ethical form for this audience.*

### Levers to NOT pull

#### Anti-lever 3A — Scarcity / artificial deadline on mobile review actions

- **Principle being rejected:** Scarcity / Urgency (`SKILL.md:247-250`) applied to the mobile-review surface.
- **Why it backfires here:** Any "Approve in next 10 min or auto-merge" pattern is the **exact pattern Devin's autonomous-engineer positioning embodies** — and SPUR's positioning against Devin is *"SPUR keeps the human in the loop on purpose"* (`positioning.md:96`, `product-marketing.md:97`). Putting a countdown on mobile review actions destroys the differentiator at the moment of highest user attention. The anti-claim in `positioning.md:53` — *"we do not promise to authorize destructive merges from mobile without a TUI confirmation"* — must be load-bearing in the actual UX, not just in the positioning doc.
- **What to do instead:** Let the Zeigarnik lever (3B) do the work. Open loops create their own pull — adding a deadline on top is both gratuitous and brand-destroying.

#### Anti-lever 3B — Gamification, streaks, or "you're on a 7-day reviewing roll!"

- **Principle being rejected:** Goal-Gradient Effect (`SKILL.md:176-179`) and related gamification mechanics, when applied to professional review work.
- **Why it backfires here:** The Mobile Operator persona is a senior engineer reviewing production-bound diffs on a phone. Gamification framing implies the work is a game; the audience reads it as condescension. Brand voice (`product-marketing.md:163-165`) explicitly forbids cuteness — *"Rigorous. Pragmatic. Terminal-native. Developer-respectful. Self-aware."* leaves no room for a streak counter.
- **What to do instead:** Goal-Gradient is fine for *plan execution progress* — "3 of 5 workers complete, 2 pending review" is information, not gamification. Reserve it for the underlying plan state, not for the human reviewer's behavior.

---

## Lever risk register

Each recommended lever above, with 3–5 ways it could specifically backfire on a developer audience, and the mitigation that keeps it safe. Dev audiences detect dark patterns faster than any other segment SPUR sells to — pre-registering the failure modes is how we keep the levers in their ethical window.

| Lever | Backfire mode | Why it stings for devs | Mitigation |
|---|---|---|---|
| **1A** Loss aversion on coordination tax | Reads as fearmongering ("you're already losing!") to a happy tmux user | Power users own their workflow as an identity marker; attacking it triggers defensive backlash | Frame the loss as *external and accelerating* (model speed), not internal to the user's skill (`themes.md:88`) |
| **1A** | Conflates SPUR's claim with a measurable promise that doesn't hold | Devs will benchmark — if a 5-worker plan takes more wall-clock with SPUR than without, the loss-framing becomes the prosecution's opening statement | Couple every loss-framing claim to a falsifiable proof (event-replay durability, cherry-pick DAG order) the user can verify in 10 minutes |
| **1A** | Crosses into vendor-bashing of Claude / Codex / Cursor | "Peers, not competitors" is the explicit stance (`_summary-indirect.md:43`); attacking Claude alienates the user's primary tool | Frame the tax as *interaction between tools*, not as a flaw in any one tool |
| **1B** IKEA / status-quo accommodation | "Keep your tmux" reads as feature-incomplete ("you should have replaced tmux") | Ambitious devs may want a maximalist tool, not a humble adjunct | Pair the humility frame with an opinionated proof of capability (the cost ledger, the cherry-pick DAG) so the humility reads as *taste*, not as *thinness* |
| **1B** | "We don't replace tmux" gets quoted back as "doesn't do anything tmux can't" | Hostile HN comments will compress this into a one-liner dismissal | Always co-deploy the IKEA frame with a specific capability tmux cannot do (durable plan, structured review, cross-vendor cost ledger) |
| **2A** Cost-discrepancy loss aversion (5× anchor) | The 5× number is two anecdotal HN quotes, not a measured population statistic | Devs will reverse-engineer the citation. If they discover it's two HN comments dressed as a finding, every cost claim on the site loses credibility | Cite the quotes verbatim with author + thread (already done in `voc.md`), and use the framing *"two engineers in the same corpus described…"* — accurate, not extrapolated |
| **2A** | "We show you what you're spending" gets compared to vendor invoice and is off by 10% | The Riskiest Claim in `positioning.md:143` already names this risk — extractor lag vs. real invoice | Ship the accuracy disclosure inline on the cost view (not buried in docs) and over-deliver on the disclosure before the user catches the gap |
| **2A** | EMs interpret "$1k/week" as a vendor-bashing claim against Anthropic specifically | Marketing the buremba quote against Claude's brand directly invites a takedown request and alienates Anthropic DevRel co-marketing path | Quote verbatim with full attribution; frame the lever as *cross-vendor*, not *anti-Claude* — the ledger spans all five vendors |
| **2B** Pratfall on anti-claims | Anti-claim list grows so long the page reads as "this tool doesn't do anything" | The Pratfall Effect is dose-dependent; one or two limitations strengthen trust, ten weaken the value claim | Cap anti-claims at 2 per persona (positioning.md already does this); deploy on pricing page and competitive page, not on the hero |
| **2B** | Competitors quote our anti-claims back at us ("even SPUR admits…") | Honest framing weaponized against us in side-by-side comparison decks | The frame survives because the anti-claims are *deliberate scope choices*, not bugs — pair each with the explicit "if you need X, look elsewhere" so the constraint reads as taste |
| **2B** | Anti-claims read as covering for a launch-quality bug ("they don't enforce caps because they can't") | Skeptical EMs may assume the limitation is technical rather than principled | Show the architectural reason inline — "we surface spend; gating it well requires per-org policy that's an Enterprise concern" |
| **3A** Present-bias on "train ride" frame | Reads as overpromising ("productive all day on my phone" was Beefin's quote, not our test) | Devs who try Telegram review for two commutes and find it clunky will publicly call the framing a lie | Cap the present-bias claim to *one review action per notification*; don't promise full plan authorship from mobile |
| **3A** | Phone-native framing implies an iOS/Android app exists | The anti-claim in `positioning.md:53` is explicit: no native app, mobile is Telegram only | Always co-deploy with the channel name ("on Telegram"), never with generic "on your phone" |
| **3A** | The 24/7 productivity frame contradicts the "calm discipline" brand voice | Senior devs are increasingly burnout-aware; "be productive all day on your phone" reads as toxic always-on hustle | Use the frame for *unblocking review*, not for *dispatching new work* — preserve the off-hours / on-hours boundary in the copy |
| **3B** Zeigarnik on review queue depth | Push notifications become noisy and devs disable them within 48 hours | Once notifications are off, the entire mobile lever stack is dead | Hard cap notification frequency in the product (one push per worker completion, batched) and let the user configure quiet hours by default |
| **3B** | Open-loop framing crosses into FOMO ("3 unreviewed diffs are waiting!") | The line between Zeigarnik and FOMO is thin; cross it and the brand-voice prohibition triggers | Notification copy stays informational (counts, sizes, costs) — never emotional adjectives ("urgent", "waiting on you") |
| **3B** | Senior engineers resent any tool that creates *more* mental open loops, even briefly | Audience is already saturated with notification fatigue | Ship the *close-the-loop action* inside the notification surface (inline approve buttons), not as a "tap to open the app" pattern |

---

## Summary — recommendations for the caller

### (a) The single highest-leverage non-obvious lever

**Lever 2B — Pratfall Effect on explicit anti-claims for the Team Lead persona.**

Three converging reasons it's the highest-leverage move:

1. **The anti-claims already exist** (`positioning.md:33, 43, 53`) — they're currently treated as defensive footnotes inside a positioning doc, when they could be the *load-bearing trust mechanic on the pricing page and competitive landing pages*. Zero new content cost; pure reframing.
2. **The market context maximally rewards it.** Devin's $73M ARR autonomous-engineer pitch (`_summary-indirect.md:65`) has trained EMs into a defensive posture by mid-2026. Pre-emptive honesty cuts through that defense in a way no positive claim can — and the more saturated the market gets with AI overpromise, the sharper the Pratfall lever cuts.
3. **It compounds the cost-ledger lever (2A).** The 2A claim ("$49/seat to stop discovering 5× cost gaps") is *more* believable to a skeptical EM when the same page also says "we surface spend; we don't gate it." Honest about the limit → trusted on the capability. Lever 2A without 2B reads like every other AI-cost dashboard pitch; 2A *with* 2B reads as the first AI dev tool that an EM doesn't have to mentally discount.

Concrete next action: rewrite the pricing-page Team-tier section to lead with the Pratfall anti-claim block, then the cost-discrepancy framing, then the price. Reverse the conventional order (price → features → caveats); SPUR's audience converts on the reverse.

### (b) Lever I almost recommended but rejected, and why

**Scarcity on Pro early-access ("100 founding seats at $290 lifetime — ends Friday").**

The temptation: launch-time scarcity is genuine (`personal_lifetime` plan key already exists in the license crate — `product-marketing.md:18`), the unit economics work, and "founding member" framing has a track record of converting power users on competing dev tools.

Why I rejected it after writing it out:

1. **Voice mismatch.** The brand voice (`product-marketing.md:163`) is *"production-hardened, not world-changing… self-aware about being early-stage."* A countdown-clock-style "founding seats end Friday" lifts the wrong half of that voice and abandons the *production-hardened* register that the rest of the positioning is built on.
2. **Audience mismatch.** The Orchestrator persona — the population that converts to Pro — *is the same population that mocks "founding member" framing on HN every week.* The scarcity lever would convert a smaller cohort but burn the brand's first impression with the larger cohort.
3. **Second-order risk** (`SKILL.md:80-83`). Even if the lever converts well on day one, it trains the audience to wait for the next "founding" cohort, the next anniversary discount, the next pricing event. SPUR is a tool people will live in for years — pricing should look like infrastructure pricing (calm, durable, boring), not consumer-launch theater.

A version that survives the audit: **Anchor the $290 lifetime on the durable artefact** (the `personal_lifetime` plan key that ships in the license crate today — verifiable in the repo), not on a synthetic deadline. The price is the same; the framing is "this is what the code already supports" rather than "buy now or lose access." Real, falsifiable, no countdown.

### (c) Action items for Phase-3 copywriting

Five concrete tasks, ordered by leverage:

1. **Rewrite the pricing-page Team-tier section using Lever 2B as the lead.** Anti-claim block first, then 2A (cost-discrepancy anchor), then price. Use the copy example from Lever 2B verbatim as the starting draft.
2. **Hero A subhead audit against Lever 2A's accuracy disclosure requirement.** The current Hero A draft (`positioning.md:64`) says *"See what you'd be billed today, across every agent, in one number."* Pair this with an inline accuracy band ("within 4 hours of vendor invoice across 5 extractors") on the hero itself, not buried in docs. Without the disclosure, Lever 2A is the Riskiest Claim per `positioning.md:143`; with it, it becomes the strongest hero on the site.
3. **Write the day-1 onboarding email using the Lever 1B copy example as the seed.** Test against a control with no email — the Lever 1B framing should outperform on day-7 retention because it lowers status-quo-bias activation cost.
4. **Design the Telegram notification copy template using Lever 3B's example** (counts, sizes, costs, three buttons; zero emotional adjectives; zero countdowns). Ship the quiet-hours default and one-push-per-completion cap in the product *at the same time* as the notification copy lands in marketing — the lever depends on the product behavior.
5. **Audit every existing or planned page against the Anti-lever 2A trap.** Until the `product-marketing.md:176` launch-blocker on 3–5 named-user quotes is cleared, no page may ship a logo wall, a "trusted by" strip, or a "join thousands of developers" claim. Hold the empty space; the Pratfall lever fills it more credibly than fake fullness.
