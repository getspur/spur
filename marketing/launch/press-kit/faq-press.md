# SPUR — Press FAQ

The 10 questions we expect from reporters, with honest answers. These are not marketing answers — they're how the founder would answer if you caught them on a call. Use freely; quote freely.

---

### 1. How is this different from Devin?

Devin is an autonomous engineer you assign tickets to in Slack. It tries to do the work and come back when it's done. SPUR is the opposite: it assumes you want to keep your hands on the wheel, that the agent you use should change based on the task, and that you already pay for two or three CLI agents and want one place to coordinate them. Devin and SPUR are not competitors — they answer different questions. If you want autonomy, Devin is the better product. If you want a control tower over the agents you've already chosen, SPUR is.

### 2. Why proprietary, in a world of open-source dev tools?

Two reasons. First, the orchestration kernel is the moat — the cost ledger, the durable plan reconciler, the dual-channel protocol layer. Open-sourcing it would mean every vendor we integrate with could ship their own free version targeted at their own users, and we'd be left with the support burden and none of the leverage. Second, we want to be alive in three years. A proprietary core with a genuinely useful Community tier (no key required, full review loop, full cost display) is the path that gives us a business. We may open-source select crates over time — telemetry, the ACP client — but the kernel stays closed.

### 3. What's the team size?

TBD — founder to confirm. Honest answer: small. We are not pretending to be a 50-person company.

### 4. Funding?

TBD — founder to confirm. We will not give a fake number here. If we are bootstrapped, say bootstrapped. If we have raised, say the round and the lead.

### 5. How many customers do you have?

Zero paying customers as of this writing. SPUR is pre-launch. We have a Community binary that runs without a key and a small group of early testers; we have not yet opened paid tiers. The first thing we want from the launch is install counts, not revenue. By Day 60 we expect to have named-user quotes worth sharing; we don't have them yet, and we won't manufacture them.

### 6. Any plans to open-source?

The orchestration core: no, for the reasons above. Select supporting crates (telemetry, the ACP client): possibly. We won't commit to a timeline we can't keep. If a specific crate gets opened, we'll announce it on its own merits.

### 7. How do you handle privacy and data?

SPUR runs locally. Plans live in SQLite on your machine. Events log to NDJSON on your machine. Worker outputs land in your git tree. We do not see your code. Telemetry is opt-in (Tier 2, off by default) and covers crash and feature-usage signals only — no source, no diffs, no prompts. Cost extraction reads vendor JSONL/SQLite files in place; that data also never leaves your machine. The license check is a signed Ed25519 policy document — verification is offline once the policy is fetched. If we ever change any of this, we will say so on a clearly-dated page on the website.

### 8. What's the pricing rationale?

Pro at $19/month is deliberately below Claude Code Max at $100/month. SPUR is meant to sit *next to* the agents you already pay for, not replace them — so we priced it like an add-on. Team at $49/seat reflects shared cost dashboards, RBAC, and webhooks, and has a three-seat minimum because it's only useful at team scale. The $290 lifetime SKU is unusual; we kept it because there's a real audience of solo power-users who would rather pay once than subscribe forever, and we have the unit economics to support it. Enterprise pricing starts north of $25,000/year because that's where SSO, audit, and custom policy work actually starts to make sense.

### 9. What does success look like by year-end?

Year-end target metrics live in the product-marketing doc; the short version is: thousands of Community installs, hundreds of activated users, low hundreds of Pro conversions, and at least a handful of Team deals. The single metric we care about more than any of those is the share of weekly-active sessions that use two or more agent vendors. If that number is high, our thesis is right. If everyone using SPUR is using it as a Claude Code wrapper, we haven't built what we set out to build.

### 10. What could kill SPUR?

A few real risks, in rough order of how much they keep us up at night. **One:** A major model vendor ships a first-party orchestrator that's good enough — bundled free into an existing subscription. We don't think it'll be as good cross-vendor, but it doesn't have to be; it has to be good enough for most users. **Two:** The Community tier ends up so generous that it cannibalizes Pro, and we end up with installs but no revenue. **Three:** ACP stops being the de-facto protocol — a fragmentation that forces us to maintain N adapters instead of one. **Four:** We lose focus and try to be a Devin competitor or a Cursor competitor instead of staying in the lane we picked. We don't think any of these are inevitable, but we'd rather you ask us about them than read a press release that pretends they don't exist.
