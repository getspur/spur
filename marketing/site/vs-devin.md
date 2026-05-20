# SPUR vs Devin

*Last updated: 2026-05-20. This page is a different-shapes-of-the-job comparison, not a head-to-head. Devin (Cognition Labs) is the category-defining autonomous AI software engineer — $73M ARR by Jun '25, 1,000+ engineers at Nubank running it for migrations at $10.2B valuation. SPUR is a local-first control tower for the CLI agents you already run. If you came here hoping for "Devin but cheaper," you're on the wrong page; the two tools are shaped for different jobs, and we'll show you which is which.*

---

## TL;DR

**Devin is the right answer if you want an autonomous engineer to assign Linear tickets to. SPUR is the right answer if you want a control tower over the agents you're already running locally.**

Both are real products with real users. Neither one is a degraded version of the other. The interesting question is which job you're trying to do — and below is the matrix to answer that honestly.

---

## What Devin owns, and why we don't try to compete

Devin's shape of the job, lifted straight from their homepage and case studies:

- **Delegate-and-walk-away.** You file a ticket in Linear, tag it `@Devin`, and Devin owns it end-to-end: shell, browser, editor, multi-day session continuity, PR opened against your repo.
- **Cloud-hosted by Cognition.** The sandbox, the model stack, and the orchestration tier all sit on Cognition's infrastructure. You don't run anything locally.
- **Enterprise distribution.** SAML/OIDC, audit log, Teams and Enterprise tiers, named account teams. Nubank put 1,000+ engineers on it for a migration with 100,000+ data-class implementations, claimed 8–12x efficiency and 20x cost savings on that scope.
- **First-mover brand.** "Devin" is shorthand for the whole autonomous-engineer category. $1M ARR in Sep '24 to $73M ARR by Jun '25 is the proof point that the category is real.

If your job-to-be-done is *"give me an autonomous engineer I can hand tickets to in Slack and Linear,"* Devin is the right answer. SPUR does not try to be that — by design, SPUR keeps a human in the loop on every merge. That's a different product, not a worse one.

---

## What SPUR is shaped for, and why Devin doesn't try to be it

SPUR's shape of the job:

- **Local-first.** Workers run in worktrees on your laptop. Your repo, your secrets, your shell. Nothing leaves your machine unless you push it.
- **Multi-vendor by design.** SPUR dispatches into Claude Code, Codex, Gemini, GLM, Kimi, OpenCode — whichever CLI you already pay for. The brain can swap mid-flow when one vendor rate-limits you.
- **Human-in-the-loop on merge.** Every worker output lands in a review queue. You (or a phone-side approver) gates the cherry-pick onto the staging branch. There is no "Devin opened a PR while you were asleep" mode.
- **One cost ledger across every agent.** SPUR aggregates spend across vendors by reading their JSONL/SQLite logs in place — so you see today's total bill across Claude + Codex + Gemini + Kimi + OpenCode in one number.

If your job-to-be-done is *"keep me in the loop while a fleet of CLI agents runs on my laptop,"* SPUR is the right answer. Devin does not try to be that — Devin's whole value proposition is *removing* you from the loop, not surfacing it.

---

## Decision matrix — use Devin if, use SPUR if

A concrete table, written for the developer or engineering leader deciding which one (or both) to adopt.

| Your situation | Use Devin | Use SPUR | Notes |
|---|---|---|---|
| You want an engineer-as-a-service to assign Slack/Linear tickets to | ✅ | ❌ | This is Devin's category. SPUR doesn't try. |
| You want to walk away from the laptop and come back to a finished PR | ✅ | ❌ | SPUR's review gate is the opposite design. |
| You're modernizing 1,000-engineer-scale legacy code (the Nubank shape) | ✅ | ❌ | Devin has the case study. SPUR has no enterprise migration motion at α. |
| You run 5–10 CLI agents on your laptop and forget which is waiting on you | ❌ | ✅ | SPUR's wedge persona. |
| You hit Claude Pro/Max rate limits and want to keep working on Codex / Gemini / GLM | ❌ | ✅ | Devin is single-vendor by design. SPUR's brain-swap is the differentiator. |
| You pay multiple model vendors and can't see one total bill | ❌ | ✅ | Devin only sees its own usage. SPUR's cross-vendor ledger is built for this. |
| You closed the laptop and lost two hours of agent context | ❌ | ✅ | SPUR's beads + NDJSON event log resumes via replay. |
| You want your repo to stay on your laptop, not Cognition's cloud | ❌ | ✅ | SPUR is local-first by design. |
| You want SAML / SSO / audit on day one | ✅ | ❌ | SPUR community / pro don't ship SAML — that's reserved for Team / Enterprise post-α. |
| You're a solo dev with one agent at a time | ❌ | ❌ | Neither tool earns its keep at fleet size 1. Stay on Claude Code or Aider. |

**The honest rule:** Devin wins when you want the engineer outsourced. SPUR wins when you want the orchestration surfaced. They are different shapes of the same broad space.

---

## Shared use cases neither tool fights for

A short list of things both tools could conceivably address — but in practice each one has chosen not to. Worth naming so the reader can see where the real overlap is (or isn't).

### Cross-vendor failover

Devin runs Cognition's chosen models in Cognition's runtime. There is no "fail Devin over to Codex" path — and Cognition has no commercial reason to build one, because it would dilute the single-vendor billing model that funds the $10.2B valuation. SPUR's whole brain-swap layer exists in this gap.

### Local-first durability

Devin's session lives in Cognition's cloud, scoped to Cognition's session model and Cognition's outage envelope. SPUR's plans live on your laptop in beads + NDJSON, replay on restart, and survive a network drop. Different product, different durability surface.

### Unified cost across multiple agents

Devin bills Devin usage. It cannot, by design, tell you what your Claude Code + Codex + Gemini bill totals — those are competitors' meters. SPUR aggregates them because it reads each vendor's local logs in place. This is the differentiator no single-vendor product can match without becoming a multi-vendor product.

None of these are gaps in Devin to attack — they're scope choices that fall outside Devin's product shape. SPUR is the tool shaped for those choices.

---

## A side-by-side that doesn't pit the two against each other

This matrix deliberately avoids the dimensions where one would obviously beat the other — autonomy-on-a-ticket (Devin wins) and local-fleet-coordination (SPUR wins). Instead it surfaces the genuinely different shapes.

| Dimension | Devin | SPUR |
|---|---|---|
| Surface | Web app, Slack, Linear, GitHub | Local terminal (TUI), Telegram for mobile review |
| Locality | Cloud only | Local-first, no required backend |
| Agent model | Single autonomous Devin instances, fleet-coordinated in Cognition cloud | Heterogeneous worker fleet across CLI vendors, human-in-loop merge |
| Model lock-in | Cognition's stack | None — brain-swap across Claude, Codex, Gemini, GLM, Kimi, OpenCode |
| Billing | Subscription + ACU usage in Cognition cloud | Subscription on top of whatever vendor bills you already pay |
| Human in the loop | Optional — designed for delegate-and-walk-away | Mandatory at merge — designed for review-gate-and-watch |
| Distribution | Pro $20 / Teams $80 / Enterprise custom; SAML/OIDC | `cargo install spur-cli`, additive to whatever CLIs you already pay for |
| Pricing for individuals | Free + Pro $20/mo | Community free tier; Pro for ledger + Telegram |
| Best persona | Eng leadership delegating large refactors | Sr/Staff IC already juggling 5–10 worktrees |

If you read across the rows, the two products barely overlap in surface area. That's deliberate.

---

## Who's best for whom

**Hire Devin if:**
- You want an autonomous engineer that owns tickets in Slack/Linear end-to-end.
- You're running an org-wide modernization at 100s–1,000s of engineers (the Nubank shape).
- You're OK with — or actively want — your repo and shell living in Cognition's cloud sandbox.
- Single-vendor billing in exchange for autonomy is the right trade for you.

**Install SPUR if:**
- You run 5–10 CLI agents at a time across worktrees and need a control tower (the canonical SPUR wedge persona).
- You've been ambushed by Claude Pro/Max rate limits mid-sprint and want a cross-vendor fail-over path.
- You pay multiple model vendors and need one ledger across all of them.
- You want your repo and shell to stay on your laptop, not in someone else's cloud.

**Honestly, you might want both:**
- Devin for the cross-cutting migration the org is paying for at the enterprise level.
- SPUR on your laptop for the day-to-day fleet of local CLI agents your team uses for normal work.

These aren't the same product; using both is not a contradiction.

---

## Open question we owe the reader

The watch-list item, for honesty: if Cognition ships a local-runtime mode, a multi-vendor brain, and a cross-vendor cost ledger, the surface area between SPUR and Devin would shrink. Today none of those are on Devin's public roadmap — and none of them are commercially obvious moves for a single-vendor cloud product valued at $10.2B. If they ship, we'll update this page; until then, the two products are shaped for genuinely different jobs.

---

## CTA

If you're a Slack/Linear ticket-eater shop: **try Devin.** Devin is the right tool for that shape of the job, and SPUR does not try to compete with it.

If you're a local-fleet operator: **try SPUR.**

- `cargo install spur-cli` — installs the control tower next to whatever agents you already run
- 60-second demo: Claude Code rate-limit fail-over to Codex, plan resumes intact
- Docs: how SPUR dispatches into Claude Code / Codex / Gemini as workers

---

*Source files: `marketing/competitors/devin.md`, `marketing/competitors/_summary-indirect.md:13,21-26,32,56-67`, `marketing/messaging/positioning.md:17,94-96`, `marketing/research/themes.md` (themes #1–#3), `marketing/product-marketing.md:8,97`. Adoption figures (Devin $73M ARR Jun '25, Nubank 1k engineers, $10.2B valuation) cited from Devin's own homepage and SiliconANGLE coverage — used as proof Devin is real, not as ammunition.*
