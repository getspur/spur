# SPUR Launch Playbook — T-14 → T+7

*Last updated: 2026-05-20. Owns the day-by-day operational checklist that turns the foundation in `marketing/product-marketing.md` V1.3, `marketing/messaging/{positioning,levers}.md`, and `marketing/site/*` into a public launch. Hero A (cost ledger) is the primary; peers-not-competitors stance applies throughout per `marketing/competitors/_summary-indirect.md:43`.*

**Hard constraints (do not violate):**

1. SPUR is proprietary (`product-marketing.md:10`). **No Show HN, no GitHub repo to point at.** HN submission is a regular submission of the launch blog post (`product-marketing.md:216`).
2. Install path is `curl -sSL getspur.dev/install.sh | sh` — never `cargo install` in any external copy (`product-marketing.md:204`).
3. No fabricated testimonials, no logo wall, no "trusted by" strip until the `product-marketing.md:183` launch-blocker on 3–5 named-user quotes is cleared (`levers.md:195`).
4. Owner placeholders: `[founder]`, `[DevRel]`, `[author]` — replace before T-14.

---

## Launch-block decisions

Every dependency below can slip T-0. Owner + go/no-go criterion is mandatory before T-14 work begins.

| # | Blocker | Source | Owner | Go criterion | If miss by T-7 |
|---|---|---|---|---|---|
| 1 | Stripe/Paddle checkout live at `getspur.dev/pro` issuing `personal_lifetime` SKU | `product-marketing.md:209`, `pricing.md:129` | [founder] | One real test purchase refunded; webhook fires license issuance | Ship without lifetime button; pricing page disclosure already drafted at `pricing.md:129` |
| 2 | 3–5 named-user testimonials from first 50 Community installs | `product-marketing.md:183` | [DevRel] | 3 quotes with real names + role + employer (or "indie") in hand, written consent | **Launch anyway** — hold empty space per `levers.md:195`; do not fake |
| 3 | `getspur.dev/install.sh` signed-binary endpoint hardened (TLS, signature verification, request log) | `product-marketing.md:204`, `product-marketing.md:237` | [founder] | Cold install on macOS + Linux from clean VM completes in <60s; request log writing to analytics | **No-go.** This is the funnel. Slip launch. |
| 4 | Cost-ledger accuracy validated against one heavy user's actual vendor invoices | `positioning.md:143` (riskiest-claim flag) | [founder] | Each of 5 extractors within ±10% of vendor invoice over one week, or copy updated to disclose lag | **No-go on Hero A.** Swap landing page to Hero B (control tower) and ship Hero A copy after fix |
| 5 | `/privacy` page live (telemetry + email + license dashboard make this legally required) | `sitemap.md:40,148` | [founder] | Page reviewed by counsel or templated from a comparable proprietary dev-tool; linked from footer + signup form | **No-go.** Legal exposure. Slip launch. |
| 6 | `/security` page live with signed-binary checksums + vuln-disclosure email | `sitemap.md:41,149` | [founder] | SHA256 + Ed25519 signature for each platform binary; `security@getspur.dev` mailbox monitored | Launch without `/security` is acceptable for 48h IF checksums are pinned in the install.sh comment; otherwise no-go |
| 7 | Telemetry signals wired: install.sh request log, activation event, signup-→-license funnel | `product-marketing.md:237` | [founder] | All three events visible in dashboard with test traffic from staging | Launch anyway, but flying blind on funnel — defer paid-ad start (T+3) until landed |
| 8 | Sitearch dependencies: `/quickstart`, `/pricing`, `/vs/*`, `/account` all 200-OK and pass Lighthouse ≥90 | `sitemap.md:28-42` | [founder] | All pages live, internal links resolve, mobile passes | No-go on T-0; partial launch with `/quickstart` + `/pricing` + `/vs/claude-code` only is the minimum-viable subset |

---

## T-14 — prep

Goal: turn every launch-blocker above from open to closed, in order of slip risk.

| Step | Owner | Success metric | Kill criterion |
|---|---|---|---|
| Provision `getspur.dev` apex + `status.getspur.dev` subdomain; pin TLS; set HSTS | [founder] | `curl -I https://getspur.dev` returns 200 with HSTS header | If domain not registrable as `getspur.dev`, fall back to documented alt in `homepage.md:5` and update copy globally before T-7 |
| Wire Stripe (or Paddle) checkout at `/pro`, issue `personal_lifetime` license on webhook | [founder] | One end-to-end test buy → license email → `spur license apply` succeeds | Skip lifetime SKU; ship monthly-only Pro at T-0; carry `pricing.md:129` disclosure as-is |
| Harden `install.sh`: signature check, OS detect, error handling, retry logic, request log to analytics endpoint | [founder] | Clean-VM install passes on macOS arm64 + Ubuntu 22.04 + Debian 12 | If signature-check infra not ready, ship checksums-pinned install.sh and flag it openly; do **not** ship unsigned in silence |
| Wire telemetry: (a) install.sh request log → events table, (b) `activation` event on first approved review, (c) `signup → license_issued` funnel | [founder] | Three events visible end-to-end with staging traffic | Launch without; flag paid-ad spend deferred until visible |
| Validate cost-ledger accuracy per `positioning.md:143`: reconcile each extractor against one heavy user's vendor invoice for the prior week | [founder] | All 5 within ±10%, OR Hero A subhead updated to disclose lag bound (e.g. "within 2 hours of vendor billing") | If accuracy fails AND no honest disclosure copy lands, swap homepage default to Hero B at T-0 |
| Stand up `/privacy` and `/security` pages (sitemap items 5+6 above) | [founder] | Both pages link from footer; checksums + Ed25519 sig published per platform | `/privacy` is no-go; `/security` may be deferred 48h with checksums in `install.sh` comment |
| Ship `spur feedback` CLI command landing at `/feedback` for post-install survey | [founder] | One submission round-trips from CLI to admin queue | Cut; defer to T+7 — does not block launch but does delay testimonial harvest |
| Draft launch blog post (the HN submission target) — frame: "we built a control tower for our CLI agents; here's the cost-ledger numbers from week one" | [author] | Draft reviewed against `positioning.md` Hero A claim and `levers.md` pratfall guidance; no logo wall, no fabricated quotes | Cut blog post → cut HN submission. Do not submit a thin one. |

---

## T-7 — quiet warm-up

Goal: line up the co-marketing surface area without leaking. **No public pre-announcement on X/LinkedIn this week** — `levers.md` favors pratfall + earned credibility over hype-stacking; a Friday-before tease invites scrutiny we have nothing to convert with yet.

| Step | Owner | Success metric | Kill criterion |
|---|---|---|---|
| Anthropic DevRel outreach: warm intro request, share Hero A draft + 60s brain-swap demo (`_summary-indirect.md:44` action #4), ask for a quote-tweet on launch day | [DevRel] | One DevRel contact confirms they'll amplify on T-0 (no commitment to endorsement) | No response by T-3 → drop and rely on organic channels |
| beads + ACP author outreach: courtesy heads-up that SPUR uses beads as plan store (`product-marketing.md:163`) and is an ACP client, offer co-marketing post on integration | [DevRel] | Acknowledgement; one confirmed "we'll RT the launch" or co-post | No response → still launch; their tools are credited in `/quickstart` and `/docs` regardless |
| Cursor team courtesy heads-up: short email to Cursor DevRel pointing at `marketing/site/vs-cursor.md` framing ("peers-not-competitors, most SPUR users keep Cursor open") — pre-empts any "they're swiping at us" read | [founder] | Email sent, acknowledged (no endorsement asked) | No response is fine — the goal is to prevent a hostile interpretation, not to secure amplification |
| Recruit 8–12 friendly beta installers from existing network for soft-validation install (separate from public Community launch) | [founder] | ≥8 successful installs, ≥3 willing to be on-the-record per launch-block #2 | <3 named quotes by T-2 → ship with empty testimonial space (do not fake; `levers.md:195`) |
| Final cost-ledger reconciliation pass (second sample user) | [founder] | Holds within ±10% bound | Swap homepage default to Hero B at T-1 |
| Pre-stage Product Hunt draft (assets, tagline, first comment, maker bio) in PH dashboard — scheduled, not published | [founder] | Draft saved, scheduled for 06:00 UTC T-0 | Slip PH to T+1; HN + X still go on T-0 |
| Pre-stage X thread + LinkedIn post drafts in scheduler | [founder] | Both drafts saved, reviewed against `levers.md` voice constraints | Manual post at T-0 — not a launch-blocker |
| Pre-stage cold-email batch 1 (warm-list opener, not cold-cold) per `marketing-cold-email` skill | [DevRel] | List segmented, copy reviewed, sender domain warmed | Defer batch 1 by 24h; not critical |

---

## T-1 — final check

| Step | Owner | Success metric | Kill criterion |
|---|---|---|---|
| OG images generated for `/`, `/pricing`, `/vs/*`, launch blog post per `marketing/site/og/PLAN.md` | [founder] | All 8 cards rendered, < 200KB each, embedded in page `<head>` | Use minimal text-only fallback OG — do not ship missing OG |
| 60s demo recorded: brain-swap failover (Claude → Codex → back) per `_summary-indirect.md:44` action #4 | [DevRel] | MP4 + GIF, hosted on `getspur.dev/demo`, embedded in launch blog post | Ship without demo video — replace with screenshots in blog post; primary kill happens only if blog post itself isn't ready |
| `/privacy` page live and footer-linked | [founder] | 200-OK, links resolve | No-go on launch (per blocker #5) |
| `/security` page live, checksums published per platform | [founder] | SHA256 + Ed25519 sig visible for each binary | Acceptable to defer 48h IF install.sh inlines checksums |
| Full end-to-end install rehearsal from 3 clean VMs (macOS, Ubuntu, Debian) | [founder] | All 3 complete `submit_plan` → review → approve flow without error | No-go. Fix or slip. |
| Verify all `marketing/site/*` pages 200-OK from the production domain; mobile pass | [founder] | Lighthouse ≥90 on `/`, `/pricing`, `/quickstart`, `/vs/claude-code` | Trim sitemap to the four core pages; defer `/vs/devin`, `/vs/cursor` if either fails |
| Schedule status.getspur.dev to publish an explicit "all systems green" at T-0 06:00 UTC | [founder] | Page reachable, polling green | Not a launch-blocker; nice signal |

---

## T-0 — launch sequence (hour-by-hour, UTC)

Single launch day. No Show HN at any point.

| UTC | Step | Owner | Success metric | Kill criterion |
|---|---|---|---|---|
| **06:00** | Publish Product Hunt listing (scheduled from T-7) | [founder] | Listing live; first comment from founder posted within 2 min | If PH ranking <#5 by 12:00 UTC, redirect engagement budget to HN comments; do not double-down on PH |
| **07:00** | Publish X thread (Hero A cost-ledger lead, links to blog post + PH) | [DevRel] | Thread live, pinned on `@getspur` profile | If <50 impressions in first 30 min, re-post at 11:00 UTC with different hook (Hero B framing) |
| **08:00** | Publish LinkedIn post (Team-Lead persona angle: cost visibility across vendors) | [founder] | Post live; tagged Anthropic DevRel + 3 beads/ACP authors per T-7 warm-up | None — LinkedIn is owned and recoverable |
| **10:00** | Submit launch blog post to Hacker News as a **regular submission** (NOT Show HN — proprietary, no public repo per `product-marketing.md:216`). Title: factual; no "Show HN", no "Launch" prefix | [founder] | Submission live; founder ready to answer comments within 5 min of any reply | If flagged within 1 hour, do not resubmit same URL — wait 7 days minimum |
| **11:00** | Send cold-email batch 1 (warm list first — never to never-contacted addresses on launch day; `marketing-cold-email` discipline) | [DevRel] | Send completes; bounce rate <2%; first replies tracked | If bounce >5%, pause batch 2 (planned T+3) for sender-reputation review |
| **14:00** | Reply-engagement window opens: dedicate next 6h to PH comments, HN comments, X replies, cold-email replies | [founder] + [DevRel] | Every comment answered within 15 min during the window | If volume exceeds 1 reply/min sustained, batch responses and post a single thread-level "answering in batches, here's the FAQ" pointer to `/quickstart` |

**Founder + DevRel are on-call from 06:00 to 22:00 UTC.** Do not ship code on launch day. Do not push install.sh changes on launch day.

---

## T+1 → T+7 — follow-up cadence

| Day | Step | Owner | Success metric | Kill criterion |
|---|---|---|---|---|
| **T+1** | Reply discipline: clear all remaining comments from T-0 across PH, HN, X, LinkedIn | [founder] + [DevRel] | Zero unanswered top-level comments by 18:00 UTC | If a thread is hostile and going nowhere, stop replying; do not feed it |
| **T+1** | Capture every real question received → seed FAQ updates on `/pricing`, `/quickstart`, blog comments | [author] | ≥5 questions added to FAQ JSON-LD (`marketing/site/schema/faq.jsonld`) | None — pure upside |
| **T+1** | Telemetry pull: install.sh runs, activations, signups (per `product-marketing.md:237`) | [founder] | First-24h numbers logged against the Day-30 targets in `product-marketing.md:228` | If install→activation rate <10%, freeze paid-ad spend (planned T+3) until first-run is fixed |
| **T+2** | Begin testimonial harvest from real installs (`spur feedback` queue + Discord) — explicit consent only | [DevRel] | ≥3 quotable, name-attributable testimonials in queue (un-published until consent confirmed) | None — multi-week effort; testimonials shipped only when ready |
| **T+3** | Second wave: paid-ad start (Google + Meta + LinkedIn per `product-marketing.md:219`) targeting Hero A copy | [founder] | Campaigns live with $X daily cap, conversion event = install.sh run | If telemetry funnel from T+1 still broken, do not start paid spend — flying blind |
| **T+3** | Co-marketing post with beads/ACP authors (if T-7 outreach landed) | [DevRel] | One co-post published | Skip silently if outreach didn't land |
| **T+5** | Mid-week ops post on X / LinkedIn — share a real number from the first 5 days (install count, "% of sessions using ≥2 vendors" — the north-star metric per `product-marketing.md:239`) | [DevRel] | One post per channel | Cut if numbers are embarrassing; the recap on T+7 is the higher-leverage moment |
| **T+7** | Recap post on the blog: "What we learned from launch week" — real numbers, real failures, real questions answered. Submit recap to HN as a separate regular submission | [author] | Recap published, HN submission live, X + LinkedIn share | If launch numbers under-shot Day-7 targets, recap still ships — pratfall lever per `levers.md` is on-brand |
| **T+7** | Decision gate: continue paid spend, double down on cold-email cadence 2, or pause and fix funnel | [founder] | Written go/no-go memo against Day-30 targets | If two of three primary metrics are <50% of plan, pause spend and run a one-week funnel fix |

---

## Reply-discipline rules (T-0 through T+7)

- Answer technical questions with a code snippet or a docs link, never with marketing copy.
- Never mention competitors negatively in any reply — peer framing in `marketing/site/vs-*.md` is the public stance.
- Hostile threads: one factual correction, then stop. Do not double down.
- If a real bug surfaces in a public thread, acknowledge it in-thread, file it, link the issue tracker URL once it exists, and move on.
- No fabricated testimonials in any comment, ever (`levers.md:81,195`).

---

## Cross-references

- Foundation: `marketing/product-marketing.md` V1.3
- Messaging: `marketing/messaging/positioning.md`, `marketing/messaging/levers.md`
- Site: `marketing/site/sitemap.md`, `marketing/site/homepage.md`, `marketing/site/pricing.md`, `marketing/site/vs-*.md`, `marketing/site/og/PLAN.md`
- Peer stance: `marketing/competitors/_summary-indirect.md`
- Channels available under proprietary constraints: `product-marketing.md:213-223`
