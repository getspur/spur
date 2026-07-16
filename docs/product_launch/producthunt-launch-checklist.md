# SPUR × Product Hunt — Success Launch Checklist

**Status:** Draft for ownership — **not execution-ready** until P0 fixes below are closed (content-marketer review 2026-07-16)  
**Date:** 2026-07-16 · **Rev:** 1.2 (journey-aligned demo + gallery vs live TUI problem stories)  
**Grounded in:** [`SPUR_PRD.md`](../../SPUR_PRD.md) v2.3 · official [Product Hunt Launch Guide](https://www.producthunt.com/launch) · [Featuring Guidelines](https://help.producthunt.com/en/articles/9883485-product-hunt-featuring-guidelines) · [Points](https://help.producthunt.com/en/articles/10275873-what-are-points) · live demos [`scripts/e2e/demos/tui-live/`](../../scripts/e2e/demos/tui-live/)  
**Product journey:** [`product-journey-ph.md`](./product-journey-ph.md) (**approved 2026-07-16**)  
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

| # | Check | Owner | Status | Grounding journey |
|---|---|---|---|---|
| 1 | Cold install: `install.sh` works macOS + Linux; signature/checksums documented | | [ ] | (install path, not TUI film) |
| 2 | `spur init` → configure ≥1 brain → land **Session Detail** in &lt;15 min | | [ ] | all live journeys (`story_session_land`) |
| 3 | **Hero path:** session → plan/workers → DELEGATE visible in Session Detail | | [ ] | `problem-plan-loop-drive` (+ optional `SPUR_DEMO_ALLOW_PLAN_LOOP=1`) |
| 4 | Specialist path: Explore adopt + `@worker` cascade without losing session | | [ ] | `product-e2e-flow` |
| 5 | Session resume / re-attach after quit (**show in video**) | | [ ] | `session-resume` (probe) + product-e2e attach |
| 6 | Ops visibility from session home (workers / Go to); lineage is secondary | | [ ] | `problem-ops-visibility` |
| 7 | Plan progress / campaign inventory reachable from session | | [ ] | `problem-plan-progress` |
| 8 | Backlog triage (Issues) reachable from session when seed present | | [ ] | `problem-backlog-triage` |
| 9 | Community **1 concurrent worker** quota visible/understandable | | [ ] | PRD + UI |
| 10 | `/privacy` (and ideally `/security`) if telemetry or email capture exists | | [ ] | legal |
| 11 | Pricing / Community vs Pro matches **signed policy** (PRD §4) | | [ ] | PRD |
| 12 | PH landing URL stable; **no UTM** on PH product URL field | | [ ] | web |
| 13 | Capacity + support staffed launch day | | [ ] | ops |
| 14 | v2.3 claim matrix signed for every PH-facing sentence | | [ ] | docs |

**Do not** make Dashboard lineage the hero proof. Live demos treat **Session Detail as operator home**; Dashboard / Lineage is optional overview (`PROBLEM_STORIES.md`).

### 3.2 Canonical product journey (cross-checked vs live TUI demos)

Source of truth for *what the product does on film*:

- Catalog: [`scripts/e2e/demos/tui-live/PROBLEM_STORIES.md`](../../scripts/e2e/demos/tui-live/PROBLEM_STORIES.md)
- Order: [`scripts/e2e/demos/tui-live/journeys.conf`](../../scripts/e2e/demos/tui-live/journeys.conf) (marketing value order)
- Full map: [`product-journey-ph.md`](./product-journey-ph.md)

```text
Session Detail (home)
  ├── ReAct (YOU / THINK / ACT / DELEGATE)
  ├── INSERT composer (@worker cascade)
  ├── Workers panel (Alt+d)
  ├── Alt+p plan inspector
  └── Ctrl+K Go to → Plans / Issues / Explore / Sessions

Dashboard / Lineage = optional ops overview (secondary)
```

**PH narrative = problem stories (value demos), not surface probes.**

| PH priority | Journey ID | User problem (exact catalog voice) | Proof the audience should see |
|---:|---|---|---|
| **1 — hero film** | `problem-plan-loop-drive` | submit_plan loop is a black box — drive brain↔worker | Session ·, INSERT, workers, DELEGATE/YOU; plan Progress when opened |
| **2** | `product-e2e-flow` | specialist + model/effort without losing context | Session attach, Explore adopt, `agent=` / `model=` / `effort=` cascade |
| **3** | `problem-ops-visibility` | what’s running where I work? | Session help, workers, Go to |
| **4** | `problem-plan-progress` | where is my multi-task campaign? | Plans / Progress from session |
| **5** | `problem-backlog-triage` | what’s P0 open in the backlog? | Issues list / open P0 when seeded |
| Support (not hero) | `session-resume` | closed laptop / re-attach | Session Detail history land |
| Regression only | `01`–`08` probes | chrome components | **Do not** lead PH gallery with these alone |

Beat spine (every value film): **HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION**.

### 3.3 PH demo video script (60–90s) — journey-aligned

Use **real capture** from the live project demos (prefer re-film with `SPUR_DEMO_STORY_PACE=1`, not AI mock terminals).

| Time | Beat | What to show | Journey ground |
|---:|---|---|---|
| 0–8s | HOOK | Multi-agent chaos / “black box campaign” pain | plan-loop problem sentence |
| 8–25s | ORIENTATION | Land **Session Detail** (composer + ReAct chrome) — say this is home | all stories `story_session_land` |
| 25–50s | ACTION | Workers panel + plan surface from session; optional live 1-task seed | `problem-plan-loop-drive` |
| 50–65s | PROOF | DELEGATE / worker activity visible in-session (not only Dashboard) | plan-loop proof anchors |
| 65–80s | PROOF+ | Quick cut: Explore adopt → `@worker` cascade (specialist ready) **or** quit/re-open session | `product-e2e-flow` / `session-resume` |
| 80–90s | RESOLUTION | Community free install; Pro = Telegram / more workers / Insights | PRD tiers |

Optional second short (15–20s) for gallery GIF only: `problem-ops-visibility` or backlog triage.

**Safety (match demo gates — do not claim live seed by default):**

- Observe-only is the honest default for most film.
- `SPUR_DEMO_ALLOW_PLAN_LOOP=1` / `SPUR_DEMO_ALLOW_AGENT_SEND=1` for real spend.
- `submit_plan` is **brain MCP**, not a TUI button — operator *lives* in Session Detail while the loop runs.

- [ ] Hero video storyboard signed against `problem-plan-loop-drive` (not Dashboard-first)
- [ ] Real capture (prefer `SPUR_DEMO_STORIES_ONLY=1 ./render.sh` or `./capture-live-seed.sh`)
- [ ] YouTube public or unlisted; full URL
- [ ] Captions/subtitles
- [ ] Interactive demo only if it mirrors Session Detail home

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
- [ ] Hero demo film from **`problem-plan-loop-drive`** (§3.3)
- [ ] Optional GIF cuts from `product-e2e-flow` / `session-resume`
- [ ] Interactive demo optional
- [ ] Promo offer if any (offer + code + expiry all required in PH form)
- [ ] Shoutouts: pick tools that mattered; **do not hardcode max 3** — verify live UI (docs conflict)
- [ ] Website PH-ready: clear CTA, real social proof only, mobile OK

**Suggested gallery sequence (journey-aligned)**

| # | Frame (real capture) | Journey / proof | Caption intent |
|---:|---|---|---|
| 1 | **Session Detail home** — composer + ReAct | all stories land here | Operator home for multi-agent work |
| 2 | Workers panel / DELEGATE in session | `problem-plan-loop-drive` | Drive brain↔worker without leaving the session |
| 3 | Plan Progress / campaign from session (Alt+p / Plans) | `problem-plan-progress` + plan-loop | Multi-task campaign is inventory, not a black box |
| 4 | Explore adopt + `@worker` cascade draft | `product-e2e-flow` | Specialist + model/effort without losing context |
| 5 | Session resume / re-attach | `session-resume` | Close laptop; session survives |
| 6 | Issues backlog triage (if seed) | `problem-backlog-triage` | P0 open work from the same control surface |
| 7 | Optional: Dashboard lineage overview | `problem-ops-visibility` (secondary) | System map — **not** the hero |
| 8 | Install one-liner | install path | Time-to-value |
| 9 | Optional: Telegram review | Pro only | **Label as Pro** |
| 10 | Optional: cost badge / Insights | Pro / observational | Visibility not governance |

Do **not** lead with Dashboard chrome tours or AI mock terminals. Do not invent keybindings (`space` collapse, `r` retry from tree, `spur cost --today`). Prefer frames already produced under `scripts/e2e/demos/tui-live/out/` (`09`–`13`, live seed `14`) after a story-pace re-capture.

**Marketing render recipe (from demo README):**

```bash
cd scripts/e2e/demos/tui-live
export SPUR_BIN="$(command -v spur)"
SPUR_DEMO_STORIES_ONLY=1 SPUR_DEMO_STORY_PACE=1 ./render.sh
# optional live seed film:
# ./capture-live-seed.sh   # → out/14-live-plan-loop-seed.{mp4,gif}
```

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

SPUR is a local-first control tower for Claude Code, Codex, Kiro, and Gemini. You work
in Session Detail — compose, watch ReAct (YOU / THINK / DELEGATE), drive workers and
plan progress from the same surface. Plans and sessions resume after restart. Every
result still hits a human review surface: approve, deny, modify, or retry.

What you’ll see in the demo: Session Detail (not a dashboard tour) → plan/worker loop
→ optional specialist @worker cascade when you need a persona + model/effort without
losing the session.

It’s for senior/staff engineers and tech leads already juggling CLI coding agents.
It isn’t an agent replacement or set-and-forget autonomy.

Community is a free solo daily driver with one concurrent worker. Pro adds up to
10 workers, Telegram review, and Insights.

I’d value blunt feedback on install → first session → first worker/review. Where did
you get stuck?
[optional PH promo]
```

- [ ] Saved in PH draft
- [ ] Accuracy pass vs PRD + journey catalog (Session Detail home; plan-loop as hero)

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
| TUI hard to demo | Gallery noise | Film **Session Detail** problem stories (`09`–`13` / live seed `14`), not chrome probes |
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

- [ ] Install + 15-minute happy path green (lands Session Detail)
- [ ] Hero path: `problem-plan-loop-drive` UAT green
- [ ] Specialist path: `product-e2e-flow` UAT green
- [ ] Resume proof available (`session-resume` or film cut)
- [ ] Pricing/tier claims match signed policy
- [ ] Privacy/security pages if needed
- [ ] Claim matrix signed

### Assets

- [ ] Tagline ≤60 (default C)
- [ ] Description ≤260
- [ ] Thumbnail + ≥2 real gallery images (**Session Detail–first**)
- [ ] Hero video = plan-loop journey (not Dashboard-first)
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
