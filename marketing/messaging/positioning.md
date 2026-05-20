# SPUR Positioning — V1

*2026-05-20. Phase-2 deliverable. Built from `marketing/product-marketing.md` V1.1, `marketing/research/themes.md`, `marketing/research/voc.md`, and the indirect-competitor profiles in `marketing/competitors/`. Every claim below cites a source already on `main`. Hero candidates and the persona matrix are written for A/B testing — do not collapse to one before validation.*

---

## Strategic frame

**Category we own (verbatim from VOC):** *Control tower for your CLI agents.*

Adopted directly from Beefin (Amux author), HN item 47104424, captured in `marketing/research/voc.md:93` and surfaced as the canonical phrasing in `marketing/competitors/_summary-indirect.md:42`. "Multi-agent orchestrator" is rejected — Cosine already uses it (`marketing/competitors/_summary-indirect.md:42`).

**What this positioning is NOT:**

- Not a Claude Code replacement — SPUR runs Claude Code as a worker (`marketing/product-marketing.md:8`).
- Not a Cursor / Aider competitor — those are in-session tools; SPUR sits above them (`marketing/competitors/_summary-indirect.md:32-36`).
- Not a Devin alternative for "autonomous engineer in Slack" — SPUR explicitly requires human review (`marketing/product-marketing.md:97`, `marketing/competitors/_summary-indirect.md:32`).

---

## Value-proposition matrix — per persona

Three personas, lifted from `marketing/product-marketing.md:40-44`. For each: the load-bearing pain (with verbatim VOC source), the SPUR capability (with PRD citation), the proof point, and an anti-claim — what we deliberately do NOT promise.

### Persona 1 — The Orchestrator (Sr/Staff eng, tmux-native, $200–600/mo AI spend)

| | Detail |
|---|---|
| **Top pain (VOC)** | *"I run 5-10 Claude Code agents at a time across different repos. Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos. I needed a control tower."* — Beefin, HN 47104424 (`marketing/research/voc.md:88-94`) |
| **Secondary pain (VOC)** | *"Yes, worktrees with workmux. I expected this to become less necessary over time as models got faster, but the opposite has happened."* — nojs, HN 47573483 (`marketing/research/voc.md:118-120`) |
| **SPUR capability** | Native ACP + MCP dual channel with structured review queue; worktree-per-worker with DAG-ordered cherry-pick onto a staging branch (`marketing/product-marketing.md:74-81`). |
| **Proof point** | Brain-swap mid-flow: a Claude rate-limit hand-off to Codex is impossible inside any of Devin / Cosine / Cursor / Aider / Claude Code (`marketing/competitors/_summary-indirect.md:21-26, 56-67`). Session resume via event replay (`marketing/product-marketing.md:78`). |
| **Anti-claim — what we do NOT promise** | We do not replace tmux. The wedge is *"you already built half of this in tmux — let us finish the other half"* (`marketing/research/themes.md:88`). We do not promise zero configuration of new agents — capability negotiation handles slash commands per-agent (`marketing/product-marketing.md:79`), but you bring the agents. |

### Persona 2 — The Team Lead (EM over 3–10 devs)

| | Detail |
|---|---|
| **Top pain (VOC)** | *"A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month."* — roxolotl, HN 44598254 (`marketing/research/voc.md:54-56`) |
| **Secondary pain (VOC)** | *"Extremely infuriating because if I could have a view into how close I was to being rate limited."* — gorbypark, HN 44713757 (`marketing/research/voc.md:58-60`) |
| **SPUR capability** | Five live cost extractors (Claude, Codex, Gemini, OpenCode, Kimi) feeding a DuckDB analytics engine that reads vendor JSONL/SQLite in place — no ETL (`marketing/product-marketing.md:170-174`). |
| **Proof point** | Unified ledger is the one differentiator no competitor can match by design — Devin, Cosine, Cursor, Aider, and Claude Code each only see their own bill (`marketing/competitors/_summary-indirect.md:25, 44`). |
| **Anti-claim — what we do NOT promise** | We do not promise SOC2 or HIPAA at launch (`marketing/product-marketing.md:102`). We do not promise RBAC or SSO in Community or Pro — those are Team/Enterprise (`marketing/product-marketing.md:22`). We do not promise to enforce a per-developer budget cap (we surface spend; we don't gate it). |

### Persona 3 — The Mobile Operator (dev away from desk)

| | Detail |
|---|---|
| **Top pain (VOC)** | *"I've been using this to be productive all day on my phone."* — Beefin, HN 47104424 (`marketing/research/voc.md:136-138`). Read as a pull, not a push: the absence of a mobile review surface is the implicit pain in the rest of the corpus. |
| **Secondary pain (VOC)** | *"I closed the terminal and lost two hours of agent work."* — provisional verbatim (`marketing/product-marketing.md:120`). **NOTE: this is provisional, not yet validated against Phase-1 VOC sweep.** Listed for completeness; mark as needs-confirmation. |
| **SPUR capability** | Telegram bot shares the same review lane and event bus as the TUI — same state machine, not a parallel surface (`marketing/product-marketing.md:80`). |
| **Proof point** | Review gate is a first-class state machine with timeout / retry / merge gating (`marketing/product-marketing.md:77`), and survives crash / OS update / network outage via beads + NDJSON (`marketing/product-marketing.md:171`). |
| **Anti-claim — what we do NOT promise** | We do not promise a web dashboard at launch — mobile is Telegram. We do not promise an iOS/Android native app. We do not promise to authorize destructive merges from mobile without a TUI confirmation (review gate stays strict). |

---

## Hero-section candidates — three, for A/B testing

Each grounded in a different top theme from `marketing/research/themes.md`. Same product, three entry points.

### Hero A — Cost ledger (theme #2, `marketing/research/themes.md:21-34`)

> **Headline:** See what you'd be billed today, across every agent, in one number.
> **Subhead:** SPUR is the control tower for your CLI coding agents. One live cost ledger across Claude, Codex, Gemini, Kimi, and OpenCode — so you stop discovering $1k weeks by accident.
> **Primary CTA:** `cargo install spur-cli`
> **Secondary CTA:** See a live ledger demo

*Source line: headline lifted from `marketing/research/themes.md:33` synthesis #2 ("cost opacity is the sharpest emotional language in the batch"). Subhead grounds the "$1k week" in the roxolotl quote (`marketing/research/voc.md:54-56`).*

### Hero B — Control tower (theme #3, `marketing/research/themes.md:36-48`)

> **Headline:** The control tower for your CLI coding agents.
> **Subhead:** Dispatch Claude Code, Codex, Gemini, and Kimi in parallel. Review every diff in one place. Cherry-pick what merits merging. Walk away — your plan survives the closed laptop.
> **Primary CTA:** `cargo install spur-cli`
> **Secondary CTA:** Watch the 60-second demo

*Source: "control tower" verbatim from Beefin (`marketing/research/voc.md:93`). "Walk away — plan survives" cites beads durability (`marketing/product-marketing.md:171`). Cherry-pick capability from `marketing/product-marketing.md:81`.*

### Hero C — DIY wedge (theme #4 + synthesis #3, `marketing/research/themes.md:50-65, 88`)

> **Headline:** You already built half of this in tmux. Let us finish the other half.
> **Subhead:** Keep your worktrees. Keep your agents. SPUR adds the one thing your shell can't: a durable plan, a review queue, and a live cost ledger across every vendor you already pay.
> **Primary CTA:** `cargo install spur-cli`
> **Secondary CTA:** Read the README

*Source: framing lifted verbatim from `marketing/research/themes.md:88` synthesis #3. "Keep your worktrees" defuses anti-pattern #4 (`marketing/research/themes.md:54-63`). Durable plan + review gate + cost ledger from `marketing/product-marketing.md:74-81, 170-174`.*

---

## Positioning-against — peers, not competitors

One paragraph per peer per `marketing/competitors/_summary-indirect.md:43` action item #2. Each paragraph names what the peer owns, what SPUR owns next to it, and the explicit no-fight stance.

### Devin (Cognition)

Devin owns *"give me an autonomous engineer I can assign tickets to in Slack"* — $73M ARR by Jun '25, 1k engineers at Nubank (`marketing/competitors/_summary-indirect.md:65`). It is cloud-only, single-vendor, and opaque to your local repo (`marketing/competitors/_summary-indirect.md:13`). SPUR does not compete: SPUR keeps the human in the loop on purpose (`marketing/product-marketing.md:97`) and lives in the developer's terminal next to their existing agents. If you want a Slack-native ticket-eater, hire Devin. If you want a control tower over the agents you already run, install SPUR.

### Cursor

Cursor owns the AI IDE — inline diffs, Composer chat, multi-file edits across 50% of the Fortune 500 and 40k engineers at NVIDIA (`marketing/competitors/_summary-indirect.md:65`). Most SPUR users keep Cursor open in another window (`marketing/competitors/_summary-indirect.md:34`). SPUR is not an editor. SPUR is the layer that takes the worktree your agent produced, queues it for review, and cherry-picks the approved diff onto your branch. Cursor edits a file; SPUR coordinates a fleet of agents editing many files in parallel.

### Aider

Aider owns the simplest, free, BYO-key single-agent CLI — 45k GitHub stars, 6.8M PyPI installs, 15B tokens / week (`marketing/competitors/_summary-indirect.md:65`). Aider is a pair-programmer by design; it does not try to be a fleet manager (`marketing/competitors/_summary-indirect.md:18, 35`). SPUR's value materializes at fleet size ≥ 2 (`marketing/competitors/_summary-indirect.md:35`). The right framing: Aider users are SPUR's warmest leads (terminal-native, BYO-key, polyamorous about models) and scoping Aider-as-a-SPUR-worker is on the roadmap (`marketing/competitors/_summary-indirect.md:46`). Don't switch from Aider; add SPUR above it.

### Claude Code

Claude Code owns the best in-session single-agent coding experience with Anthropic's models — it is the source of the entire VOC corpus in `marketing/research/voc.md` (`marketing/competitors/_summary-indirect.md:65`). SPUR uses Claude Code as a worker (`marketing/product-marketing.md:8`) and explicitly does not compete on in-session UX (`marketing/competitors/_summary-indirect.md:36`). What SPUR adds: cross-session durability (close the laptop, plan survives), cross-vendor failover (rate-limit Claude → keep working on Codex, then come back), and a cost ledger that spans every CLI you run — none of which Claude Code attempts (`marketing/competitors/_summary-indirect.md:19, 23-26`).

---

## Words to use / words to avoid

Grounded in verbatim VOC and the brand-voice section of `marketing/product-marketing.md:162-165`.

| Use | Avoid | Why |
|---|---|---|
| "Control tower" | "Multi-agent orchestrator" | Beefin verbatim (`voc.md:93`); Cosine occupies "orchestrator" (`_summary-indirect.md:42`). |
| "Cost ledger" / "one number" | "Cost optimization" / "cost intelligence" | Concrete vs. vague. `themes.md:33` shows the sharpest pain is *not knowing*, not *paying too much*. |
| "Brain-swap" / "fail Claude to Codex" | "Failover" / "high availability" | Developer-native, specific to the pain (`voc.md:147-149`). Enterprise jargon flagged in `product-marketing.md:142`. |
| "Worktree per worker" | "Sandboxed execution" | Power users already say "worktree" (`voc.md:102, 114, 118`); "sandbox" reads as marketing. |
| "Issue in, PR out" | "End-to-end automation" | One-liner from `product-marketing.md:5`, mirrors the VOC frame. |
| "Cherry-pick approved diffs" | "Smart merge" / "intelligent integration" | Specific git operation per `product-marketing.md:81`. |
| "Locked out" / "weekly cap hit" | "Throttled" / "usage governance" | Verbatim from esperent + TheOtherHobbes (`voc.md:21, 44`). |
| "Closed the laptop" / "lost two hours" | "Session persistence" | Developer voice; "session persistence" is brochure-ware. |
| "Review card" / "approve / reject / retry" | "Human-in-the-loop AI" | Concrete UI nouns from `product-marketing.md:154`; the abstract phrase is buzzwordy. |
| "Bring your own agent" / "any ACP-speaking agent" | "Vendor-agnostic platform" | "Platform" is on the brand-voice avoid list (`product-marketing.md:143`). |
| "Rate-limit-proof" | "Always-on AI" | Folk phrasing already in `product-marketing.md:128`; "always-on" overpromises. |
| "Show" cost / lineage / review state | "Dashboard" / "observability" | Verb beats noun; dashboards are commodity. |
| Plain "we" / "you" | "AI-powered", "autonomous", "revolutionary", "next-gen" | Per `product-marketing.md:141-147` brand-voice prohibitions. |
| "Survives crash / OS update / network outage" | "Enterprise-grade reliability" | Specific claim from `product-marketing.md:171`; the bland version forfeits proof. |

---

## Summary — recommendations for the caller

### (a) Recommended primary A test

**Hero A (cost ledger).** Three converging reasons: (1) `themes.md:33` synthesis #2 explicitly flags cost opacity as the *sharpest emotional language* in the batch, even though rate-limit has more total quotes; (2) `_summary-indirect.md:44` action item #3 calls out the unified ledger as **the one differentiator no peer can address by design** — every other peer is single-billing; (3) Hero A is the candidate most aligned with the V1.1 PRD's stated proof points (`product-marketing.md:170-174`) — five live extractors + DuckDB already shipped. Hero B (control tower) is the strongest A/B contender and should be the secondary test. Hero C (DIY wedge) is the warm-leads test against the existing tmux-native audience and is most useful for an HN Show post rather than the cold-traffic homepage.

### (b) Riskiest claim that needs validation before launch

**"See what you'd be billed today, across every agent, in one number"** (Hero A subhead). Risk: the five extractors (`product-marketing.md:171`) parse vendor-side billing surfaces (Claude / Codex / Gemini / OpenCode / Kimi), but each vendor's billing surface lags real usage by an unknown amount and uses inconsistent units (input vs. output tokens, cache reads vs. cache writes, per-vendor model pricing tables). A claim of *"today"* implies near-real-time aggregate accuracy. Before launch we need: (i) ground-truth comparison of each extractor's number vs. the vendor's own invoice for the same week, on at least one heavy user; (ii) explicit copy disclosing the lag if it's >2 hours for any vendor. If accuracy is not within a defensible band, the headline becomes a liability the moment the first power-user reverse-engineers it (as buremba did to Anthropic, `voc.md:50-52`).

### (c) Revisions recommended to `product-marketing.md` V1.1

Three, all surfaced by Phase-1:

1. **§ Customer Language (`product-marketing.md:112-129`) is flagged as provisional pending F2.** F2 is now done. Replace the provisional verbatim block with the real Beefin / roxolotl / nojs / esperent / gorbypark quotes from `voc.md` and drop the *"Status: Provisional"* preamble.
2. **§ Differentiation (`product-marketing.md:73-86`) lists ten differentiators flat.** Reorder so **unified cost ledger** is #1 and **brain-swap across vendors** is #2, per `_summary-indirect.md:44`. Today the list opens with "Rust single binary" — that's a credibility token, not the hero claim, and burying the cost ledger in position #2 of the proof-points table (`product-marketing.md:182`) undersells the one moat no peer can copy.
3. **§ Competitive Landscape (`product-marketing.md:56-71`) currently buckets Devin/Cosine as a single "cloud agent platforms" line under indirect.** Phase-1 produced full profiles for Devin, Cosine, Cursor, Aider, and Claude Code in `marketing/competitors/` with explicit peers-not-competitors framing in `_summary-indirect.md`. Update this section to link to each profile and adopt the peers-not-competitors stance from `_summary-indirect.md:43` — the current language ("Falls short: ...") picks a fight SPUR cannot win on autonomous-engineer or in-IDE persona and erodes the credibility of the differentiators that *do* hold up.
