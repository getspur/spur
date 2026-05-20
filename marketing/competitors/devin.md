# Devin (Cognition Labs)

*Profile date: 2026-05-20. INDIRECT competitor — cloud-hosted autonomous agent platform. Schema matches the F3 brief field list (the `competitor-profiling` SKILL.md assumes Firecrawl/DataForSEO MCPs not available in this worktree, so we use the practical field list from the task brief, consistent with `acpx.md`, `ralph.md`, etc.).*

## Identity

- **Name:** Devin (product) / Cognition Labs (company).
- **Official site:** https://devin.ai
- **Pricing page:** https://devin.ai/pricing
- **Docs:** https://docs.devin.ai
- **Launched:** 2024-03-12 as "the first AI software engineer" ([Cognition blog "Introducing Devin"](https://cognition.ai/blog/introducing-devin)).
- **Funding / valuation:** $400M round led by Founders Fund (Sep 2025), valuation ~$10.2B, up from ~$4B in March 2025 ([SiliconANGLE](https://siliconangle.com/2026/04/23/cognition-creator-ai-software-engineer-devin-talks-raise-hundreds-millions-25b-valuation/)).

## Headline pitch

> "Devin, the AI software engineer." — accelerates complex engineering tasks: code migrations, PR review, bug fixing, documentation. ([devin.ai homepage](https://devin.ai))

Positions itself as a **delegate-and-walk-away** autonomous engineer, not a pair-programmer. The user assigns a ticket via web app / Slack / Linear; Devin owns it end-to-end.

## Agent model

- **Multi-agent capable** at the *fleet* level: "spin up a team of Devins for large tasks" coordinating across repos ([devin.ai homepage](https://devin.ai)).
- Each Devin instance is a single autonomous agent with shell + editor + browser; the orchestration tier sits on Cognition's cloud, opaque to the user.

## Architecture

- **Cloud-hosted.** Devin runs in Cognition's infrastructure, accessed via web app, Slack, Linear, GitHub, MCP. Not a local CLI. ([devin.ai homepage](https://devin.ai))
- Integrations: GitHub, Linear, Slack, Datadog, Jira, plus Windsurf IDE (also Cognition-owned).

## Pricing

From [devin.ai/pricing](https://devin.ai/pricing) (2026-05-20):

| Tier | Price | Notes |
|------|-------|-------|
| Free | $0 | Limited Devin usage, Devin Review, DeepWiki |
| Pro | $20 / mo | Devin + Windsurf quota, pay-as-you-go overage, Slack/Linear/MCP |
| Max | $200 / mo | Higher Devin + Windsurf quotas |
| Teams | $80 / mo | Unlimited members, collab, centralized billing, admin analytics |
| Enterprise | Custom | SAML/OIDC, dedicated account team |

Usage-based on top of subscription; ACU ("Agent Compute Unit") consumption is not surfaced on the pricing page itself.

## Target persona

Enterprise engineering leadership wanting to **delegate large refactors / migrations** without per-engineer assignment. Marketing centers on engineering-org-wide modernization, not the IC's inner loop. ([devin.ai homepage](https://devin.ai))

## Adoption signals

- **Revenue:** ARR ~$1M (Sep 2024) → ~$73M (Jun 2025), per [SiliconANGLE coverage](https://siliconangle.com/2026/04/23/cognition-creator-ai-software-engineer-devin-talks-raise-hundreds-millions-25b-valuation/).
- **Marquee case study:** Nubank — 1,000+ engineers used Devin for migration; 100,000+ data-class implementations; claimed 8-12x efficiency and 20x cost savings on migration scope ([devin.ai homepage](https://devin.ai)).
- **Valuation:** $10.2B (Sep 2025).

## Top 3 strengths

1. **First-mover brand in autonomous-agent category.** "Devin" is shorthand for the whole concept of an AI software engineer. Massive earned media + enterprise sales motion.
2. **End-to-end ticket ownership.** Linear/Slack/GitHub assignment, multi-day session continuity, browser + shell + editor in the cloud sandbox — the user does not have to babysit a TUI. ([Cognition blog](https://cognition.ai/blog/introducing-devin))
3. **Enterprise distribution.** SAML/OIDC, audit, Teams plan, named account teams; already inside Nubank-scale orgs.

## Top 3 reasons a SPUR user would still want SPUR

1. **Local-first vs cloud-only.** Devin runs in Cognition's cloud — your repo, secrets, and shell live on their infra. SPUR's wedge persona (F2: DIY tmux + worktrees) explicitly chose *local* for IP, latency, and shell-tooling reasons. SPUR keeps that.
2. **Vendor-agnostic, not Anthropic/OpenAI-locked.** Devin runs Cognition's chosen models in Cognition's runtime. SPUR's brain-swap / multi-vendor failover (theme #1, `marketing/research/themes.md:7-19`) is *the* SPUR differentiator the moment the user hits a rate-limit window. You can't fail Devin over to Codex.
3. **Cost transparency.** Devin bills usage on top of subscription with no live cross-vendor ledger; SPUR's status-bar live spend (theme #2, `themes.md:23-34`) is precisely the visibility Devin doesn't offer because it doesn't need to (single-vendor billing).

## Notes for downstream positioning

- **Don't position SPUR as "Devin alternative for individuals."** That sounds like a downgrade. Position is *"the local orchestrator for people who don't want to outsource their repo to a SaaS."*
- Devin owns the **"hand it off, walk away"** JTBD. SPUR owns the **"keep me in the loop while my fleet runs"** JTBD (control tower, not autopilot).
