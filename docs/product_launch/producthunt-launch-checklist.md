# SPUR × Product Hunt — Success Launch Checklist

**Status:** Draft for ownership — **not execution-ready** until P0 fixes below are closed (content-marketer review 2026-07-16)  
**Date:** 2026-07-16 · **Rev:** 1.1 (post content-marketer audit)  
**Grounded in:** [`SPUR_PRD.md`](../../SPUR_PRD.md) v2.3 · official [Product Hunt Launch Guide](https://www.producthunt.com/launch) · [Featuring Guidelines](https://help.producthunt.com/en/articles/9883485-product-hunt-featuring-guidelines) · [Points](https://help.producthunt.com/en/articles/10275873-what-are-points)  
**Review:** [`producthunt-checklist-content-review.md`](./producthunt-checklist-content-review.md)  
**Related assets:** [`marketing/launch/playbook.md`](../../marketing/launch/playbook.md) (cross-channel ops) · [`marketing/messaging/positioning.md`](../../marketing/messaging/positioning.md) (persona language)  
**Superseded for PH copy/execution:** [`marketing/launch/product-hunt.md`](../../marketing/launch/product-hunt.md) (2026-05 package — cost-ledger lead, unverified UI claims, unsupported pricing; salvage brand-visual direction only after audit)

### Source-of-truth hierarchy

| Owns | Document |
|---|---|
| Capabilities, tiers, quotas, gaps | `SPUR_PRD.md` |
| PH fields, policy, tagline/description/first comment, gallery, PH day go/no-go | **This checklist** |
| Cross-channel T−14→T+7 (email, X, LinkedIn, HN, support) | `marketing/launch/playbook.md` — must import PH timestamp/copy from here |
| Persona vocabulary / category words | `marketing/messaging/positioning.md` — hero ranking **not** current product truth if it conflicts with PRD v2.3 |

---

## 0. How to use this document

- Checkboxes are the work unit. Assign an **owner** and a **due (T−N)** before T−30.
- **T−0** = launch day. Product Hunt’s homepage day rolls at **12:01 AM Pacific**. Publish **one** canonical PT + UTC pair after the date is chosen (do not hardcode competing times across docs).
  - PDT: 12:01 AM PT = **07:01 UTC**
  - PST: 12:01 AM PT = **08:01 UTC**
  - Playbook’s old **06:00 UTC** PH slot is **wrong** for a midnight-PT launch in summer (that is still the previous PH day).
- “Success” is **not only rank**. Rank helps; **installs, activation, reviews, and durable community** are the business outcomes (see §1).
- Do **not** invent tier claims, open-source promises, or cost-governance language that contradicts the PRD (§2).
- Third-party “research consensus” numbers (upvote totals, 400 supporters, 10‑min reply ranking, etc.) are **hypotheses**, not PH policy — label them as such or omit them from planning targets.

### P0 close-out (from content-marketer review)

- [ ] v2.3 claim matrix approved (capability, tier, platform, pricing, roadmap)
- [ ] May PH package marked superseded in that file’s header
- [ ] PH mechanics updated in this doc (points, product forum, featuring criteria, pricing field) — **done in rev 1.1**; re-verify live submit UI 72h before T−0
- [ ] Audience language free of vote-engineering / account-age screening — **done in rev 1.1**
- [ ] One canonical launch timestamp published after date chosen
- [ ] Privacy-safe funnel events + numerical T+1/T+7 targets defined
- [ ] Tagline C + ≤260 description + maker comment locked
- [ ] Real screenshots + restart/resume demo (not AI mock terminals)
- [ ] Clean Community beta: install → init → first review &lt;15 min
- [ ] Live PH draft dry-run 72h before T−0; founder go/no-go signed

---

## 1. Define success before you launch

Product Hunt official guidance: set goals first; rank is a means, not the only end.

### 1.1 Rank / visibility goals (platform)

| Goal | Target | Notes |
|---|---|---|
| **Featured (homepage)** | Desired, not sole go/no-go | Editorial. Current language prioritizes **Useful, Novel, High Craft, Creative**. Non-Featured posts still appear in the All feed; do not treat “must-have Featured” as product readiness. [Featuring Guidelines](https://help.producthunt.com/en/articles/9883485-product-hunt-featuring-guidelines) · [Why not on homepage](https://help.producthunt.com/en/articles/484926-why-is-my-post-not-on-the-homepage) |
| **Top 5 Product of the Day** | Stretch | Ranking uses **points** (upvotes + comments + authenticity signals) — not a fixed upvote count. [Points](https://help.producthunt.com/en/articles/10275873-what-are-points). Do **not** plan against “500–800 upvotes.” |
| **#1 Product of the Day** | Aspirational | Nice; not required for a successful SPUR launch. |
| **Comment quality** | Maker present; every substantive comment answered during staffed windows | Keep ~10‑minute reply as an **internal service standard**, not an “algorithm boost” claim. |

### 1.2 Business goals (SPUR-specific — PRD-aligned)

Define **numerical** targets before T−7 once telemetry/privacy is decided. Metrics below are the funnel shape — fill “Target” cells with real numbers.

| Metric | Definition | Target (fill) | Notes |
|---|---|---|---|
| **PH landing visits** | Sessions on PH product URL / dedicated `/product-hunt` path | | Prefer stable path for attribution; **no UTM in PH product URL field** |
| **Install starts / completed** | `install.sh` or binary download | | Primary top-of-funnel |
| **Install → `spur init`** | Configured project | | |
| **Init → first brain session** | Session attach/start | | |
| **Session → first completed review** | A/D/M/R on a real worker outcome | | Activation definition of “worked” |
| **Community → Pro-interest** | Explicit event (pricing click, trial, Telegram setup intent) | | Soft comment interest is not enough for KPI |
| **Email list adds (post-launch)** | Signups attributed to PH week | | Do **not** conflate with “400 pre-launch supporters” |
| **Named testimonials** | Consent + name + role | ≥3 by T+14 | |
| **Press / partner pings** | Logged inbound | Log all | Secondary value often exceeds rank |

**Privacy constraint:** Local-first positioning may imply limited/opt-in telemetry. Owner must document exactly what is collected and disclose it on `/privacy` before activation KPIs are launch-critical.

### 1.3 PH eligibility blockers (featuring / launch honesty)

- [ ] Community binary is **immediately usable** (not waitlist-only as the product)
- [ ] Product URL lands on install + quickstart, not primarily an email capture wall
- [ ] Pricing field set correctly (expected: **Paid with a free plan** if Community free + Pro paid — verify live UI)
- [ ] Every gallery frame is a **real, reproducible** product state; Pro surfaces **labeled Pro**
- [ ] No unreleased-only “Coming Soon product” positioning as the shippable SKU

### 1.4 Explicit non-goals

- **Do not** optimize via bots, vote-swapping, mass “please upvote,” or coordinated voting schedules — [Community Guidelines](https://help.producthunt.com/en/articles/3615694-community-guidelines) · [Sharing guidance](https://help.producthunt.com/en/articles/2690626-how-do-i-share-my-post)
- **Do not** screen supporters by PH account age for “vote quality”
- **Do not** promise SOC2/HIPAA, budget enforcement, or “set and forget autonomous coding” (PRD anti-persona + Risk #17)
- **Do not** run Show HN with a public repo if distribution remains proprietary (`marketing/launch/playbook.md` constraint)

**Owner:** ___________ · **Goals signed off by:** ___________ · **Date:** ___________

- [ ] Rank + business goals written with numbers and privacy notes
- [ ] Attribution plan (referrer and/or dedicated path; no UTM on PH product URL)
- [ ] Success review calendar: T+1, T+7, T+30

---

## 2. Product narrative (from `SPUR_PRD.md`)

Use this as the **source of truth** for all PH copy. Prefer grounded capabilities over roadmap.

### 2.1 Positioning (use these, not free-form hype)

| Element | PRD-backed copy |
|---|---|
| **Category** | Control tower / orchestration layer for AI *coding agents* — not a replacement for Claude Code, Codex, Kiro, Gemini. |
| **One-liner (brand)** | *“One brain, many workers, zero lost context.”* |
| **Conversion line (gallery/CTA)** | *“Issue in, PR out — across every agent, in parallel, with one review surface.”* — hedge “every agent” as **ACP-compatible agents you configure**, not every tool on earth. |
| **What to market first** | (1) Session immortality / resume · (2) Worktree isolation + human review gate · (3) Local-first durability · (4) Ops surfaces (Plan/Loop/Explore) as proof of depth |
| **Honest gaps** | Cost = **visibility**, not governance · Peer mailbox experimental · Insights maturing (Pro) · Community concurrent workers **quota = 1** |

### 2.2 Tagline options (PH max **60** characters)

| Option | Tagline | Notes |
|---|---|---|
| **A** | Issue in, PR out — across every agent. | Strong conversion; better as gallery/CTA than sole category tagline (“every agent” broad) |
| **B** | One brain, many workers, zero lost context. | Ownable brand; cold PH visitors may not know what it is |
| **C (recommended for PH)** | Control tower for CLI coding agents. | Clearest category + ICP; no tier overclaim |
| **D** | Parallel agents. One review gate. Local-first. | Awkward with Community **1** concurrent worker — use carefully |
| **E** | ~~One live cost ledger across every CLI coding agent.~~ | **Delete** for PH — conflicts with PRD v2.3 resilience-first priority and overclaims |

- [ ] Final tagline chosen (**default ship: C**) and character-counted in live UI
- [ ] Tagline avoids hype (“most advanced”, “#1 AI”, “game-changing”)
- [ ] Tagline matches featuring spirit (clear usefulness)

### 2.3 Description (production cap **≤260** characters)

Official docs currently conflict on 260 vs 500 — use **260 as production max** and re-check live submit UI.

**Recommended (260 chars exactly):**

> SPUR is a local-first control tower for Claude Code, Codex, Kiro, and Gemini. Workers run in isolated git worktrees; plans resume after restarts; every change hits one review surface. Community is free. Pro adds Telegram review, up to 10 workers, and Insights.

- [ ] Description reviewed against PRD tier map (Community complete daily driver; Pro = Telegram / concurrency / Insights)
- [ ] No open-source claim unless policy changes
- [ ] Install CTA path: `curl … install.sh` (not `cargo install`)

### 2.4 Topics / launch tags (up to 3)

1. **Developer Tools**
2. **Artificial Intelligence** *or* **Productivity**
3. **Open Source** *only if true* — otherwise **Terminal** / **Git** / available alternatives

- [ ] Tags verified in live PH submit UI

### 2.5 Personas in maker comment

1. **Orchestrator** — rate limits, multi-tab agent chaos, session loss  
2. **Team Lead** — review queue / ops visibility (not “Issue Browser is Team-only”)  
3. **Mobile Operator** — Telegram review (**Pro**, labeled)

- [ ] First comment states who it is / isn’t for

---

## 3. Product readiness (go / no-go)

PH traffic is unforgiving. Ship a **loveable** Community path, not a half-setup CLI.

### 3.1 Launch-blocker product checks

| # | Check | Owner | Status |
|---|---|---|---|
| 1 | Cold install: `install.sh` works macOS + Linux; signature/checksums documented | | [ ] |
| 2 | `spur init` → configure ≥1 brain agent → first session in &lt;15 minutes for a motivated user | | [ ] |
| 3 | Demo path: issue/plan → worker in worktree → **review card** A/D/M/R | | [ ] |
| 4 | Session resume / re-attach works after process kill (**show in video**) | | [ ] |
| 5 | Community **1 concurrent worker** quota is visible and understandable in UI/docs | | [ ] |
| 6 | Status page or known-issue note if flaky agents | | [ ] |
| 7 | `/privacy` (and ideally `/security`) live if telemetry or email capture exists | | [ ] |
| 8 | Pricing / Community vs Pro page matches **signed policy** (PRD §4) | | [ ] |
| 9 | PH landing URL stable (`/` or `/product-hunt`); **no bit.ly / UTM on PH product URL field** | | [ ] |
| 10 | Capacity: install + docs can absorb spike | | [ ] |
| 11 | Support channel staffed launch day | | [ ] |
| 12 | v2.3 claim matrix signed for every PH-facing sentence | | [ ] |

### 3.2 Demo script (60–90 seconds)

1. **Pain** (5s): multi-agent tabs / rate limit / lost context  
2. **Control tower** (20s): Dashboard lineage tree + workers  
3. **Review gate** (20s): Approve a real diff  
4. **Durability** (15s): kill/restart → resume (this is the differentiator screenshots cannot prove)  
5. **Close** (10s): Community free install + clear Pro optional  

- [ ] Real capture (not AI-generated terminal mockups)
- [ ] YouTube public or unlisted (**not private**); full URL
- [ ] Captions/subtitles
- [ ] Optional interactive demo only if faithful to the TUI

---

## 4. Pre-launch timeline checklist

### 4.1 T−12 to T−8 weeks — audience & product

Prep time scales with product readiness, not a magic 8–12 week rule. **Engaged beta Orchestrators** matter more than an arbitrary 400-supporter KPI.

- [ ] Waitlist / email list live (grow honestly; no ranking guarantee attached to list size)
- [ ] Product Hunt **product page / forum thread** live so people can follow and get launch notifications (**“Coming Soon” pages are retired** — use current product + discussion flow) [changelog](https://www.producthunt.com/changes)
- [ ] Maker PH profiles complete for **all** makers
- [ ] Organic PH participation: thoughtful comments on related launches (dev tools / agents)
- [ ] Seed private beta: Orchestrator-persona users
- [ ] Confirm install + onboarding metrics from beta

### 4.2 T−6 to T−4 weeks — assets & story

- [ ] Tagline + description finalized (§2) — default tagline **C**
- [ ] First maker comment drafted + peer-reviewed (§5)
- [ ] Gallery: **≥2 required**, recommend **5–8** real screenshots
- [ ] Thumbnail (square; PH recommends **240×240**; &lt;3MB; GIF first frame must read static)
- [ ] Gallery images (recommend **1270×760**)
- [ ] Demo video with **restart → resume** proof
- [ ] Interactive demo optional
- [ ] Promo offer if any (offer + code + expiry all required in PH form)
- [ ] Shoutouts: pick tools that mattered; **do not hardcode max 3** — verify live UI (docs conflict)
- [ ] Website PH-ready: clear CTA, real social proof only, mobile OK

**Suggested gallery sequence for SPUR**

| # | Frame | PRD proof |
|---|---|---|
| 1 | Hero: Dashboard / lineage tree | Control tower |
| 2 | Review card A/D/M/R | Human gate as state machine |
| 3 | Worktree isolation | Isolation; **honest** Community = 1 concurrent worker |
| 4 | Session resume / “close laptop” | Event-sourced durability |
| 5 | Plan Browser / Loop Browser | Ops depth (secondary) |
| 6 | Install one-liner | Time-to-value |
| 7 | Optional: Telegram review | **Label as Pro** |
| 8 | Optional: Cost badge / Insights | Visibility not governance; **Pro** for Insights |

Do **not** use AI mock terminals or invented keybindings (`space` collapse, `r` retry from tree, `spur cost --today`, etc.). Capture the real binary.

### 4.3 T−3 to T−2 weeks — community & hunter decision

**Self-hunt is the default.** PH encourages makers to hunt themselves; third-party hunters are optional amplification, not a success prerequisite. Do not pay hunters.

- [ ] Decision: self-hunt (default) vs optional hunter co-sign
- [ ] If hunter: assets ≥1 week early; niche overlap &gt; vanity followers
- [ ] Warm list: **relevant beta users + peers who use CLI coding agents**
- [ ] Soft ask language only (§6.3) — feedback / try, never upvotes
- [ ] Competitive day scan: avoid major platform keynotes if possible
- [ ] Day-of-week + **one** timestamp locked (team coverage first):

| Goal | Typical choice | Tradeoff |
|---|---|---|
| Orchestrator / business-day attention | **Tue–Thu** | More competition (hypothesis, not PH law) |
| Easier rank, smaller audience | **Weekend** | Lower absolute visits |
| Full PH day window | **12:01 AM PT** rule of thumb | Only if team can staff early hours |

### 4.4 T−7 days — dry run

- [ ] PH draft fully filled; **canonical** schedule set
- [ ] First comment pre-written in submit flow
- [ ] All makers added by PH username
- [ ] Social posts scheduled — link to PH **after** live
- [ ] **One** launch email ready (not dual T+0 spam blasts)
- [ ] FAQ reply outlines (OSS?, vs Cursor?, pricing?, Windows?) — product-truth only
- [ ] Team roles assigned (§7)
- [ ] Install rehearsal from **clean machines**
- [ ] Incident plan if install breaks
- [ ] **72h-before-T−0:** re-audit live PH submit fields (description length, shoutouts, pricing)

### 4.5 T−1 day — freeze

- [ ] No risky deploys to install path or license service
- [ ] Final gallery order pass (mobile + desktop)
- [ ] Confirm canonical PT/UTC pair
- [ ] Makers logged in on two devices
- [ ] Hunter (if any) has live URL ready
- [ ] Calendar blocked for staffed windows (first hours + business day minimum)

---

## 5. Product Hunt submission content checklist

Map to PH submit fields ([official preparation guide](https://www.producthunt.com/launch/preparing-for-launch)). Re-verify live UI.

| Field | Required | SPUR prep | Done |
|---|---|---|---|
| **Product URL** | Yes | `https://getspur.dev` or `/product-hunt`. No short links / UTM. | [ ] |
| **Name** | Yes | `SPUR` only | [ ] |
| **Tagline** | Yes (≤60) | §2.2 option **C** default | [ ] |
| **Links** | Optional | Docs; Discord; GitHub only if public | [ ] |
| **X handle** | Optional | Product handle | [ ] |
| **Description** | Yes (cap ≤260 production) | §2.3 | [ ] |
| **Pricing** | Verify in UI | Free plan + paid Pro (or current honest state) | [ ] |
| **Launch tags** | Up to 3 | §2.4 | [ ] |
| **Thumbnail** | Yes | Square, &lt;3MB | [ ] |
| **Gallery** | ≥2 | Real captures §4.2 | [ ] |
| **Video** | Optional | YouTube full URL | [ ] |
| **Interactive demo** | Optional | Only if faithful | [ ] |
| **Makers** | Yes | All builders | [ ] |
| **Shoutouts** | Optional | Tools that mattered; verify any cap live | [ ] |
| **Promo** | Optional | Offer + code + expiry | [ ] |
| **First comment** | Strongly recommended | §5.1 | [ ] |
| **Schedule** | Yes | Up to ~1 month ahead; canonical PT midnight default | [ ] |

### 5.1 First maker comment (template)

Ask for **feedback**, not upvotes. Origin story must be **literally true**.

```text
Hi Product Hunt — [Name] here, maker of SPUR.

I built SPUR after [one true, specific incident]. The problem wasn’t getting an agent
to write code; it was keeping multiple agents’ work visible, isolated, and recoverable.

SPUR is a local-first control tower for Claude Code, Codex, Kiro, and Gemini. Workers
run in git worktrees; plans and sessions resume after restarts; every result reaches
one review surface: approve, deny, modify, or retry.

It’s for senior/staff engineers and tech leads already juggling CLI coding agents.
It isn’t an agent replacement or set-and-forget autonomy.

Community is a free solo daily driver with one concurrent worker. Pro adds up to
10 workers, Telegram review, and Insights.

I’d value blunt feedback on the install-to-first-review path. Where did you get stuck?
[optional PH promo]
```

- [ ] Saved in PH draft
- [ ] Accuracy pass vs PRD (tiers, quotas, cost visibility, platform support honesty)

---

## 6. Audience activation (policy-safe)

### 6.1 Email sequence (waitlist)

| When | Subject angle | CTA |
|---|---|---|
| T−7 | “We launch on Product Hunt next week — date locked” | Follow product on PH |
| T−1 | “Tomorrow at 12:01 AM PT — what to expect” | Calendar / reminder |
| T+0 (once, shortly after live) | “We’re live on Product Hunt” | Open listing, try install, leave feedback |
| T+1 | Thanks + install help | Docs / Discord |

Rules:

- [ ] Ask for **try / feedback**, never “please upvote”
- [ ] Segment beta users vs cold waitlist
- [ ] Prefer **one** launch-day email over same-day non-opener spam

### 6.2 Social

- [ ] X thread: pain → real demo → PH link → install
- [ ] LinkedIn: Team Lead angle (review + ops visibility)
- [ ] Community posts after live
- [ ] No identical copy-paste blast across many accounts

### 6.3 Personal / beta network (not “quality votes”)

Policy-safe language:

> Share the launch individually with beta users and peers who already use CLI coding agents. Ask them to try one workflow and offer honest feedback if they choose. Do not ask for votes, screen for account age, or coordinate voting times.

- [ ] Personal list of people who **use** the product category
- [ ] Brief: what it is, 1 real GIF, link, feedback ask
- [ ] No account-age filtering, no vote timing coordination

---

## 7. Launch day operating system

### 7.1 Roles (minimum)

| Role | Responsibility |
|---|---|
| **Launch commander** | Go-live, points/rank watch, incident call |
| **Comment desk** | Reply to PH comments; escalate bugs |
| **Social desk** | X/LinkedIn timing; no over-posting |
| **Support desk** | Install failures, Discord/email |
| **Analytics** | Visits, installs, activation funnel |

- [ ] Names filled
- [ ] Coverage for first hours + US business day (full 24h theater optional if unstaffable)

### 7.2 Hour-by-hour (Pacific — adjust to your coverage)

| Window (PT) | Actions |
|---|---|
| **12:01–12:15 AM** | Live → first comment if needed → verify gallery/video → war-room open |
| **First hours** | Reply to every comment; organic share to beta peers; **no** engineered vote spikes |
| **Morning business hours** | Main email (once); LinkedIn; founder/network outreach (feedback framing) |
| **Business day** | Continuous maker replies; FAQ updates if same question repeats |
| **Evening** | Soft authentic share only if energy remains |
| **End of PH day** | Thank-you comment; snapshot points/rank; no panic tactics |

### 7.3 Launch day “never do” list

- [ ] Do not beg for upvotes
- [ ] Do not run paid “guaranteed #1” services
- [ ] Do not ship install.sh breaking changes
- [ ] Do not argue with hostile commenters past one factual reply
- [ ] Prefer a **content freeze** as ops hygiene — not because “edits reset the algorithm” (unverified folklore)

---

## 8. Post-launch (T+1 → T+30)

### 8.1 Immediate (T+1–T+3)

- [ ] Clear remaining comments
- [ ] Log product insights into beads / tracker
- [ ] Internal “what we learned” note
- [ ] PH badge optional
- [ ] Thank early supporters individually where real
- [ ] Snapshot: points/rank, comments, visits, installs, funnel steps, emails

### 8.2 Week 1–4

- [ ] Fix biggest onboarding drop-off
- [ ] Testimonials with written consent
- [ ] Directory wave (`marketing/launch/directory-tracker.csv`) while warm
- [ ] Ongoing maker presence if useful
- [ ] Relaunch later only with **material product change** and generally **≥6 months** between launches for same product/root domain — [relaunch guidance](https://help.producthunt.com/en/articles/484934-can-i-relaunch-my-product)

### 8.3 Retrospective template

| Question | Answer |
|---|---|
| Featured? | |
| PotD / PotW rank / points? | |
| Install → first-review rate? | |
| Which gallery / comment drove questions? | |
| What broke under load? | |
| What prep would we cut next time? | |
| Was PH worth the focus week? | |

---

## 9. SPUR-specific risk register for PH

| Risk | Why it hits on PH | Mitigation |
|---|---|---|
| “Just another AI wrapper” editorial skip | Crowded AI category | Lead with orchestration + review gate + local-first resume |
| TUI hard to demo | Gallery noise | Real terminal captures + resume video |
| Complex install | High bounce | Prerequisites + 5-minute path docs |
| Proprietary / no public GitHub | Dev skepticism | Honest maker comment; signed binary; local-first privacy |
| Cost claims overreach | Trust burn | Visibility not governance |
| Community vs Pro confusion | Comment pile-on | Free daily driver + **1 worker** + Pro bullets labeled |
| Windows users | Support load | Honest platform matrix; no fake WSL/native dates |
| Stale May marketing package | Wrong claims ship | **Superseded** — do not execute from it |
| Hunter delay | False dependency | Self-hunt default |

---

## 10. Master checkbox summary

### Strategy

- [ ] Goals (platform + business numbers) signed
- [ ] Narrative locked to PRD v2.3
- [ ] Canonical day/time published
- [ ] Self-hunt default decided

### Product

- [ ] Install + 15-minute happy path green
- [ ] Review gate demo reliable
- [ ] Resume demo reliable (video)
- [ ] Pricing/tier claims match signed policy
- [ ] Privacy/security pages if needed
- [ ] Claim matrix signed

### Assets

- [ ] Tagline ≤60 (default C)
- [ ] Description ≤260
- [ ] Thumbnail + ≥2 real gallery images
- [ ] Video with resume proof
- [ ] First maker comment
- [ ] Pricing field set
- [ ] Shoutouts verified live

### Audience

- [ ] Beta / peer list for authentic feedback (not vote engineering)
- [ ] PH product follow / forum presence
- [ ] Email + social ready
- [ ] Maker accounts ready

### Launch day

- [ ] Roles staffed for critical windows
- [ ] Hour-by-hour runbook
- [ ] FAQ outlines
- [ ] Incident plan
- [ ] Deploy freeze holds

### Aftermath

- [ ] T+1 metrics + thank-yous
- [ ] T+7 retro
- [ ] Onboarding fixes shipped
- [ ] Testimonials pipeline

---

## 11. Source map

| Topic | Primary sources |
|---|---|
| Official prep, fields, first comment, timing | [producthunt.com/launch](https://www.producthunt.com/launch), [preparing-for-launch](https://www.producthunt.com/launch/preparing-for-launch) |
| Featuring / eligibility | [Featuring Guidelines](https://help.producthunt.com/en/articles/9883485-product-hunt-featuring-guidelines) |
| Points (not raw upvotes) | [What are points?](https://help.producthunt.com/en/articles/10275873-what-are-points) |
| Sharing / anti-gaming | [How do I share](https://help.producthunt.com/en/articles/2690626-how-do-i-share-my-post), [Community Guidelines](https://help.producthunt.com/en/articles/3615694-community-guidelines) |
| Relaunch | [Can I relaunch?](https://help.producthunt.com/en/articles/484934-can-i-relaunch-my-product) |
| SPUR product truth | [`SPUR_PRD.md`](../../SPUR_PRD.md) |
| Content-marketer audit | [`producthunt-checklist-content-review.md`](./producthunt-checklist-content-review.md) |
| Cross-channel ops | [`marketing/launch/playbook.md`](../../marketing/launch/playbook.md) |
| Superseded May PH package | [`marketing/launch/product-hunt.md`](../../marketing/launch/product-hunt.md) |

---

## 12. One-page “done means”

SPUR’s Product Hunt launch is **successful** if:

1. **Community install → first review** works under traffic.  
2. Listing is honest and **PRD-true** (tiers, quotas, platforms).  
3. Maker presence is **human and fast** during staffed windows.  
4. Audience activation stays **policy-safe** (feedback, not engineered votes).  
5. You leave with real funnel data, feedback, and a written retro — even without #1 PotD.

**Featured / high rank** is a desired amplifier, not the definition of done.

Rank without activation is vanity. Activation without honesty is churn. Aim for both.
