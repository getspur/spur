# SPUR Homepage — Copy

*Phase-3 deliverable. 2026-05-20. Primary hero: Hero A (cost ledger), per `marketing/messaging/positioning.md:137-139`. Every claim cites a source already on `main`: VOC quotes by HN item ID, `product-marketing.md` line ranges, license-crate paths, or competitor profiles. Brand-voice prohibitions enforced per `marketing/product-marketing.md:148-154` — no "AI-powered", no "platform", no "autonomous", no "revolutionary", no "open source" framing.*

*Domain `getspur.dev` is provisional pending the launch-blocker on secure checkout (`product-marketing.md:209`) — flagged here once, used unflagged in the rest of the file.*

---

## 1. Hero — Hero A (cost ledger, primary)

**Headline**

> See what you'd be billed today, across every agent, in one number.

**Subhead**

> SPUR is the control tower for your CLI coding agents. One live ledger across Claude, Codex, Gemini, Kimi, and OpenCode — so you stop discovering $1k weeks by accident.

**Accuracy band — inline, not buried** *(Lever 2A mitigation, `levers.md:145, 192`; addresses the Riskiest Claim at `positioning.md:141-143`)*

> Numbers reconcile to each vendor's own invoice within a documented lag per extractor. Claude / Codex / Gemini / OpenCode / Kimi read JSONL or SQLite the vendor already writes to disk — no proxying your traffic, no second account. If your invoice and SPUR disagree by more than the published band, that's a bug, not a feature.

**Primary CTA**

```
curl -sSL https://getspur.dev/install.sh | sh
```

Button label: **Install SPUR** *(signed Rust binary, no Node, no Python, no Docker — `product-marketing.md:10, 90`)*

**Secondary CTA**

> Watch the 60-second fail-Claude-to-Codex demo → `[video-placeholder]`

---

## 2. Problem — four pains, in their own words

*All quotes verbatim from `marketing/research/voc.md`. Reviewers MUST spot-check at the source HN item before publish per `voc.md:9`.*

### Pain 1 — You hit the weekly cap on day three

> *"Paying $200 a month, I hit my weekly in 3 days last week."*
> — esperent, HN 47626833 (`voc.md:20-21`)

You bought Claude Code Max for the headroom. You're locked out by Thursday with four days of subscription you can't spend. *"You're locked out of the service … while still paying your subscription … It's ridiculous."* — TheOtherHobbes, HN 44713757 (`voc.md:44-46`).

### Pain 2 — You don't know what you're actually spending

> *"I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"*
> — buremba, HN 44598254 (`voc.md:50-52`)

> *"A coworker of mine claimed they've been burning $1k a week this month. Pretty wild it's only costing the company $200 a month."*
> — roxolotl, HN 44598254 (`voc.md:54-56`)

Two engineers in the same corpus described agent spend that was roughly 5× what their finance team thought it was (`levers.md:62-63`). The gap is the problem; the dollar figure is incidental.

### Pain 3 — Parallel agents collide on the same worktree

> *"Yes, worktrees with workmux. I expected this to become less necessary over time as models got faster, but the opposite has happened."*
> — nojs, HN 47573483 (`voc.md:118-120`)

Worktrees got you to five parallel agents. The coordination tax compounds faster than the models do.

### Pain 4 — Context dies the moment you close the terminal

> *"I run 5-10 Claude Code agents at a time across different repos. Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos. I needed a control tower."*
> — Beefin (Amux author), HN 47104424 (`voc.md:88-94`)

Close the laptop and you lose the plan, the lineage, and the half-finished review. SPUR doesn't.

---

## 3. How it works — three steps

```
   ┌─────────────┐      ┌──────────────────────┐      ┌─────────────────┐
   │  Issue in   │ ───▶ │ Workers in parallel  │ ───▶ │ Review surface  │
   │             │      │                      │      │                 │
   │ submit_plan │      │  Claude · Codex      │      │ Approve / Reject│
   │ (beads SQL) │      │  Gemini · Kimi       │      │ Modify / Retry  │
   │             │      │  OpenCode            │      │ Cherry-pick DAG │
   │             │      │  one worktree each   │      │ → staging branch│
   └─────────────┘      └──────────────────────┘      └─────────────────┘
```

**Issue in.** A brain agent reads the task and submits a DAG of subtasks (`product-marketing.md:8, 162`). The plan lives in beads (SQLite), so it survives crashes, OS updates, and network outages (`product-marketing.md:81, 171`).

**Workers in parallel.** Each subtask runs in its own git worktree under `spur/worker/v2/{agent}/...` (`product-marketing.md:167`). Any ACP-speaking agent works — Claude Code, Codex, Gemini, Kimi, OpenCode, or your own (`product-marketing.md:79, 102`).

**Review surface.** Approve, reject, modify, or retry each completed attempt. Approved diffs cherry-pick in DAG order onto a staging branch (`product-marketing.md:84`). Same state machine in the TUI and on Telegram (`product-marketing.md:86`).

---

## 4. Capabilities — top 5, ordered by uniqueness

*Order matches `product-marketing.md:78-90` Differentiation list — uniqueness, not chronology. "Rust single binary" is a credibility token, not the lede (`product-marketing.md:90`), and lives in the install footer.*

### 1. Unified cost ledger across every vendor

Five live extractors (Claude, Codex, Gemini, OpenCode, Kimi) feed a DuckDB engine that reads vendor JSONL/SQLite in place — no ETL, no proxy (`product-marketing.md:79, 181`). The one moat no peer can close by design: Devin, Cosine, Cursor, Aider, and Claude Code each only see their own bill (`marketing/competitors/_summary-indirect.md:25, 44`).

### 2. Brain-swap across vendors mid-flow

Hit a Claude rate limit, keep working on Codex, come back to Claude when the window resets. Not session-pause-and-resume — full cross-vendor failover (`product-marketing.md:80`). Impossible inside any single-vendor tool.

### 3. Local-first durability

Plans in SQLite (beads). Events in NDJSON. Outcomes in git blobs. Survives crashes, OS updates, network outages (`product-marketing.md:81, 171`). Close the laptop; the plan is still there.

### 4. Human review as a first-class state machine

Approve / reject / modify / retry with timeout and merge gating — not a UI convenience layered over a chat (`product-marketing.md:82`). The review gate is the load-bearing state machine, not a polish layer.

### 5. Session resume via event replay

Restart SPUR, the brain replays the event log, and you pick up exactly where you left off (`product-marketing.md:83`). Not soft-reconnect — full replay.

---

## 5. Peers, not competitors

*One paragraph each, per `positioning.md:90-108`. SPUR does not pitch as a head-to-head replacement for any of these.*

### Devin

Devin owns *"give me an autonomous engineer I can assign tickets to in Slack"* — $73M ARR by Jun '25, 1k engineers at Nubank (`marketing/competitors/_summary-indirect.md:65`). It is cloud-only, single-vendor, opaque to your local repo. SPUR does not compete: SPUR keeps the human in the loop on purpose (`product-marketing.md:105`) and lives in the developer's terminal next to the agents they already run. If you want a Slack-native ticket-eater, hire Devin. If you want a control tower over the agents you already use, install SPUR.

### Cursor

Cursor owns the IDE — inline diffs, Composer chat, multi-file edits across 50% of the Fortune 500 (`_summary-indirect.md:65`). Most SPUR users keep Cursor open in another window (`product-marketing.md:69`). SPUR is not an editor. SPUR takes the worktree the agent produced, queues it for review, and cherry-picks the approved diff onto your staging branch. Cursor edits a file; SPUR coordinates a fleet of agents editing many files in parallel.

### Aider

Aider owns the simplest free BYO-key single-agent CLI — 45k GitHub stars, 6.8M PyPI installs, 15B tokens / week (`_summary-indirect.md:65`). Aider is a pair-programmer by design; it does not try to be a fleet manager. SPUR's value materializes at fleet size ≥ 2. Don't switch from Aider — add SPUR above it (`product-marketing.md:70`).

### Claude Code

Claude Code owns the best in-session single-agent experience with Anthropic's models — it is the source of the entire VOC corpus on this page. SPUR runs Claude Code as a worker (`product-marketing.md:8`) and explicitly does not compete on in-session UX. What SPUR adds: cross-session durability (close the laptop, the plan survives), cross-vendor failover when Claude rate-limits you, and a cost ledger that spans every CLI you run — none of which Claude Code attempts (`product-marketing.md:71`).

---

## 6. Social proof — placeholder

<!--
NO logo wall, NO trusted-by-strip until product-marketing.md launch-blocker on
3-5 named-user quotes clears (see product-marketing.md:183, levers.md:78-82,
195). Per Anti-lever 2A (levers.md:78): "self-aware emptiness outperforms fake
fullness for this persona." Hold this space. Do not fill with stock logos,
unearned 'as seen on HN' tropes, or padded press mentions. Replace only when
3-5 named quotes from the first 50 Community installs have landed via the
`spur feedback` command + 2-question post-install survey planned in
product-marketing.md:183.
-->

> *We're early. The quotes on this page are real — from public HN threads — but they're about the pain, not about SPUR yet. Once the first fifty Community installs land and the feedback survey clears, named-user quotes from SPUR users go here. Until then, this space stays empty on purpose.*

---

## 7. Closing CTA

**Headline**

> Install SPUR. Dispatch one plan. Review one diff. That's the loop.

**Primary CTA — `curl`-pipe signed binary** *(per `product-marketing.md:10, 204`; NOT `cargo install`)*

```
curl -sSL https://getspur.dev/install.sh | sh
```

Button label: **Install SPUR (Community, $0)**

What you get on Community: 1 brain, 1 worker, full review loop, full cost display, full lineage (`product-marketing.md:104`). No license key required; runs under our EULA. No signup, no credit card.

**Secondary CTA**

> Watch the 60-second fail-Claude-to-Codex demo → `[video-placeholder]`

**Footer credibility tokens**

- Single signed Rust binary. No Node, no Python, no Docker (`product-marketing.md:90`).
- Any ACP-speaking agent works out of the box (`product-marketing.md:102`).
- We surface spend. We don't gate it. We don't promise SOC 2 or HIPAA at launch (`product-marketing.md:43, 110`). If you need either, talk to us before installing.

---

## A/B variant blocks

*Hero B and Hero C are A/B candidates against Hero A per `positioning.md:57-86, 139`. Hero B is the strongest secondary; Hero C is the warm-leads test against the existing tmux-native audience.*

### Variant — Hero B (control tower)

**Headline**

> The control tower for your CLI coding agents.

**Subhead**

> Dispatch Claude Code, Codex, Gemini, and Kimi in parallel. Review every diff in one place. Cherry-pick what merits merging. Walk away — your plan survives the closed laptop.

**Primary CTA**

```
curl -sSL https://getspur.dev/install.sh | sh
```

**Secondary CTA:** Watch the 60-second fail-Claude-to-Codex demo.

*Source: "control tower" verbatim from Beefin (`voc.md:93`). "Walk away — plan survives" cites beads durability (`product-marketing.md:171`). Cherry-pick from `product-marketing.md:84`.*

### Variant — Hero C (DIY wedge)

**Headline**

> You already built half of this in tmux. Let us finish the other half.

**Subhead**

> Keep your worktrees. Keep your agents. SPUR adds the one thing your shell can't: a durable plan, a review queue, and a live cost ledger across every vendor you already pay.

**Primary CTA**

```
curl -sSL https://getspur.dev/install.sh | sh
```

**Secondary CTA:** Watch the 60-second fail-Claude-to-Codex demo.

*Source: framing from `marketing/research/themes.md:88` synthesis #3. Defuses anti-pattern #4. Durable plan + review gate + cost ledger from `product-marketing.md:81-83, 79`.*
