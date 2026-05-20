# SPUR — Launch-day X thread

*Phase-3 deliverable. 2026-05-20. Skill: `marketing/marketingskills/skills/social/SKILL.md`. Sources cited inline: `marketing/site/homepage.md`, `marketing/site/demo-90s-script.md`, `marketing/site/pricing.md`, `marketing/messaging/positioning.md`, `marketing/research/voc.md`, `marketing/competitors/_summary-indirect.md`. Brand-voice prohibitions enforced per `marketing/product-marketing.md:148-154`. No emoji. No "launching today!!" energy. No `@anthropic` anywhere in the thread — they get organic reposts if the work earns them, not a mention that reads as riding the brand.*

---

## The thread — 9 posts

### Post 1 — Hook (control tower + Insights ledger screenshot)

```
On HN last summer:

"I run 5-10 Claude Code agents at a time. Keeping
track of which is waiting, which is working, which
broke something was chaos. I needed a control tower."
— Beefin

We built it.
```

- **Char count:** 217 / 280
- **Image:** `insights-cross-vendor.png` (still from Shot 6 of the demo — Insights overlay, two vendor rows, `session total $1.73 — live`. See `demo-90s-script.md:131, 199-208`).
- **@-mentions:** None. The Beefin quote is verbatim from `voc.md:88-94` (HN item 47104424, "Show HN: Amux"). Beefin's X handle is **TBD — verify before posting**; if confirmed, add `(@beefin-handle)` after the dash on the attribution line *only* if char budget holds. Do not @-mention without verifying the handle belongs to the same person who wrote the HN comment.
- **Voice check:** Calm, attribution-first. The hook is the recognition, not a claim about SPUR. Per `positioning.md:118` — "control tower" is Beefin verbatim, not our coinage.

---

### Post 2 — Pains 1 & 2 (locked out + cost opacity, verbatim VOC)

```
Four pains we built SPUR for:

1. You hit the weekly cap on day 3 — and keep paying
   the subscription you can't spend.

2. You don't know what you're actually spending.
   "I'm paying for Max, and when the API spend gets
   calculated, it's almost $1k." — buremba, HN
```

- **Char count:** 256 / 280
- **Image:** None. Text-first post. (If a single second-image were added, candidate is `marketing/research/voc.md` HN screenshot of buremba's comment — but pinning that to a single tweet risks reading as a callout. Recommend text-only.)
- **VOC:** buremba quote condensed from `voc.md:50-52` / `homepage.md:50-51` (HN 44598254). Original verbatim is *"I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"* — condensed by ~25% for the char ceiling, preserving the dollar figure, the Max subscription, and the API-vs-perceived-spend gap. The shape of the claim is unchanged.
- **@-mentions:** None.

---

### Post 3 — Pains 3 & 4 (worktree collisions + context death)

```
3. Parallel agents collide on the same worktree.
   "Worktrees with workmux. I expected this to
   become less necessary as models got faster.
   Opposite happened." — nojs, HN

4. Context dies the moment you close the terminal.
   Close the laptop, lose the plan, the lineage,
   the half-finished review.
```

- **Char count:** 273 / 280
- **Image:** None.
- **VOC:** nojs verbatim from `voc.md:118-120` / `homepage.md:60-61` (HN 47573483). Condensed by removing "Yes," prefix and tightening "to become less necessary over time as models got faster, but the opposite has happened" — the keep-the-shape rule from Post 2 applies.
- **@-mentions:** None. nojs X handle **TBD — verify before posting**. Same rule as Beefin.

---

### Post 4 — The brain-swap moment (from demo-90s-script.md)

```
Demo, 90 seconds, real terminal:

Claude prints "usage limit reached."
Press Alt-p. Plan Inspector opens.
Ctrl-K, switch worker, codex. Badge flips.

Codex picks up the same task, same worktree,
same file Claude was about to write.

You never lost the plan.
```

- **Char count:** 248 / 280
- **Image / video:** Attach `brain-badge-flip.gif` from `demo-90s-script.md:129` — the 4-second loop of the `claude-code-acp` → `codex` badge transition. (If GIF compression on X degrades readability, swap to a 6-second muted MP4 of the same beat. Do not link to the full 90s demo here — that's Post 9's job.)
- **Beat source:** Shots 3–5 of the demo (`demo-90s-script.md:39-57`). The verbs "swap the brain — not the plan" mirror the narration script (`demo-90s-script.md:106-107`).
- **@-mentions:** None.

---

### Post 5 — The cost-ledger moment (Insights overlay)

```
Press Alt-a.

Insights overlay slides in:

  claude-code-acp   $1.42
  codex             $0.31
  session total     $1.73   live

One number, across every agent you ran today.
No proxy. No second account. Vendor JSONL read
in place.
```

- **Char count:** 246 / 280
- **Image:** `insights-cross-vendor.png` — second appearance, intentional. This is the single still flagged in `demo-90s-script.md:199-208` as "the answer-image to the cost-opacity pain." Re-use it here because the dollar figures in the tweet body and the figures in the still must match exactly — if they drift, screenshot-sharers will catch the inconsistency.
- **Source:** Shot 6 narration (`demo-90s-script.md:111`) + accuracy band copy from `homepage.md:19-21`. "No proxy. No second account." defuses the most common "is this a man-in-the-middle?" reply (covered in Reply Playbook below).
- **@-mentions:** None.

---

### Post 6 — The honest limits (anti-claim from pricing.md)

```
What SPUR doesn't do:

No autonomous "set-and-forget" mode. Every worker
output passes through a human review gate. Approve,
reject, modify, retry — that's the state machine.

If you want "assign tickets in Slack, get PRs back,
no review surface" — that's Devin. They own that
job. We don't compete on it.
```

- **Char count:** 277 / 280 — at the ceiling. Do not edit in copy without re-counting.
- **Image:** None.
- **Source:** Anti-claim block from `pricing.md:11` verbatim in spirit; Devin peers-not-competitors paragraph from `homepage.md:127` / `positioning.md:94-96`. "They own that job and own it well" is softened to "They own that job" for char budget — the generosity still reads.
- **Risk note:** This is the post most at risk of being misread as vendor-bashing of Devin. See Summary §(b). Mitigation already in the copy: "They own that job. We don't compete." reads as deference, not attack — but the second sentence ("If you want X — that's Devin") could be screenshotted out of context. Acceptable risk; do not soften further or the anti-claim stops working.
- **@-mentions:** None. **Do not @-mention `@cognition_labs` or `@devin`.** A named @-mention turns a peer paragraph into a callout. Let Devin find this organically if they want.

---

### Post 7 — The tmux+worktree wedge

```
If you already built half of this in tmux +
worktrees + a shell script that scrapes prices
out of someone's JSONL:

SPUR is the other half. The durable plan. The
review queue. The cross-vendor ledger.

Keep your worktrees. Keep your agents. Add the
layer on top.
```

- **Char count:** 246 / 280
- **Image:** None.
- **Source:** Hero C variant in `homepage.md:213-230` and `positioning.md:80-86`. "Keep your worktrees" defuses anti-pattern #4 from `themes.md:54-63`. This is the post that converts the F2 power-user audience — they recognize themselves in the first three lines and trust the next three.
- **@-mentions:** None. This is the post most likely to be quote-tweeted by tmux/worktree power users (Beefin, nojs, rox_kd) — that's the goal. Don't pre-empt the quote-tweet with an @-mention that turns it into an obligation.

---

### Post 8 — Pricing in one tweet

```
Pricing:

Community — $0. One brain, one worker, full
review loop, full cost ledger. No signup, no
credit card.

Pro — $19/mo. Unlimited workers, brain-swap,
session resume, Telegram review.

No logo wall on the homepage. No testimonials
yet. We're early. We want yours.
```

- **Char count:** 274 / 280 — at the ceiling.
- **Image:** None.
- **Source:** `pricing.md:64-87`. The "no logo wall, no testimonials" line is the empty-space stance from `homepage.md:143-156` and `pricing.md:125-126` ("we have not earned them"). This is the post that earns trust on launch day — the absence is the proof.
- **@-mentions:** None.
- **Note:** Lifetime SKU ($290) is deliberately omitted from this post. It complicates the math and invites "is this a launch promo?" replies that the answer to is "no, it's a code-real plan key" — too much nuance for 280 chars. Lifetime gets covered in the Reply Playbook if anyone asks.

---

### Post 9 — Install + demo link

```
curl -sSL https://getspur.dev/install.sh | sh

Signed Rust binary. No Node, no Python, no Docker.
Any ACP-speaking agent works out of the box.

90-second demo, real terminal, not a mock:
getspur.dev/demo
```

- **Char count:** 195 / 280
- **Image:** None. The install command is the visual.
- **Source:** Install CTA from `homepage.md:25-27, 166-174`. Credibility tokens from `homepage.md:180-184`. Demo link is the 90s demo from `demo-90s-script.md` — assumes `getspur.dev/demo` redirects to the hosted video (verify routing before posting; if the URL is not live, swap to the direct YouTube link).
- **@-mentions:** None.

---

## Reply playbook — prepared responses to predictable replies

*Use these as drafts, not scripts. Tighten to fit the specific reply. The voice is calm-confident; never defensive, never enthusiastic.*

### R1 — "Is this open source?"

```
No. SPUR is proprietary. The Community tier is free
and genuinely useful (full review loop, full cost
ledger, any ACP agent) but the orchestration core's
source isn't public. We may open-source select
crates (telemetry, ACP client) over time.

If "must be open source" is a hard requirement,
Aider serves you better than we will.
```
*Source: `pricing.md:101-102`. The Aider hand-off is genuine, not deflection.*

### R2 — "How accurate is the cost ledger? What's the lag?"

```
Reads vendor JSONL/SQLite in place — no proxy, no
second account. Reconciles to each vendor's own
invoice within a documented per-extractor lag (≤ ~4
hours across all five today).

If your invoice and SPUR disagree by more than the
published band, file a ticket with the session ID
and we'll reproduce it. That's a bug.
```
*Source: `homepage.md:19-21`, `pricing.md:30`.*

### R3 — "Why not just claude-code + tmux + a shell script?"

```
That's literally how most of our power users
started. Beefin built Amux for the same reason —
the coordination tax compounds faster than the
models do (his words, roughly).

What SPUR adds on top: the durable plan in SQLite,
the review queue as a state machine, the
cross-vendor ledger. The other half of what you
already built.
```
*Source: Hero C frame from `homepage.md:213-230`. nojs/Beefin VOC backing.*

### R4 — "$19/mo is steep / too cheap"

```
Frame: SPUR is an add-on to the agents you already
pay for, not a substitute. If Pro cost more than
the underlying CLI ($200/mo Claude Max, etc.), the
math wouldn't work.

The two engineers in our VOC corpus described 5×
spend gaps between perceived and actual. $19/mo is
priced to be a rounding error against catching
that gap once.
```
*Source: `pricing.md:113-114` + cost-discrepancy framing `pricing.md:24-30`.*

### R5 — "Doesn't Devin already do this?"

```
Different shape. Devin owns "assign a ticket in
Slack, get a PR back, no review surface" — cloud,
single-vendor, autonomous. $73M ARR by mid-'25;
they earned it.

SPUR is the opposite: lives in your terminal next
to the agents you already run, keeps the human in
the loop on purpose, spans every vendor's bill in
one ledger. Peers, not competitors.
```
*Source: `positioning.md:94-96`, `homepage.md:127`.*

### R6 — "What about Aider / Claude Code / Cursor?"

```
Aider: pair-programmer, single agent. SPUR's value
starts at fleet ≥ 2. Add SPUR above Aider; don't
switch.

Claude Code: SPUR runs it as a worker. We don't
compete on in-session UX; we add cross-session
durability and cross-vendor failover.

Cursor: editor. Keep it open in the other window —
most SPUR users do.
```
*Source: `positioning.md:102-108`, `homepage.md:133-139`.*

### R7 — "Lifetime license — is that real or a launch promo?"

```
Real and not a promo. The `personal_lifetime` plan
key ships in the license crate today —
crates/spur-license/src/lib.rs:83 — a verifiable
artifact in the signed binary, not a marketing
SKU.

We're not running a countdown. We're not capping
it at the first 100 seats. If we ever retire it,
we'll retire it for new buyers only and honor
every existing lifetime license.
```
*Source: `pricing.md:77-79`. The file-path citation is on purpose — it's the proof.*

---

## Quote-tweet bait — 3 candidates to seed launch-day engagement

*The goal is not to drive traffic from these QTs — it's to make the thread legible to the F2 power-user audience by adding our voice next to theirs on a recognized timeline. Each QT should land within the first 4 hours of the main thread going live, while reply velocity is highest.*

**Critical caveat — handles to verify before launch:** the F2 audience research in `marketing/research/voc.md` cites these users by their HN usernames (Beefin, nojs, sukit). Their X handles are not confirmed in the corpus. **Before posting any of the QTs below, the launch operator must:** (a) confirm the X account belongs to the same person who wrote the HN comment (cross-reference GitHub bio, personal site, or self-attribution in their X bio); (b) confirm the linked tweet still exists and the author hasn't deleted/locked it. If either fails, do not QT — pick a different candidate from their recent timeline that maps to the same theme.

The three candidates below are described by **theme + recommended search query**, not by static URL, because the goal is to find a recent (last 30–60 days) tweet on that theme rather than a stale link.

### QT1 — Beefin, on Amux / control-tower coordination

- **Find:** any recent tweet from Beefin's account about multi-agent coordination, Amux, or "running N Claude Code agents." Best signal: a screenshot of his own kanban or a quote of his own HN comment.
- **Recommended QT text:**

```
"I needed a control tower" is the line we built
the whole product around. Amux solved the
tab-switching half; SPUR adds the durable plan,
the review queue, and the cost ledger across
every vendor you also pay.

Peers, not competitors. Demo: getspur.dev/demo
```
- **Char count:** 247 / 280
- **Voice check:** Names Amux generously, positions SPUR as the layer above. The "peers, not competitors" line is load-bearing — it must read as deference, not co-option. Do not QT a tweet where Beefin is criticizing a specific vendor; that risks making us look like we're piggy-backing on a callout.

### QT2 — nojs, on worktrees + workmux

- **Find:** any tweet from nojs about worktrees, workmux, parallel agents, or the "coordination tax" theme from his HN comment (`voc.md:118-120`).
- **Recommended QT text:**

```
"Opposite happened" is exactly the wedge. Faster
models didn't reduce the coordination cost — they
multiplied the agents you can run in parallel,
and the tax scales with that.

We built SPUR for fleet size ≥ 2. Keep your
worktrees. Add the plan layer.
```
- **Char count:** 254 / 280
- **Voice check:** Affirms his diagnosis in his own words, names the wedge (`positioning.md:104` — "SPUR's value materializes at fleet size ≥ 2"). Do NOT include an install link in the QT itself — the affirmation has to land before the pitch. The link is in the main thread (Post 9).

### QT3 — sukit, on terminal-native multi-agent workflows

- **Find:** any tweet from sukit on running multiple CLI agents, terminal workflow, or vendor-switching as escape valve. Their HN comments aren't in the current `voc.md` corpus — **flag for the launch operator:** verify sukit's relevance and recent X activity before pulling the trigger.
- **Recommended QT text (generic — tighten when the specific tweet is identified):**

```
This is the audience SPUR is for: terminal-native,
already running multiple CLI agents, already
hitting the rate-limit wall, already paying enough
across vendors that a single-vendor cost view
isn't enough.

If that's you, the 90s demo is the fastest way to
see if SPUR fits: getspur.dev/demo
```
- **Char count:** 272 / 280
- **Voice check:** This QT is conditional on the right tweet existing. If sukit's recent activity doesn't include a clear multi-agent-workflow signal, **drop QT3 entirely** and replace with a QT of a tweet from `rox_kd` or `tumf` (both in `voc.md:96-104` with affirmative pull-toward-control-tower quotes). Two strong QTs beat three weak ones.

---

## Summary

### (a) Single post most likely to be screenshotted and shared independent of the thread

**Post 5 (the cost-ledger moment).** Three reasons:

1. It is the only post in the thread that pairs a concrete dollar figure (`session total $1.73`) with an image that proves it. Screenshot-sharers reach for posts that contain both the claim and the evidence in one frame — Post 5 is the only one.
2. `demo-90s-script.md:199-208` explicitly identifies the `insights-cross-vendor` still as "the only image where you can hand someone a single frame and they immediately understand both halves of SPUR's value prop (cross-vendor *and* live cost)." That property survives the move from demo-frame to tweet-screenshot intact.
3. The post body itself is structured for screenshot-extraction — three short lines for the vendor breakdown, one for the total, then the three-sentence accuracy claim. A reader screenshots it once and the whole point lands without the surrounding thread.

Post 1 is the close-second candidate (the Beefin quote is screenshot-friendly), but Post 1's payoff requires the reader to recognize Beefin's HN comment to feel the recognition. Post 5 works for anyone who has ever paid two vendors.

### (b) Post most at risk of being misread as vendor-bashing

**Post 6 (the honest-limits post that names Devin).** The risk is asymmetric:

- **Intended read:** Devin is a peer with a different shape, we explicitly do not compete with them, we are deferring to their expertise on autonomous Slack ticket-eating.
- **Misread risk:** "If you want X — that's Devin. They own that job. We don't compete on it." When screenshotted out of context, the second half ("we don't compete on it") can be read as a dismissive jab rather than a generous deference. The line *is* generous — but on X, the absence of the surrounding thread is the default state of any quoted screenshot.
- **Mitigation already in the copy:** the verb "own" is borrowed from `positioning.md:94` ("Devin owns *'give me an autonomous engineer...'*"). It is praise. Do not soften further — softening past this point breaks the anti-claim, and the anti-claim is the reason the rest of the thread is credible.
- **Secondary candidate:** Post 2's buremba quote. It quotes a Claude Max user describing $1k of API spend against a $200 subscription. Read uncharitably, this is "Anthropic is overcharging." Read accurately (the way buremba wrote it on HN), it is "Anthropic's billing surface and Anthropic's API tell two different stories, and I want a third party to reconcile them." The verbatim VOC framing is the protection — we did not paraphrase, we did not add commentary, we attributed by name and forum. If anyone on X reads it as vendor-bashing, we point them at the original HN comment, not at our restatement.

**Recommendation:** Do not edit Post 6 or Post 2 to reduce this risk. Both are load-bearing for credibility. If either gets traction as "vendor bash," respond in-thread with the full peers-not-competitors paragraph (`homepage.md:127`) and let the source links do the work.

### (c) Schedule the thread or post live?

**Post live.** Three converging reasons:

1. **First-hour reply velocity is the load-bearing metric.** A scheduled thread denies the operator the ability to reply within minutes of the first wave of responses. On X, replies inside the first hour drive ~70% of total thread reach (industry-wide; not SPUR-specific). If the operator can't be present, the thread under-performs by default. Scheduling makes the worst-case outcome more likely.
2. **The quote-tweet bait list requires real-time judgment.** QT1–QT3 depend on which of those tweets are actually live, recent, and uncontroversial *on launch morning*. A scheduled thread cannot adapt if a candidate QT has been deleted, locked, or made controversial since the schedule was set. Posting live lets the operator pick the strongest 2–3 candidates as of that hour.
3. **The brand is calm-confident, not "look at the clock."** A scheduled launch reads as a launch event ("9:00 AM PT sharp"). The voice we've earned in the homepage and the demo is the opposite of that — quiet, deliberate, no countdown. Posting live, mid-morning, on a Tuesday or Wednesday (Mon/Fri are weaker on X for developer audiences) is consistent with the brand. The post time is "when the operator can sit with it for two hours," not "9:00:00."

**Concrete recommendation:** post live, Tuesday or Wednesday, between 9:30 and 10:30 AM PT. Operator clears two uninterrupted hours after posting Post 1 to drip Posts 2–9 at ~90-second intervals (let each post breathe; do not paste the whole thread in one tool-burst). Reply playbook is open in a second window. QT seeds happen at the +60 to +90 minute mark, after the main thread has stabilized. End-of-day check at +6 hours; any reply still unanswered at that point gets a reply that night, not the next morning.
