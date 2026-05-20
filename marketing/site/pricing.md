# Pricing — SPUR

*Draft V1 — 2026-05-20. Structure follows `marketing/messaging/levers.md` Lever 2B (Pratfall-on-anti-claims): anti-claims first, cost-discrepancy framing second, price third. Anchored on artifacts that already exist in the repo — no countdowns, no founding-seat scarcity, no "limited-time launch pricing." If you want the conventional version of this page (price → features → caveats), every competitor in `marketing/competitors/` already wrote it. This is the reverse.*

---

## Before the price: what SPUR does **not** do

SPUR is a Rust-native orchestrator for AI coding agents. The list below is what it is **not**, written before the feature matrix on purpose. If any line below is a blocker for your team, you should know before reading further — not after a sales call.

- **No autonomous "set-and-forget" mode.** Every worker output passes through a human review gate. Approve / Reject / Modify / Retry is the state machine, not a UI convenience. If your goal is "assign tickets in Slack, get PRs back, no review surface," look at Devin instead — they own that job and own it well. We don't try to compete on it.
- **No SOC 2 or HIPAA at launch.** Both are on the Enterprise roadmap. They are not available in Community, Pro, or Team today. If your procurement requires either before evaluation, we're not ready for you yet.
- **No per-developer budget cap enforcement.** SPUR Team **surfaces** spend per developer, per repo, per week, across all five vendor extractors. It does not gate it. If your EM mandate is "stop spend over $X/dev/week automatically," SPUR Team will not satisfy that mandate today. We surface the gap; we do not close it for you. Gating spend well requires per-org policy that we treat as an Enterprise concern.
- **No native iOS or Android app.** Mobile review lives in Telegram — same state machine and event bus as the TUI, but it is Telegram, not an App Store binary. If your security posture excludes Telegram, the mobile lane is unavailable to you.
- **No fully autonomous merges from mobile.** Telegram review can Approve / Reject / Modify / Retry; it cannot authorize a destructive force-push or override branch protection from a phone. Final-stage merges require a TUI confirmation by design.
- **No public source repository.** SPUR is proprietary. The signed binary is distributed via `curl -sSL getspur.dev/install.sh | sh`. We may open-source select crates (telemetry, ACP client) over time; the orchestration core stays closed.
- **No PTY scraping, no terminal hijacking.** SPUR speaks native ACP + MCP to agents. If your agent does not speak ACP, it is not a worker today. (Most modern CLIs do; capability negotiation handles the rest.)
- **No on-prem / air-gapped deployment at launch.** Cosine Genie owns that segment. We have no air-gap story today.

If you got this far and none of the above is a deal-breaker: keep reading. Everything below is what we do build for, and what we charge for it.

---

## Cost-discrepancy framing

> Two engineers in the same VOC corpus described agent spend that diverged from what their finance team thought it was. **buremba** (HN 44598254): *"I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"* **roxolotl** (same thread, HN 44598254): *"A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month."*
>
> These are two HN comments, not a measured population statistic. We are citing them verbatim because they describe the shape of the problem SPUR's cost ledger was built for: the gap between perceived agent spend and actual agent spend is **discoverable per developer, per repo, per week** — and most teams discover it by accident, six weeks late, on a credit-card statement.
>
> SPUR Team is what you pay to stop discovering that gap by accident. The ledger reads vendor JSONL / SQLite in place across five extractors (Claude, Codex, Gemini, OpenCode, Kimi) via a DuckDB engine — no ETL, no shipping logs to a third-party SaaS. Accuracy band: within ~4 hours of vendor invoice across all five extractors today. If you find a delta bigger than that, file a ticket with the session ID and we will reproduce it.

---

## Tiers

| Capability | Community | Pro | Team | Enterprise |
|---|---|---|---|---|
| **Price** | $0 — no key required | **$19 / seat / mo** *(or $182 / yr, or $290 one-time)* | **$49 / seat / mo** *(min 3 seats; $470 / seat / yr)* | Contact sales *(est. $25k+/yr floor)* |
| Brain agent | 1 | 1 | 1 | Multi-brain with `spur:plan-owner` safety labels |
| Concurrent workers | 1 | Unlimited (subject to vendor rate limits) | Unlimited | Unlimited |
| Human review loop (Approve / Reject / Modify / Retry) | ✓ | ✓ | ✓ | ✓ |
| Live cross-vendor cost ledger (5 extractors, DuckDB engine) | ✓ — full display | ✓ | ✓ — plus per-developer / per-repo aggregation | ✓ — plus custom policy hooks |
| Event-sourced lineage (full plan replay) | ✓ | ✓ | ✓ | ✓ |
| Worktree-per-worker isolation | ✓ | ✓ | ✓ | ✓ |
| Any ACP-speaking agent (Claude Code / Codex / Gemini / Kimi / OpenCode / BYO) | ✓ | ✓ | ✓ | ✓ |
| Session resume via event replay (close laptop → resume exactly where it stopped) | — | ✓ | ✓ | ✓ |
| Brain-swap across vendors mid-flow (Claude → Codex → Claude) | — | ✓ | ✓ | ✓ |
| DAG-ordered cherry-pick of approved subtasks onto staging | — | ✓ | ✓ | ✓ |
| Reflexion retry (max 3, prior attempts as context) | — | ✓ | ✓ | ✓ |
| Telegram bot review lane | — | ✓ | ✓ | ✓ |
| Team cost dashboard (per-developer, per-repo, per-week aggregation) | — | — | ✓ | ✓ |
| RBAC + seat management | — | — | ✓ | ✓ |
| Webhooks (review-state transitions, plan completion) | — | — | ✓ | ✓ |
| SSO (SAML / OIDC) | — | — | — | ✓ |
| Audit log export | — | — | — | ✓ |
| Custom policy documents (Ed25519-signed) | — | — | — | ✓ |
| SOC 2 / HIPAA | — | — | — | On roadmap |
| Air-gapped / on-prem deploy | — | — | — | Roadmap — not at launch |

**Community is genuinely usable solo.** Full review loop, full cost display, full lineage, any ACP agent. The gates are parallelism, session resume, brain-swap, and team analytics — features that only matter once you've outgrown solo single-agent use. If Community fits you forever, it fits you forever. We'd rather you stay on Community for two years and tell one friend than churn off Pro in month two because you didn't need it yet.

---

## Pricing

### Community — $0

No license key. No signup. No credit card. The signed binary runs under our EULA. One brain, one worker, full review loop, full cost ledger, full event-sourced lineage. Survives crashes, OS updates, and network outages because plans live in beads (SQLite) and events live in NDJSON on your disk — not ours.

```
curl -sSL getspur.dev/install.sh | sh
spur init
```

### Pro — $19 / seat / month

Or $182 / seat / year (–20%). Or **$290 one-time, lifetime.**

The lifetime SKU is not a launch promotion. It maps to the `personal_lifetime` plan key that already ships in the license crate at `crates/spur-license/src/lib.rs:83` — a real, durable artifact you can verify in any signed binary. We are not running a countdown on it. We are not capping it at "the first 100 founding seats." It exists because the code already supports it. If we ever retire it, we will retire it for new buyers only and honor every existing lifetime license; that's what "lifetime" means.

Pro unlocks: unlimited concurrent workers (subject to vendor rate limits), session resume via event replay, brain-swap across vendors mid-flow, DAG-ordered cherry-pick onto staging, Reflexion retry, and the Telegram review lane.

### Team — $49 / seat / month

Or $470 / seat / year (–20%). Three-seat minimum.

Team adds the per-developer / per-repo cost dashboard, RBAC, seat management, and webhooks on review-state transitions and plan completion. Team is what you pay to stop discovering 5× cost gaps by accident.

Team **does not** enforce per-developer budget caps (see the anti-claim block above). It shows you the gap. Closing the gap is your call.

### Enterprise — contact sales

Estimated floor ~$25k / yr. Adds SSO (SAML / OIDC), audit log export, custom Ed25519-signed policy documents, multi-brain coordination with plan-owner safety labels, and a roadmap toward SOC 2 and HIPAA. Air-gapped / on-prem is on the roadmap; not available at launch.

Contact: `sales@getspur.dev`. We'd rather tell you "not yet" today than sell you a binder of promises.

---

## FAQ

**Is SPUR open source?**
No. SPUR is proprietary. The Community tier is free and genuinely useful, but the orchestration core's source is not public. We may open-source select crates (telemetry, ACP client) over time; the orchestration core stays closed. If "must be open source" is a hard requirement, Aider or one of the ACP reference clients in `marketing/competitors/` will serve you better than we will.

**What's in Community for free, exactly?**
One brain, one worker. The full human-review state machine (Approve / Reject / Modify / Retry). The full cross-vendor cost ledger across all five extractors. The full event-sourced lineage. Any ACP-speaking agent. Crash / OS-update / network-outage durability via beads + NDJSON. We did not gate any of the core safety primitives behind a paywall — that's a design choice, not a bug we forgot to fix.

**Can I downgrade from Pro / Team back to Community?**
Yes. Cancel any time. Your binary keeps working; the Pro-gated capabilities (parallelism, session resume, brain-swap, cherry-pick DAG, Telegram) stop at the end of the paid period. Plans already in beads stay in beads; you keep the data. We do not lock historical lineage behind the subscription.

**What's the refund policy?**
Monthly and annual subscriptions: full refund within 14 days of first payment, no questions asked. Lifetime Pro: full refund within 30 days. After that, lifetime is lifetime — see the Pro section above for why we treat that word literally.

**Why is Pro priced below Claude Code Max?**
Because SPUR is an *add-on* to the agents you already pay for, not a substitute. If Pro cost more than the underlying CLI, the math wouldn't work. The frame is "SPUR sits next to the agents you already chose" — pricing has to be consistent with that.

**Is the Team minimum really 3 seats?**
Yes. Two seats is two solo Pro users in a Slack channel; that doesn't need RBAC or a team dashboard. The features that justify Team's price (cost aggregation, role separation, webhooks on review state) only start mattering at three or more developers sharing one cost surface.

**What happens at vendor rate limits?**
You keep working. Brain-swap across vendors is the Pro-tier feature that handles this — hit a Claude Max weekly limit, your plan continues on Codex (or Gemini, or Kimi), and resumes on Claude when the window resets. The plan is the unit of durability, not the vendor session. Community users hit the rate limit and wait; Pro users don't.

**Can I bring my own agent?**
Any ACP-speaking agent works out of the box. Per-agent capability negotiation handles slash commands. If your agent does not yet speak ACP, it isn't a worker today.

**Do you have customer logos?**
Not yet. We are pre-launch and have not earned them. We would rather hold the empty space than stage one. See `marketing/product-marketing.md` for the launch-blocker on named-user quotes.

**What about Stripe / Paddle setup for the lifetime SKU?**
Honest answer: confirming that our billing provider can issue a one-time lifetime SKU under the `personal_lifetime` plan key is a launch-blocker on our side, not yours. The plan key is real in the code; the checkout integration is in progress. If you reach this page on launch day and the lifetime button is missing, that is the reason — and it will be back within the week.

---

*Pricing is in USD. EU VAT and similar are added at checkout. All Pro / Team / Enterprise licenses are issued as Ed25519-signed policy documents from the account dashboard — see `crates/spur-license/` if you want to read how that works before you buy.*
