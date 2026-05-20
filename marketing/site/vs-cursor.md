# SPUR vs Cursor

*Last updated: 2026-05-20. This page is a peers-not-competitors comparison. Cursor is the default AI IDE — over half of the Fortune 500 and 40k+ engineers at NVIDIA run it daily, and Composer 2.5 + Cloud Agents are real, capable products. SPUR is not an AI IDE and will not try to be one. Most SPUR users keep Cursor open in another window. If you're looking for a "Cursor replacement," you're on the wrong page; keep reading and we'll explain why.*

---

## TL;DR

**Most SPUR users keep Cursor open in another window.**

Cursor owns the in-editor experience — inline diffs, Composer's multi-file edits, IDE-native UX, and a polished cloud-agent path that never makes you leave the editor. SPUR doesn't compete with any of that. SPUR is the cross-session, cross-vendor control tower for the fleet of CLI agents you run *next to* the editor: 5–10 Claude Code / Codex / Gemini / GLM workers across worktrees, one review queue, one cost ledger.

This is an "editor vs. fleet" page, not a switch pitch.

---

## The honest overlap we're not going to hide

Cursor Cloud Agents and SPUR's worker fleet **overlap on the multi-agent JTBD**. Cursor ships:

- Multiple parallel agents on "their own dedicated computers" ([cursor.com/features](https://cursor.com/features)).
- Multi-model selection — Composer 2.5, GPT-5.5, Opus 4.7, Gemini 3.1 Pro, Grok 4.3, Auto-select.
- A Teams tier with shared cloud agents, team rules/skills/automations, and per-team analytics.
- A Security Review agent and a Bugbot that hook into PR reviews.

If your mental model is "AI IDE that also runs background agents for me," Cursor already does that and does it well. We will not tell you it doesn't.

Where SPUR sits **next to** rather than against Cursor:

1. **Cursor's cloud agents run on Cursor-managed sandboxes.** SPUR's workers run on **your repo, your shell, your worktrees** — the same place your tests, hooks, and tools already work. No "it runs differently on the vendor's machine" surprise when you merge.
2. **Cursor's multi-model menu is Cursor's curated set.** SPUR's brain layer dispatches into the CLIs you already install (Claude Code, Codex, Gemini CLI, GLM, Kimi, OpenCode). Failover between them isn't a feature on a roadmap — it's the wedge.
3. **Cursor's billing is Cursor's billing.** SPUR's cost ledger reads each vendor's JSONL/SQLite in place and aggregates spend across every CLI you run, including the ones you run *outside* Cursor.

The frame is workflow-complementary: Cursor wins inside the editor; SPUR wins across the editor and every CLI window you have open beside it.

---

## What Cursor owns

These are not concessions — they are deliberate scope choices SPUR will not contest.

- **The in-editor experience.** Streaming inline diffs, Composer chat in the editor pane, the cursor-aware multi-file edit loop, and the keyboard muscle memory of a VS Code fork. Nothing in a terminal touches it for the "I'm writing code right now" loop.
- **Composer's multi-file edits.** Composer 2.5 owns the "turn an idea into a coordinated diff across N files" job inside one task. SPUR does not try to be a multi-file editor.
- **IDE-native UX for cloud agents.** Even when Cursor's cloud agents run remotely, you review their output without leaving the editor. That's a strong UX for the IDE-bound user.
- **Distribution and default-tool status.** Over half of the Fortune 500. 40k+ engineers at NVIDIA. Stripe. YC portfolio companies. Cursor is *the* AI IDE for the median 2026 developer. SPUR will never out-distribute it for editor-bound users — and isn't trying to.
- **PR-surface integrations.** Bugbot in GitHub PR reviews, a Security Review agent on Teams, Slack/Jira/MS Teams hooks. SPUR's review surface is the TUI and Telegram, not the PR thread.

If your day is mostly "edit code in an editor," Cursor is the right surface and SPUR is not.

---

## What SPUR owns next to Cursor

Four things, all explicitly about the layer **between and around** editor sessions:

### 1. Cross-session orchestration

Cursor's Composer is in-session; Cursor's Cloud Agents are managed in Cursor's runtime. SPUR's plan + event log lives in beads + NDJSON on your machine. Close the laptop, kill the IDE, take an OS update — the plan resumes via event replay against your own repo. The "I closed the terminal and lost two hours" failure mode is the one SPUR was built against.

### 2. Multi-vendor agent fleet

Cursor's model menu is Cursor's. SPUR dispatches into the CLIs you already pay for: Claude Code, Codex, Gemini CLI, GLM, Kimi, OpenCode. The canonical use case: you trip Anthropic's Max weekly rate limit mid-sprint, brain-swap the plan to Codex, keep moving, and swap back when the window resets. **Neither Cursor (one curated menu) nor any single-vendor CLI ships this — by design.**

### 3. Cost ledger across CLIs

Cursor surfaces Cursor's spend — Pro+/Ultra sub-tiers, usage-based Bugbot, Teams analytics. That's the Cursor bill.

SPUR aggregates spend across Claude, Codex, Gemini, OpenCode, and Kimi by reading each vendor's JSONL/SQLite in place — no ETL, no proxy, no API key handover. One number for today's total, every vendor. Cursor cannot ship this without reading competitors' logs; SPUR's whole reason to exist is to do exactly that, locally.

### 4. Durable plan reconciler

When five workers in five worktrees produce five diffs, SPUR holds a DAG-ordered review queue and cherry-picks approved diffs onto a staging branch. The review state machine survives crashes, OS updates, and network outages because it's an event log, not a session. Cursor's Cloud Agents return work into the IDE; SPUR's workers return work into a durable queue that you (or a teammate, or a Telegram tap from the gym) can act on across sessions.

---

## Decision matrix

A concrete "what to use when" for the developer who already pays for Cursor and wants to know whether SPUR adds anything.

| Your situation | Cursor alone | Add SPUR next to Cursor | Both running |
|---|---|---|---|
| You spend the day editing code inside the IDE | ✅ Yes | ❌ SPUR is not an editor | — |
| You want inline diffs and Composer-style multi-file edits | ✅ Yes — Cursor wins this | ❌ | — |
| You occasionally fire a Cursor Cloud Agent for a background task | ✅ Yes | ❌ Don't add SPUR yet | — |
| You run **5–10 CLI agents** in tmux/worktrees beside the editor | — | ✅ This is the wedge | ✅ Editor in one window, fleet TUI in another |
| You **trip Anthropic Max rate limits** mid-sprint and your plan stalls | — | ✅ Brain-swap to Codex / Gemini / GLM and back | ✅ |
| You want **one cost number** across Anthropic + OpenAI + Google + Z.AI + Cursor | — | ✅ Unified live ledger | ✅ |
| You closed the laptop and lost two hours of agent context | — | ✅ Plans survive via beads + NDJSON | ✅ |
| You want to review/approve a diff from your phone on the train | — | ✅ Telegram bot on the same review state machine | ✅ |
| You want a curated, vendor-managed cloud sandbox to run agents in | ✅ Cursor Cloud Agents do this well | ❌ SPUR runs on your machine on purpose | — |
| You want an AI IDE with first-class team rules, Bugbot, and PR review | ✅ Cursor Teams | ❌ SPUR has no PR-surface story | — |
| You want an autonomous engineer that owns tickets in Slack | ❌ That's Devin, not Cursor and not SPUR | ❌ | ❌ |

**The honest rule:** if your day is editor-bound and your only AI vendor is Cursor, stay on Cursor alone. SPUR's value materializes when the fleet size is ≥ 2 *outside* the editor, or when the vendor count is ≥ 2 *across* tools.

---

## A concrete workflow

The picture we want you to leave with:

> It's a Wednesday afternoon. You're in **Cursor**, in the middle of a tight loop on a UI component — Composer is fixing three files at once, inline diffs streaming, you're reviewing them as they land. The component is yours; you want the muscle memory and the keystrokes.
>
> In the window next to Cursor, **SPUR's TUI** is showing four workers running in four worktrees: a Claude Code worker pulling apart a 1,400-line god-module, a Codex worker porting a legacy serializer test suite to the new framework, a Gemini worker writing a migration script, and a GLM worker doing a bulk-rename refactor across the test directory. The status grid tells you which is waiting on you and which is still working. Today's spend so far is one number in the status bar: $34.18 across four vendors.
>
> You hit a wall on the UI component because Anthropic just rate-limited you in Cursor's Composer. You don't lose anything — Cursor stays open with your half-edited files, and SPUR's brain swaps the relevant CLI worker from Claude Code to Codex without abandoning the plan. You go back to editing.
>
> An hour later, the Claude Code worker finishes the god-module split and lands in SPUR's review queue. You tap "merge" on your phone (Telegram bot, same review state machine as the TUI). The diff cherry-picks onto your staging branch. You never left Cursor.

Cursor edits the file you have your attention on. SPUR coordinates the four jobs you don't.

---

## Side-by-side capability matrix

Picked from dimensions where the two tools live at different layers. We've left in-editor UX, multi-file Composer edits, Bugbot, PR reviews, and IDE integration out of the matrix on purpose — Cursor wins those and a comparison would be a category error.

| Capability | Cursor (standalone) | Cursor + SPUR |
|---|---|---|
| In-editor coding UX | Best-in-class | Unchanged — SPUR is not in the editor |
| Multi-file Composer edits inside one task | First-class (Composer 2.5) | Unchanged — SPUR doesn't touch this |
| Cloud agents in a vendor sandbox | Cursor-managed compute | Unchanged — you can still use them |
| **Parallel CLI workers across local worktrees** | Manual / out of scope | Managed fleet with DAG-ordered cherry-pick onto a staging branch |
| **Rate-limit recovery across vendors** | Switch models inside Cursor's curated menu | Brain-swap the plan to Codex / Gemini / GLM, resume on Claude when the window reopens |
| **Cross-vendor cost view (incl. non-Cursor CLIs)** | Cursor spend only (Pro+/Ultra/Bugbot) | Unified live ledger across Claude, Codex, Gemini, Kimi, OpenCode, and Cursor (where surfaced) |
| **Plan durability across crashes / closed laptops** | Session-scoped + cloud-agent state | Beads + NDJSON event log on your machine, resume via replay |
| **Mobile review** | n/a (editor-first) | Telegram bot on the same review state machine as the TUI |
| **Cross-session, cross-vendor review queue** | n/a | First-class review lane with timeout / retry / merge gating |
| Runtime locus | Cursor's IDE + Cursor's cloud | Your shell, your repo, your worktrees |
| Distribution | $0 Hobby → $20 Individual → $40/user Teams → Enterprise | `cargo install spur-cli`, additive to whatever you already pay |

Read the matrix as: *Cursor owns the row inside the editor; SPUR owns the row across everything next to it.*

---

## Who's best for who

**Stay on Cursor alone if:**
- Your day is mostly editor-bound
- One Cursor Cloud Agent at a time covers your background-work needs
- Your only AI bill is Cursor's
- You don't run CLI agents in tmux/worktrees beside the editor

**Add SPUR next to Cursor if:**
- You already run 5–10 CLI agents at a time across worktrees (the canonical wedge persona — Beefin, HN 47104424)
- You've been ambushed by Anthropic rate limits mid-sprint and want a cross-vendor brain-swap path
- You pay Anthropic + OpenAI + Google + Z.AI + Cursor and can't see one total
- You want a review queue that survives a closed laptop and accepts a "merge" tap from your phone

**Neither of us is for you if:**
- You want an autonomous engineer you assign tickets to in Slack — that's **Devin**.
- You want the best in-session single-agent CLI with Anthropic's models — that's **Claude Code** (and SPUR runs it as a worker; see [SPUR vs Claude Code](vs-claude-code.md)).
- You want the simplest possible BYO-key single-agent CLI with no orchestration — that's **Aider**.

---

## Open question we owe the reader

Cursor's roadmap is the obvious watch-list. If Cursor Cloud Agents ship a cross-vendor cost ledger that reads non-Cursor CLI spend, or a durable cross-session plan reconciler that runs against the user's local worktrees rather than Cursor's sandbox, the gap narrows fast. We'll keep this page honest. Today, Cursor's design is deliberately editor-first, vendor-curated, and Cursor-runtime — that's the shape SPUR is built next to, not against.

---

## CTA

Keep Cursor. Add SPUR.

- `cargo install spur-cli` — installs the control tower next to whatever editor and agents you already run
- 60-second demo: four CLI workers in four worktrees, one Cursor window, one cost number, one phone tap to merge
- Docs: how SPUR's fleet sits beside an editor without trying to be one

---

*Source files: `marketing/competitors/cursor.md`, `marketing/competitors/_summary-indirect.md:17,32-36`, `marketing/messaging/positioning.md:98-100`, `marketing/research/themes.md` (themes #1–#4), `marketing/product-marketing.md:8,74-81`. Verbatim VOC: Beefin (HN 47104424), nojs (HN 47573483), roxolotl (HN 44598254), gorbypark (HN 44713757).*
