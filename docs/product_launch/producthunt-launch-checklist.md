# SPUR × Product Hunt — Success Launch Checklist

**Status:** Actionable checklist (ready for ownership assignment)  
**Date:** 2026-07-16  
**Grounded in:** [`SPUR_PRD.md`](../../SPUR_PRD.md) v2.3 · official [Product Hunt Launch Guide](https://www.producthunt.com/launch) · 2025–2026 launch research  
**Related assets:** [`marketing/launch/product-hunt.md`](../../marketing/launch/product-hunt.md) · [`marketing/launch/playbook.md`](../../marketing/launch/playbook.md) · [`marketing/messaging/positioning.md`](../../marketing/messaging/positioning.md)

---

## 0. How to use this document

- Checkboxes are the work unit. Assign an **owner** and a **due (T−N)** before T−30.
- **T−0** = launch day (Product Hunt day rolls at **12:01 AM Pacific**).
- “Success” is **not only rank**. Rank helps; **installs, email capture, reviews, and durable community** are the business outcomes (see §1).
- Do **not** invent tier claims, open-source promises, or cost-governance language that contradicts the PRD (§2).

---

## 1. Define success before you launch

Product Hunt official guidance: set goals first; rank is a means, not the only end.

### 1.1 Rank / visibility goals (platform)

| Goal | Target | Notes |
|---|---|---|
| **Featured** | Must-have | Non-Featured posts get minimal traffic and may not appear on mobile. Editorial criteria: *useful, interesting, well-made, creative*. |
| **Top 5 Product of the Day** | Stretch | Competitive weekdays often need ~500–800+ quality upvotes; weekends often lower competition / lower absolute traffic. |
| **#1 Product of the Day** | Aspirational | Requires early velocity + sustained engagement all day; not the only definition of a good launch. |
| **Comment quality** | ≥30 substantive comments | Maker replies within ~10 minutes during peak hours. |

### 1.2 Business goals (SPUR-specific — PRD-aligned)

Grounded in PRD personas (Orchestrator, Team Lead, Mobile Operator) and metrics culture:

| Metric | Suggested T+0–T+7 target | Why |
|---|---|---|
| **Install attempts** (`install.sh` / binary downloads) | Track raw + unique | Primary funnel; PH is top-of-funnel, not MRR day-one. |
| **Successful Community activation** | First brain session + at least one review card | Proves product, not just curiosity clicks. |
| **Email / waitlist captures** from PH UTM | 400–1,000+ over launch week | Research: pre-built audiences of ~400+ committed supporters correlate with top rankings. |
| **Pro interest / waitlist for paid** | Soft: replies asking about Telegram / concurrency | Pro upsells: Telegram, 10 workers, DuckDB Insights, auto-approve, custom skills (PRD §4.2). |
| **Named testimonials with consent** | ≥3 within T+14 | Launch-blocker in general playbook; PH amplifies social proof later. |
| **Press / partner pings** | Log every inbound | Secondary value often exceeds day-one upvotes. |

### 1.3 Explicit non-goals

- **Do not** optimize for upvote count via bots, vote-swapping rings, or “please upvote” spam — PH algorithm and community punish this.
- **Do not** promise SOC2/HIPAA, budget enforcement, or “set and forget autonomous coding” (PRD anti-persona + Risk #17).
- **Do not** run Show HN with a public repo if distribution remains proprietary (`marketing/launch/playbook.md` constraint). PH launch is independent of that choice.

**Owner:** ___________ · **Goals signed off by:** ___________ · **Date:** ___________

- [ ] Rank + business goals written and shared with launch team
- [ ] Analytics UTMs planned (homepage links may omit UTMs per PH rules — use post-click analytics / referrer)
- [ ] Success review calendar: T+1, T+7, T+30

---

## 2. Product narrative (from `SPUR_PRD.md`)

Use this as the **source of truth** for all PH copy. Prefer grounded capabilities over roadmap.

### 2.1 Positioning (use these, not free-form hype)

| Element | PRD-backed copy |
|---|---|
| **Category** | Control tower / orchestration layer for AI *coding agents* — not a replacement for Claude Code, Codex, Kiro, Gemini. |
| **One-liner** | *“One brain, many workers, zero lost context.”* |
| **Conversion one-liner** | *“Issue in, PR out — across every agent, in parallel, with one review surface.”* |
| **What to market first** | (1) Session immortality / resume · (2) Worktree isolation + human review gate · (3) Local-first durability · (4) Ops surfaces (Plan/Loop/Explore) as proof of depth |
| **Honest gaps** | Cost = **visibility**, not governance · Peer mailbox experimental · Insights maturing (Pro) |

### 2.2 Tagline options (PH max **60** characters)

Drafts grounded in PRD (finalize one before T−7):

| Option | Tagline | Chars | Notes |
|---|---|---|---|
| **A (recommended)** | Issue in, PR out — across every agent. | 42 | Conversion one-liner compressed |
| **B** | One brain, many workers, zero lost context. | 45 | Product vision one-liner |
| **C** | Control tower for CLI coding agents. | 38 | Category-owning, safe |
| **D** | Parallel agents. One review gate. Local-first. | 48 | Feature triad |
| **E** *(from May package)* | One live cost ledger across every CLI coding agent. | 52 | Strong if cost accuracy disclosure is ready |

- [ ] Final tagline chosen and character-counted
- [ ] Tagline does not use banned hype (“most advanced”, “#1 AI”, “game-changing”)
- [ ] Tagline matches Featured criteria (clear usefulness, not vague platform-speak)

### 2.3 Description draft (PH max **260–500** depending on field; keep scannable)

**Recommended (~240 chars):**

> SPUR orchestrates Claude Code, Codex, Kiro, and Gemini from one TUI. Workers run in git worktrees; every diff hits a human review gate. Plans and sessions survive restarts (local-first). Community free; Pro adds Telegram review and deeper analytics.

- [ ] Description reviewed against PRD tier map (Community complete daily driver; Pro = Telegram / concurrency / Insights)
- [ ] No claim of open-source unless policy changes
- [ ] Install CTA path is correct: `curl … install.sh` (not `cargo install` in public PH copy)

### 2.4 Topics / launch tags (up to 3)

Suggested set:

1. **Developer Tools**
2. **Artificial Intelligence** *or* **Productivity**
3. **Open Source** *only if true* — otherwise **Terminal** / **Git** / **SaaS** as available

- [ ] 3 tags selected that match actual product surfaces
- [ ] Tags checked live in PH submit UI (labels change over time)

### 2.5 Personas to speak to in maker comment

1. **Orchestrator** — rate limits, multi-tab agent chaos, session loss  
2. **Team Lead** — spend visibility, review queue (not “Issue Browser is Team-only”)  
3. **Mobile Operator** — Telegram review (**Pro**)

- [ ] First comment names who it is / isn’t for (PH loves clear ICP)

---

## 3. Product readiness (go / no-go)

PH traffic is unforgiving. Ship a **loveable** Community path, not a half-setup CLI.

### 3.1 Launch-blocker product checks (from PRD + playbook)

| # | Check | Owner | Status |
|---|---|---|---|
| 1 | Cold install: `install.sh` works macOS + Linux; signature/checksums documented | | [ ] |
| 2 | `spur init` → configure ≥1 brain agent → first session in &lt;15 minutes for a motivated user | | [ ] |
| 3 | Demo path: issue/plan → worker in worktree → **review card** A/D/M/R | | [ ] |
| 4 | Session resume / re-attach works after process kill | | [ ] |
| 5 | Status page or known-issue note if flaky agents | | [ ] |
| 6 | `/privacy` (and ideally `/security`) live if telemetry or email capture exists | | [ ] |
| 7 | Pricing / Community vs Pro page matches **signed policy** (PRD §4), not outdated marketing tables | | [ ] |
| 8 | PH landing URL stable (homepage or dedicated `/launch`); **no bit.ly / UTM on PH product URL field** | | [ ] |
| 9 | Capacity: install + docs CDN can absorb traffic spike | | [ ] |
| 10 | Support channel staffed launch day (Discord / email / X) | | [ ] |

### 3.2 Demo script (60–90 seconds) — required asset input

Story arc aligned with PRD differentiators:

1. **Pain** (5s): multi-agent tabs / rate limit / lost context  
2. **Control tower** (20s): Dashboard lineage tree + workers  
3. **Review gate** (20s): Approve a real diff  
4. **Durability** (15s): kill/restart → resume  
5. **Close** (10s): Community free install + PH offer if any  

- [ ] Loom/YouTube public (unlisted OK; **not private** — PH needs full YouTube URL)
- [ ] Captions/subtitles for silent autoplay environments
- [ ] Optional: interactive demo (Arcade / Supademo / Storylane — free PH tools listed by Product Hunt)

---

## 4. Pre-launch timeline checklist

### 4.1 T−12 to T−8 weeks — audience & product

Research consensus: **8–12 weeks** prep; **~400+** committed supporters strongly improves odds vs cold launch.

- [ ] Waitlist / email list live (target **400+** engaged; quality &gt; raw size)
- [ ] “Coming Soon” on Product Hunt scheduled when ready (collect **followers**; PH emails them on launch)
- [ ] Maker PH profiles complete (photo, bio, links) for **all** makers
- [ ] Organic PH participation: thoughtful comments on related launches (dev tools / AI agents) — **≥30 days** of real engagement preferred
- [ ] Seed private beta: Orchestrator-persona users who will show up on day one
- [ ] Confirm install + onboarding metrics from beta (activation rate)

### 4.2 T−6 to T−4 weeks — assets & story

- [ ] Tagline + description finalized (§2)
- [ ] First maker comment drafted + peer-reviewed (§5)
- [ ] Gallery storyboard: **≥2 required**, recommend **5–8** high-signal frames
- [ ] Thumbnail (square; PH recommends **240×240**; &lt;3MB; GIF first frame must read static)
- [ ] Gallery images (recommend **1270×760**)
- [ ] Demo video on YouTube (optional but ~half of PotD winners include video)
- [ ] Interactive demo (optional, high conversion for complex TUI products)
- [ ] Promo offer defined if any (PH promo fields: offer + code + expiry) — e.g. extended Pro trial for hunters
- [ ] Shoutouts shortlist (max **3** tools that helped build SPUR — beads, ratatui, ACP ecosystem, etc.)
- [ ] Website PH-ready: clear CTA above fold, social proof only if real, mobile OK

**Suggested gallery sequence for SPUR**

| # | Frame | PRD proof |
|---|---|---|
| 1 | Hero: Dashboard / lineage tree | Control tower |
| 2 | Review card A/D/M/R | Human gate as state machine |
| 3 | Worktree isolation / parallel workers | Isolation + quota story (Community = 1 worker honest) |
| 4 | Session resume / “close laptop” | Event-sourced durability |
| 5 | Plan Browser / Loop Browser | Ops depth (secondary) |
| 6 | Install one-liner | Time-to-value |
| 7 | Optional: Telegram review | **Label as Pro** |
| 8 | Optional: Cost badge / Insights | Visibility not governance; Pro for Insights |

Asset production detail also lives in [`marketing/launch/product-hunt.md`](../../marketing/launch/product-hunt.md) §4 (May package — refresh numbers/copy to PRD v2.3 before ship).

### 4.3 T−3 to T−2 weeks — community & hunter decision

Official PH stance: **self-hunt is fine** (large share of featured / #1 posts are self-hunted). Do not block launch waiting for a celebrity hunter. Do not pay for hunters (PH discourages this).

- [ ] Decision: **self-hunt** vs optional hunter co-sign
- [ ] If hunter: assets package sent ≥1 week early; niche overlap preferred over vanity follower count
- [ ] Warm list segmented: beta users, personal network, Discord, X, LinkedIn
- [ ] Soft asks drafted: “We’re live — feedback welcome” (**never** “upvote us”)
- [ ] Competitive day scan: avoid major Apple/Google keynotes if possible
- [ ] Day-of-week choice locked:

| Goal | Typical choice | Tradeoff |
|---|---|---|
| Max traffic / business audience | **Tue–Thu** | Harder #1 |
| Easier rank, smaller audience | **Weekend** | Lower absolute visits |
| Official rule of thumb time | **12:01 AM PT** | Full 24h window; schedule up to 1 month ahead |

For SPUR (dev tools / Orchestrator ICP): **Tue–Thu at 12:01 AM PT** is the default recommendation unless team coverage cannot staff the full day.

### 4.4 T−7 days — dry run

- [ ] PH draft fully filled; scheduled launch time set
- [ ] First comment pre-written in submit flow
- [ ] All makers added by PH username (accounts created early)
- [ ] Social posts scheduled (X, LinkedIn) — link to PH **after** live
- [ ] Email #1 ready for waitlist (see §6 templates)
- [ ] Reply macros for FAQs (OSS?, vs Cursor?, pricing?, Windows?) — see marketing package §6
- [ ] Team roles assigned (§7)
- [ ] Install rehearsal from **clean machines** (no local cargo target hacks)
- [ ] Status / incident plan if install breaks

### 4.5 T−1 day — freeze

- [ ] No risky deploys to install path or license service
- [ ] Final gallery order pass (mobile + desktop)
- [ ] Confirm launch time in **Pacific** (account for PDT vs PST → UTC conversion)
- [ ] Makers logged in on two devices
- [ ] Hunter (if any) has live URL draft ready
- [ ] Personal calendar blocked for launch day
- [ ] Sleep plan for midnight PT launch (or designate night-shift owner)

---

## 5. Product Hunt submission content checklist

Map 1:1 to PH submit fields ([official content checklist](https://www.producthunt.com/launch/preparing-for-launch)).

| Field | Required | SPUR prep | Done |
|---|---|---|---|
| **Product URL** | Yes | `https://getspur.dev` (or launch page). No short links / UTM in this field. | [ ] |
| **Name** | Yes | `SPUR` only — no slogan in the name | [ ] |
| **Tagline** | Yes (≤60) | §2.2 | [ ] |
| **Links** | Optional | GitHub only if public; docs; Discord | [ ] |
| **X handle** | Optional | Product handle, not only personal | [ ] |
| **Description** | Yes | §2.3 | [ ] |
| **Launch tags** | Up to 3 | §2.4 | [ ] |
| **Thumbnail** | Yes | 240×240-ish square, &lt;3MB | [ ] |
| **Gallery** | ≥2 | §4.2 sequence | [ ] |
| **Video** | Optional | YouTube full URL, public/unlisted | [ ] |
| **Interactive demo** | Optional | Arcade/etc. | [ ] |
| **Makers** | Yes | All builders with PH accounts | [ ] |
| **Shoutouts** | Max 3 | Tools that mattered | [ ] |
| **Promo** | Optional | Offer + code + expiry all required if used | [ ] |
| **First comment** | Strongly recommended | 70% of PotD/Week/Month winners had maker first comment | [ ] |
| **Schedule** | Yes | Up to 1 month ahead; prefer 12:01 AM PT | [ ] |

### 5.1 First maker comment structure (template)

PH guidance: humble, specific ICP, story, features, ask for **feedback not upvotes**.

```text
Hi Product Hunt — [Name] here, building SPUR.

Problem: I was juggling Claude Code / Codex / … across tabs. Rate limits and closed laptops killed context.

What SPUR is: a Rust-native control tower. One brain, many workers in git worktrees, every change through a human review gate (approve / deny / modify / retry). Plans and sessions are local-first and resume after restart.

What it is not: not a Cursor replacement; not fully autonomous “merge without you.”

Who it’s for: staff+ engineers and tech leads already running multi-agent CLI workflows.

Community is free (full solo daily driver). Pro adds Telegram review, higher concurrency, analytics, and more.

Ask: try the install, break it, tell us what failed in onboarding. Feature requests welcome.
[optional PH promo]
```

- [ ] First comment saved in PH draft (not only in Notion)
- [ ] Accuracy pass vs PRD (tiers, cost visibility, Windows/WSL honesty)

---

## 6. Audience activation (pre-written messages)

### 6.1 Email sequence (waitlist)

| When | Subject angle | CTA |
|---|---|---|
| T−7 | “We launch on Product Hunt next week — date locked” | Follow on PH Coming Soon |
| T−1 | “Tomorrow at 12:01 AM PT — what to expect” | Calendar / reminder |
| T+0 (~minutes after live) | “We’re live on Product Hunt” | Open listing, leave feedback |
| T+0 afternoon | Soft reminder for non-openers | Link + one feature GIF |
| T+1 | Thanks + install help | Docs / Discord |

Rules:

- [ ] Ask for **support / feedback / try**, never “please upvote”
- [ ] Segment: beta users vs cold waitlist (different tone)
- [ ] Expect ~10–20% click-to-PH if list is warm; plan list size accordingly

### 6.2 Social

- [ ] X thread: pain → demo → PH link → install
- [ ] LinkedIn: Team Lead angle (review + visibility)
- [ ] Discord / community posts after live only
- [ ] Avoid identical copy-paste across 20 accounts (looks inorganic)

### 6.3 Personal network (highest quality votes)

- [ ] Private message list of 30–50 people who **use** AI coding tools
- [ ] Prefer people with established PH accounts (algorithm weights experienced voters more)
- [ ] Brief: what it is, 1 GIF, link, “honest feedback appreciated”

---

## 7. Launch day operating system

### 7.1 Roles (minimum)

| Role | Responsibility |
|---|---|
| **Launch commander** | Go-live, rank watch, incident call |
| **Comment desk** | Reply to every PH comment; escalate bugs |
| **Social desk** | X/LinkedIn timing; no over-posting |
| **Support desk** | Install failures, Discord/email |
| **Analytics** | Rank, traffic, installs, signup funnel snapshots |

- [ ] Names filled for each role
- [ ] Backup person for comment desk overnight (EU/US coverage if possible)

### 7.2 Hour-by-hour (Pacific time)

Adjust to your coverage; keep the **first 4 hours** sacred.

| Window (PT) | Actions |
|---|---|
| **12:01–12:15 AM** | Launch live → post first comment if not embedded → verify gallery/video → pin team war-room link |
| **12:15–4:00 AM** | Activate closest supporters steadily; reply to every comment; **no vote spikes** from sketchy sources; social soft announce |
| **~4:00 AM** | Rank becomes more competitive/visible historically — stay present |
| **6:00–9:00 AM** | Main email blast (peak opens); LinkedIn; personal network |
| **Business day** | Continuous maker replies; ship micro-FAQ updates on site if same question repeats |
| **EU evening / US evening** | Second soft push; “last hours” only if authentic |
| **11:00 PM–12:00 AM** | Thank-you comment; capture final rank screenshot; no panic tactics |

Research heuristics (directional, not guarantees):

- Early velocity matters (first hours often decide Featured/top-shelf contention).
- Steady engagement beats unnatural spikes.
- Reply latency under ~10 minutes correlates with healthier threads.

### 7.3 Launch day “never do” list

- [ ] Do not edit critical listing fields mid-day if it risks resetting momentum (plan freezes content)
- [ ] Do not beg for upvotes in comments
- [ ] Do not run paid “guaranteed #1” services
- [ ] Do not ship install.sh breaking changes
- [ ] Do not argue with hostile commenters past one factual reply

---

## 8. Post-launch (T+1 → T+30)

### 8.1 Immediate (T+1–T+3)

- [ ] Reply to remaining comments
- [ ] Log every product insight into beads / issue tracker
- [ ] Publish “what we learned” internal note
- [ ] Add PH badge to site **only if** proud of result (some data claims conversion lift; optional)
- [ ] Thank hunters, makers, early supporters individually where possible
- [ ] Snapshot metrics: rank, upvotes, comments, visits, installs, activations, emails

### 8.2 Week 1–4

- [ ] Convert PH traffic into onboarding improvements (biggest drop-off step)
- [ ] Harvest testimonials with written consent
- [ ] Directory wave (see `marketing/launch/directory-tracker.csv`) while attention is warm
- [ ] Consider Product Hunt **Ship** / ongoing maker presence if relevant
- [ ] If major release later: plan a **second launch** only with material new value (PH allows relaunch with real updates; don’t spam)

### 8.3 Retrospective template

| Question | Answer |
|---|---|
| Did we hit Featured? | |
| Rank PotD / PotW? | |
| Install → activation rate? | |
| Which gallery slide / comment drove questions? | |
| What onboarding broke under load? | |
| What would we cut from prep next time? | |
| Net: was PH worth the week of focus? | |

---

## 9. SPUR-specific risk register for PH

| Risk | Why it hits on PH | Mitigation |
|---|---|---|
| “Just another AI wrapper” editorial skip | Crowded AI category | Lead with **orchestration + review gate + local-first resume**, not “AI magic” |
| TUI hard to demo in screenshots | Gallery looks like noise | Real terminal captures + short video; interactive demo |
| Complex install (agent CLIs required) | High bounce | PH landing: prerequisites checklist + 5-minute path; pre-seed agents docs |
| Proprietary / no public GitHub | Dev audience skepticism | Honest maker comment; signed binary; local-first privacy story |
| Cost claims overreach | Trust burn with Orchestrator ICP | “Visibility not governance” (PRD Risk #17) |
| Community vs Pro confusion | Comment pile-on | Explicit free daily driver + Pro bullets (Telegram, concurrency, Insights) |
| Windows users | Instant “does it support?” | WSL honesty; native roadmap without fake dates |
| Hunter delay | False dependency | Self-hunt default; hunter is bonus amplification only |

---

## 10. Master checkbox summary (print this)

### Strategy

- [ ] Goals (rank + business) signed
- [ ] Narrative locked to PRD v2.3
- [ ] Day/time locked (default Tue–Thu 12:01 AM PT)
- [ ] Self-hunt vs hunter decided

### Product

- [ ] Install + 15-minute happy path green
- [ ] Review gate demo reliable
- [ ] Resume demo reliable
- [ ] Pricing/tier claims match signed policy
- [ ] Privacy/security pages if needed

### Assets

- [ ] Tagline ≤60
- [ ] Description crisp
- [ ] Thumbnail + ≥2 gallery (prefer 6+)
- [ ] Video and/or interactive demo
- [ ] First maker comment
- [ ] Promo (optional) complete fields
- [ ] 3 shoutouts

### Audience

- [ ] ≥400 engaged supporters *or* honest plan for smaller authentic launch
- [ ] PH Coming Soon followers
- [ ] Email + social + personal network ready
- [ ] Maker accounts warmed on PH

### Launch day

- [ ] Roles staffed 24h
- [ ] Hour-by-hour runbook
- [ ] Reply macros
- [ ] Incident plan
- [ ] No deploy freeze broken

### Aftermath

- [ ] T+1 metrics + thank-yous
- [ ] T+7 retro
- [ ] Onboarding fixes shipped
- [ ] Testimonials pipeline

---

## 11. Source map

| Topic | Primary sources |
|---|---|
| Official prep, fields, first comment, timing | [producthunt.com/launch](https://www.producthunt.com/launch), [preparing-for-launch](https://www.producthunt.com/launch/preparing-for-launch), [before-launch](https://www.producthunt.com/launch/before-launch) |
| Featured vs All, waitlist scale, hour-by-hour patterns | Industry guides synthesizing 2024–2026 launches (e.g. waitlist / Demand Curve style playbooks) — treat upvote thresholds as **directional** |
| SPUR product truth | [`SPUR_PRD.md`](../../SPUR_PRD.md) |
| Copy drafts & gallery prompts | [`marketing/launch/product-hunt.md`](../../marketing/launch/product-hunt.md) |
| Cross-channel T−14→T+7 ops | [`marketing/launch/playbook.md`](../../marketing/launch/playbook.md) |
| Positioning / heroes | [`marketing/messaging/positioning.md`](../../marketing/messaging/positioning.md) |

---

## 12. One-page “done means”

SPUR’s Product Hunt launch is **successful** if:

1. The listing is **Featured** (or still drives meaningful installs without it — rare).  
2. **Community install → first review** path works under traffic.  
3. Maker presence is **human and fast** all day.  
4. Copy and tiers are **PRD-true** (no trust debt with Orchestrator ICP).  
5. You leave with a **warmer waitlist**, real feedback, and a written retro — even if #1 PotD does not happen.

Rank without activation is vanity. Activation without honesty is churn. Aim for both.
