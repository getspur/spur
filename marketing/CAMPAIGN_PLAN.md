# SPUR Marketing Campaign — Plan-as-Beads Structure

*Sketch v1 — 2026-05-20. Maps the marketing skills onto Spur's brain/worker/review system.*

## Operating Model

```
   Foundation (sequential, human-in-loop, single brain session)
        │
        ▼
   marketing/ artifacts (PRODUCT.md, RESEARCH.md, COMPETITORS.md, POSITIONING.md)
        │
        ▼
   beads epic: "SPUR Launch Campaign"
        │
   ┌────┴───────────────────────────────────────────────────┐
   │ Pillar epics, dispatched to workers via submit_plan    │
   │                                                        │
   │ 1. Foundation        (sequential — owner: you)         │
   │ 2. Positioning       (sequential — owner: you)         │
   │ 3. Web & Landing     (parallel — copywriting + cro)    │
   │ 4. Launch Moment     (parallel — launch + directories) │
   │ 5. Content/SEO       (parallel — seo + content + ai)   │
   │ 6. Outbound          (parallel — cold-email + ads)     │
   │ 7. Community         (continuous — social + community) │
   │ 8. Measurement       (cross-cutting — analytics + ab)  │
   └────────────────────────────────────────────────────────┘
```

Every artifact lives under `marketing/` (separated from code repo). Beads issue IDs prefixed `mkt.*` so they don't collide with engineering work.

---

## Phase 1 — Foundation (Sequential, You-In-The-Loop)

These are creative/strategic and shouldn't be parallelized — quality beats throughput.

| # | Bead | Skill | Output |
|---|---|---|---|
| F1 | `mkt.foundation.product-marketing` | `marketing-product-marketing` | `marketing/product-marketing.md` (V1 drafted today) |
| F2 | `mkt.foundation.customer-research` | `marketing-customer-research` | `marketing/research/voc.md` — Reddit (r/ClaudeAI, r/LocalLLaMA), Hacker News threads on Claude Code rate limits, X discussions on multi-agent workflows |
| F3 | `mkt.foundation.competitor-profiling` | `marketing-competitor-profiling` | `marketing/competitors/{acpx,tuicommander,ralph,agent-orchestrator,devin,cosine}.md` |
| F4 | `mkt.foundation.pricing` | `marketing-pricing` | `marketing/pricing.md` — Community/Pro/Team/Enterprise dollar amounts + value-metric decision |
| F5 | `mkt.foundation.marketing-ideas` | `marketing-marketing-ideas` | `marketing/IDEAS.md` — long-list of channels/tactics scored by ICE |

---

## Phase 2 — Positioning (Sequential)

| # | Bead | Skill | Output |
|---|---|---|---|
| P1 | `mkt.position.messaging` | `marketing-copywriting` | `marketing/messaging/positioning.md` — value props per persona (Orchestrator / Team Lead / Mobile Operator) |
| P2 | `mkt.position.psychology` | `marketing-marketing-psychology` | `marketing/messaging/levers.md` — loss aversion (lost context), social proof angles, scarcity (Pro early-access) |

---

## Phase 3 — Web & Landing (Parallel — first big fan-out)

Dispatch all in one `submit_plan` once Phase 1 + 2 are done. Each task = one worker.

| Bead | Skill | Output | Dep |
|---|---|---|---|
| `mkt.web.homepage` | `marketing-copywriting` + `marketing-cro` | `marketing/site/homepage.md` | F1, P1 |
| `mkt.web.pricing-page` | `marketing-copywriting` + `marketing-cro` | `marketing/site/pricing.md` | F4 |
| `mkt.web.docs-quickstart` | `marketing-copywriting` + `marketing-onboarding` | `marketing/site/quickstart.md` | F1 |
| `mkt.web.vs-claude-code` | `marketing-competitors` | `marketing/site/vs-claude-code.md` | F3 |
| `mkt.web.vs-devin` | `marketing-competitors` | `marketing/site/vs-devin.md` | F3 |
| `mkt.web.alternatives-page` | `marketing-competitors` | `marketing/site/multi-agent-alternatives.md` | F3 |
| `mkt.web.site-arch` | `marketing-site-architecture` | `marketing/site/sitemap.md` + `marketing/site/nav.md` | F1 |
| `mkt.web.schema-jsonld` | `marketing-schema` | `marketing/site/schema/{software,faq,product}.jsonld` | site live |
| `mkt.web.og-images` | `marketing-image` | `marketing/site/og/*.png` | P1 |
| `mkt.web.demo-video` | `marketing-video` | `marketing/site/demo-90s.mp4` script + storyboard | P1 |
| `mkt.web.cro-review` | `marketing-cro` | `marketing/site/cro-pass-1.md` (review of homepage + pricing) | homepage, pricing-page |

---

## Phase 4 — Launch Moment (Parallel, time-boxed)

| Bead | Skill | Output |
|---|---|---|
| `mkt.launch.plan` | `marketing-launch` | `marketing/launch/playbook.md` — T-14 → T+7 checklist |
| `mkt.launch.product-hunt` | `marketing-launch` | `marketing/launch/ph-assets.md` (tagline, gallery, hunter outreach) |
| `mkt.launch.hn-post` | `marketing-copywriting` | `marketing/launch/hn-show.md` ("Show HN: SPUR — issue in, PR out across every agent") |
| `mkt.launch.directories` | `marketing-directory-submissions` | `marketing/launch/directory-tracker.csv` (TAAFT, MCP registry, AlternativeTo, SaaSHub…) |
| `mkt.launch.twitter-thread` | `marketing-social` | `marketing/launch/x-thread.md` |
| `mkt.launch.linkedin-post` | `marketing-social` | `marketing/launch/linkedin.md` |
| `mkt.launch.press-kit` | `marketing-sales-enablement` | `marketing/launch/press-kit/` (logo, screenshots, one-pager) |

---

## Phase 5 — Content / SEO (Parallel, continuous)

| Bead | Skill | Output |
|---|---|---|
| `mkt.seo.audit` | `marketing-seo-audit` | `marketing/seo/audit.md` (once site exists) |
| `mkt.seo.ai-citations` | `marketing-ai-seo` | `marketing/seo/aeo.md` — getting cited by ChatGPT / Perplexity for "Claude Code rate limit" etc. |
| `mkt.content.strategy` | `marketing-content-strategy` | `marketing/content/calendar.md` — topic clusters |
| `mkt.content.pseo-vs-pages` | `marketing-programmatic-seo` | `marketing/content/pseo/` — template for `[Agent A] vs [Agent B]` pages |
| `mkt.content.blog-1` | `marketing-copywriting` | "Why your Claude Code session dies at hour 1 (and what to do about it)" |
| `mkt.content.blog-2` | `marketing-copywriting` | "Running 5 coding agents in parallel without corrupting your git tree" |
| `mkt.content.blog-3` | `marketing-copywriting` | "We treat human review as a state machine. Here's why." |
| `mkt.content.lead-magnet` | `marketing-lead-magnets` | "Multi-agent orchestration cheatsheet" PDF |
| `mkt.content.free-tool` | `marketing-free-tools` | Idea: Claude Code rate-limit calculator? ACP compatibility checker? |

---

## Phase 6 — Outbound (Parallel)

| Bead | Skill | Output |
|---|---|---|
| `mkt.outbound.cold-email-seq` | `marketing-cold-email` | `marketing/outbound/seq-tech-leads.md` (5-touch) |
| `mkt.outbound.ad-strategy` | `marketing-ads` | `marketing/ads/plan.md` (search: Claude Code alternative; meta retargeting) |
| `mkt.outbound.ad-creative` | `marketing-ad-creative` | `marketing/ads/creative/{google,x,linkedin}.md` — 10 variants each |
| `mkt.outbound.co-marketing` | `marketing-co-marketing` | `marketing/partners/shortlist.md` (Anthropic DevRel, ACP authors, beads maintainers, Kiro team) |

---

## Phase 7 — Community (Continuous)

| Bead | Skill | Output |
|---|---|---|
| `mkt.comm.social-calendar` | `marketing-social` | `marketing/social/calendar.md` (weekly X + LinkedIn cadence) |
| `mkt.comm.community-strategy` | `marketing-community-marketing` | `marketing/community/discord-plan.md` |
| `mkt.comm.referrals` | `marketing-referrals` | `marketing/community/referral-program.md` |

---

## Phase 8 — Measurement (Cross-Cutting)

| Bead | Skill | Output |
|---|---|---|
| `mkt.meas.analytics` | `marketing-analytics` | `marketing/measurement/tracking-plan.md` — install events, activation funnel, Pro upgrade |
| `mkt.meas.ab-program` | `marketing-ab-testing` | `marketing/measurement/experiment-backlog.md` (ICE-scored) |
| `mkt.meas.revops` | `marketing-revops` | `marketing/measurement/lead-lifecycle.md` (Community install → Pro lead → Team deal) |

---

## Dispatch Pattern (Spur-native)

Once Phase 1+2 artifacts are written, encoding the plan into Spur looks like:

1. Create epic in beads: `mkt.launch-campaign`.
2. Create child issues per row above with the right `good_for` tags so the brain routes correctly:
   - Long-form copywriting → Claude (best long-form prose).
   - SEO audits / structured data → Codex or Gemini (fast, structured output cheap).
   - Image / OG generation → out-of-band (Stitch/Midjourney) — link assets back.
   - Video script → Claude; production via separate Veo/Hyperframes pipeline.
3. `submit_plan` with phase 3 (Web) tasks first — they're all independent.
4. Each worker reads `marketing/product-marketing.md` (the foundation doc) before producing its asset — same way engineering workers read `AGENTS.md`.
5. Review cards in TUI / Telegram → approve/modify → artifacts land in `marketing/{phase}/...`.
6. PR-back is optional for marketing (most assets aren't code) — instead use beads "completed" + `marketing/` git commits.

---

## Skill Routing Map (worker → skill)

Workers don't auto-load `marketing-*` skills — they need to be told. Add to each task prompt:

```
Required skill: marketing-{name}
Foundation context: marketing/product-marketing.md
Output path: marketing/{phase}/{artifact}.md
```

Or set up `.claude/CLAUDE.md` in workers' contexts to auto-load the relevant skill based on task labels.

---

## What's NOT in scope here

- **Magister** (the autonomous CMO agent the repo author mentions): could be a phase-9 layer that drives this whole loop, but we should walk before we run.
- **Paid ad spend** before Phase 1 + 3 complete — no point driving traffic to undefined positioning.
- **Enterprise sales playbook** — Enterprise tier is "sales enablement only" today per PRD; defer.

---

## Suggested Order of Operations (next 2 weeks)

| Day | Action |
|---|---|
| Today | Review V1 of `marketing/product-marketing.md` ← **you are here** |
| Day 1 | Fill `[NEEDS INPUT]` sections (pricing, goals, verbatim language) |
| Day 2–3 | Run F2 (customer-research) + F3 (competitor-profiling) — partial parallel OK |
| Day 4 | Run F4 (pricing) + F5 (marketing-ideas) — sequential, you-in-loop |
| Day 5 | Run P1 + P2 (positioning) |
| Day 6 | Submit Phase 3 (Web) as a beads plan, dispatch in parallel |
| Day 7–10 | Review & iterate web assets |
| Day 11 | Submit Phase 4 (Launch) plan |
| Day 12+ | Launch + parallel content/outbound/community streams |
