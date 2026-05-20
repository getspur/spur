# SPUR — Product Hunt Submission Package

*2026-05-20. Phase-4 launch deliverable. Grounded in `marketing/product-marketing.md` V1.3 (Product Hunt is on the channel list at line 215; Show HN is explicitly NOT, per line 216 — no public repo), `marketing/messaging/positioning.md` (Hero A/B/C candidates), `marketing/messaging/levers.md` (Pratfall + cost-ledger anchors), and `marketing/site/og/PLAN.md` (brand-visual constraints). Every copy element fits PH character limits exactly. No emoji. No "AI-powered" / "platform" framing.*

---

## 1. Tagline (PH limit: 60 characters)

PH renders the tagline directly under the product name. Keep it concrete, no superlatives.

### Recommended — lead (Hero A, cost-ledger flavor) — **52 chars**

> **One live cost ledger across every CLI coding agent.**

Sources Hero A's "see what you'd be billed today, across every agent, in one number" (`marketing/messaging/positioning.md:64`) and the V1.3 differentiation order — the unified cost ledger is the one moat no peer can match by design (`marketing/messaging/positioning.md:42`, `marketing/competitors/_summary-indirect.md:44`).

### Alternate A — Hero B (control tower) — **46 chars**

> **The control tower for your CLI coding agents.**

Verbatim category lift from Beefin's HN comment (`marketing/research/voc.md:93`), already adopted as SPUR's owned category in `positioning.md:9`. Safer if the cost-ledger accuracy disclosure is still pending — control-tower framing carries fewer numeric obligations.

### Alternate B — Hero C (DIY wedge) — **48 chars**

> **You built half of this in tmux. We finished it.**

Compressed from `positioning.md:81`. Reads best to the tmux/worktree audience PH gets from its dev-tool tag, but loses traction with EM/Team-Lead browsers who don't run worktrees themselves.

**Ship-it pick:** see § 9 below.

---

## 2. Description (PH limit: 260 characters)

PH shows the description on the listing card and in search results. Open with the category-owning phrase ("control tower"), then prove with the cost ledger, then close with a falsifiable install action.

> **Control tower for your CLI coding agents. One review queue, one cost ledger across Claude Code, Codex, Gemini, Kimi, and OpenCode — so you stop discovering $1k weeks by accident. Worktree per worker. Approve from terminal or Telegram. Install in one curl.**

Character count: **255**. (PH counts the trailing period; the em-dash is one character.)

Differentiation order matches V1.3: control-tower category → cost ledger (the moat) → worktree-per-worker (the substrate) → mobile review (the surface) → install ergonomics (the proof). No "AI-powered," no "platform," no superlatives. The "$1k weeks by accident" anchor is the 2A lever (`marketing/messaging/levers.md:60-66`) — verbatim VOC framing, not invented copy.

---

## 3. First-comment / maker comment (~200 words)

Posted by the maker account at T+0 minutes. PH's convention is that the first comment carries the long-form story; the description is the elevator pitch. Voice: calm, specific, self-aware about scope (`marketing/product-marketing.md:163-165`). Inline accuracy disclosure for the cost ledger is non-negotiable per `marketing/messaging/levers.md:144-145`.

> Hi PH — Vu here, building SPUR.
>
> I built SPUR because I was already running five Claude Code, Codex, and Gemini sessions across different worktrees, and the coordination tax was eating my afternoon. tmux gets you to five parallel agents. It does not get you to ten.
>
> SPUR is a single Rust binary that sits above the agents you already use. One brain decomposes the plan, workers execute in isolated git worktrees, every diff lands in a structured review queue. You approve from the terminal or from Telegram. Approved diffs cherry-pick onto a staging branch in DAG order. Close the laptop; the plan survives.
>
> The differentiator we cared most about: a single live cost ledger across Claude Code, Codex, Gemini, Kimi, and OpenCode — reading each vendor's JSONL/SQLite in place, no ETL. Two engineers in our research corpus described agent spend that was ~5× what their finance team thought it was. SPUR shows the actual number, per developer, per repo, this week.
>
> A disclosure I want to make up front: the cost ledger reflects vendor-side billing surfaces, which lag the real invoice by up to ~4 hours per extractor and use per-vendor pricing tables that we keep in version control. It is the closest cross-vendor view that exists, and it is not the official invoice. We surface the gap; we do not gate it.
>
> Bring your own agents, bring your own keys. Install in one curl. Happy to answer anything.

(~225 words after the disclosure paragraph, which is the load-bearing addition. If a hard 200-word cap is enforced, the cut is the second paragraph's second sentence, not the disclosure.)

---

## 4. Gallery plan — 6 slides

PH allows up to 8 media items; we ship 6 to keep the story tight. All slides 1200×750 (PH gallery aspect), brand-visual constraints from `marketing/site/og/PLAN.md` — terminal palette `#0B0E14` / `#E6E1CF` / ANSI accents, JetBrains Mono, no emoji, no people, no glassmorphism, no SaaS-y illustration.

### Slide 1 — Hero: the cost ledger (must be slide #1)

| Field | Value |
|---|---|
| **Caption (2 lines)** | One live cost ledger across every CLI coding agent.<br>Read straight off vendor JSONL — no ETL, no plugin, no auth dance. |
| **Generation prompt** | A near-black terminal screenshot, flat `#0B0E14` background, displaying a single `spur cost --today` table view: five rows (claude-code, codex, gemini, kimi, opencode), three columns (today $, this-week $, vs last-week %). Numbers in `#E6E1CF`, header in `#7FB4CA`, the "vs last-week" column conditionally `#76946A` or `#C34043`. JetBrains Mono, tight tracking. Footer line `▎ spur cost --today` in `#957FB8` block-cursor + ivory wordmark. No window chrome, no emoji, no glassmorphism, no gradient mesh. 1200×750. Composite headline overlay in post — generation handles background + table glyphs only, no real readable text in the AI pass per `marketing/site/og/PLAN.md` text-rendering note. |

### Slide 2 — Lineage tree (collapsible ASCII)

| Field | Value |
|---|---|
| **Caption** | Lineage view: every brain, every worker, every retry, in one tree.<br>Press space to collapse a subtree. Press `r` to retry the failed one. |
| **Generation prompt** | A near-black terminal panel showing a collapsible ASCII tree built from box-drawing glyphs (`├─` `╰─` `│`). Three branches (brain → worker-1, worker-2, worker-3), each with two children (plan → executing → review-pending). Status glyphs in ANSI accents: `●` green for completed, `◐` yellow for executing, `○` ivory for queued, `✗` red for failed. JetBrains Mono. Background `#0B0E14`, foreground `#E6E1CF`. No emoji, no UI screenshots from real SaaS apps, no gradient, no depth. 1200×750. |

### Slide 3 — Review card

| Field | Value |
|---|---|
| **Caption** | Every diff lands in a review card: approve, reject, modify, retry.<br>Cherry-pick in DAG order onto staging. No auto-merge, ever. |
| **Generation prompt** | A near-black terminal showing a single review-card panel with a TUI box-drawing border (`╭───╮│╰───╯`). Inside: a diff header line (`+ 23 / − 6 in src/auth.rs`), four rows of `+` / `−` lines with subtle syntax-color hints (only two accent colors used — `#76946A` for added, `#C34043` for removed). Footer button row `[ A approve · R reject · M modify · ↻ retry ]` in `#7FB4CA`. Background `#0B0E14`, ivory text. JetBrains Mono. No emoji, no mouse cursor, no glassmorphism. 1200×750. |

### Slide 4 — Brain-swap moment

| Field | Value |
|---|---|
| **Caption** | Claude hit your weekly cap. SPUR fails the plan to Codex and keeps going.<br>Cross-vendor failover is a config flag, not a refactor. |
| **Generation prompt** | A near-black terminal log showing a vertical sequence of three event lines, monospaced, left-aligned. Line 1 in `#C34043`: `15:42  claude-code  rate_limit_exceeded  retry_after=86400s`. Line 2 in `#DCA561`: `15:42  brain-swap   claude-code → codex   plan_id=p_8f3a`. Line 3 in `#76946A`: `15:43  codex        worker_started        diff_pending=0`. Background `#0B0E14`, JetBrains Mono. No emoji, no UI chrome, no people. 1200×750. |

### Slide 5 — Telegram approval

| Field | Value |
|---|---|
| **Caption** | Same review queue, on your phone.<br>Three buttons. No countdown. No auto-merge from mobile. |
| **Generation prompt** | A vertical mobile-shaped frame (rounded corners, dark `#0B0E14` fill) on a slightly lighter background `#11141C`. Inside: a single Telegram-style message bubble showing five lines of monospaced text (worker id, file changed, lines added/removed, est. cost in `#DCA561`, three inline buttons `[ Approve ]` `[ Reject ]` `[ Open in TUI ]` rendered as TUI-style brackets in `#7FB4CA`). No emoji in the bubble. No notification badges. No app icons. JetBrains Mono. 1200×750. |

### Slide 6 — Install one-liner

| Field | Value |
|---|---|
| **Caption** | Install in one curl. One Rust binary. Ed25519-signed.<br>No daemon. No telemetry by default. `cargo uninstall` leaves nothing behind. |
| **Generation prompt** | A near-black terminal showing a single prompt line: `$ curl -sSL https://getspur.dev/install.sh \| sh` in `#E6E1CF`, followed by three subdued log lines underneath in `#7FB4CA` at 60% opacity (`verifying signature…  installing to ~/.local/bin  ready: spur --help`). Background `#0B0E14`. JetBrains Mono. Bottom-left wordmark `▎spur` (`#957FB8` cursor + ivory). No emoji, no window chrome, no gradients, no logo lockups other than the SPUR wordmark. 1200×750. |

**Aspect note:** PH automatically generates 16:9 thumbnails from the first image, so slide 1 must read as a hero crop at 1200×675 *and* fill the full 1200×750 — keep the table center-aligned with vertical breathing room.

---

## 5. Hunter outreach shortlist

PH's "hunter" mechanic is softened in 2026 (PH dropped exclusive hunter privileges in 2023 and the hunter field is now closer to a co-sign than a launch lever) — but a hunter with terminal/devtool reach still amplifies day-1 visibility through their following. Outreach goal is **a co-sign and amplification**, not an exclusivity arrangement.

Outreach order is by 2026-recency of devtool hunts × likely terminal-audience overlap × responsiveness to cold messages with a working binary attached.

| # | Hunter | Why for SPUR | Best contact | 2026 activity check |
|---|---|---|---|---|
| 1 | **Chris Messina** | Most-followed hunter on PH; consistently hunts dev infra and CLI tools; recognized in maker community. | DM on X (`@chrismessina`) — known to respond to dev-tool DMs with a working demo link | Active through 2025; verify recent hunts in last 30 days before outreach |
| 2 | **Kevin William David** | Editorial taste in productivity + dev tools; 17k+ hunts historical; strong cross-promotion via newsletter. | LinkedIn DM + X (`@kwdinc`); newsletter is `Bootstrapped`/`SaaS Bytes`-adjacent | **Verify before outreach — see § 9(b)** |
| 3 | **Tim Smith** | Hunts terminal + indie-dev tools; smaller follower count, higher response rate, comments thoughtfully on his hunts (drives early comment density). | X (`@tim_smith` — confirm handle) | Confirm the right Tim Smith — common name; risk of stale contact |
| 4 | **Mubashar Iqbal** | Indie-hacker community; hunts many micro-tools; comments engage maker stories well. | X (`@mubashariqbal`) — known to do "hunt me" intros via DM with a demo loom | Confirm 2026 active — historically very consistent |
| 5 | **Tristan Pollock** | VC-aligned hunter (500 Startups alum / current investor); brings later-stage SaaS audience that overlaps with EM/Team-Lead persona. | X DM + warm intro via Anthropic DevRel if available | Confirm current PH activity — historically less prolific than Messina |
| 6 | **Andreas Klinger** | Former PH head of remote/community; carries weight inside PH org; hunts technical, infra-y products. | X (`@andreasklinger`) — responds to thoughtful technical pitches | Confirm 2026 active |
| 7 | **Hiten Shah** | Crossover audience: SaaS founders + RevOps + dev-tool builders; high reply rate on cold DMs with a clear ICP fit. | X (`@hnshah`) — known reply rate ~24h on substantive DMs | Active through 2025; verify last hunt date |

**Outreach template (per-hunter, ~80 words, customize the lead sentence):**

> Hey [name] — Vu, building SPUR (control tower for CLI coding agents — one review queue + one cost ledger across Claude Code, Codex, Gemini, Kimi, OpenCode). Launching on PH on [date]. The early TUI build is at [private demo link]. Two engineers in our research corpus described agent spend that was ~5× what their finance team thought it was; SPUR is the first thing that shows the actual cross-vendor number. Would you be open to hunting? Happy to send a 60-second loom.

Cap: pitch no more than 3 hunters at once with the same launch date. If two say yes, pick the one with the higher recent-hunt frequency; the second gets credited as a maker collaborator instead.

---

## 6. Launch-day comment-reply playbook

Pre-drafted replies to the four predictable PH comment patterns. Each ≤ 100 words, voice-matched to `marketing/product-marketing.md:163-165` (rigorous, pragmatic, self-aware), and aligned to the Pratfall lever (`marketing/messaging/levers.md:68-74`) — admit the scope honestly.

### 6.1 — "Why isn't this open source?"

> Honest answer: open-core was the original plan, and we walked it back during V1.3 of the PRD when it became clear that the cost-ledger extractors (the moat) would be the first thing forked and re-skinned, and the human-review state machine would degrade in the hands of someone removing the gate "just for their workflow." SPUR is a single signed Rust binary you install with one curl; it runs entirely on your machine; we don't take telemetry by default. Source availability is on the roadmap for the components we can open without breaking the moat. Not promising a date.

### 6.2 — "How is this different from Cursor / Aider / Claude Code?"

> Short version: those are in-session tools; SPUR sits above them. Cursor is an editor — most SPUR users keep Cursor open in another window. Aider is a single-agent pair-programmer; SPUR's value materializes at fleet size ≥ 2. Claude Code is the source of half our research corpus — we run it as a worker, not against it. The thing none of them attempt: a single live cost ledger across every vendor you pay, plus cross-vendor brain-swap when one of them rate-limits. We wrote that out in `marketing/site/vs-claude-code.md` and `marketing/site/vs-cursor.md` if you want the long version.

### 6.3 — "What's the pricing?"

> Three tiers: Community (free, single developer, BYO keys, full TUI). Pro at $19/mo or $290 lifetime (cost ledger across all five vendors, Telegram review, priority support). Team at $49/seat/mo (shared cost ledger across the team, audit log, basic RBAC). What Team *doesn't* do: it doesn't enforce a per-developer budget cap (it surfaces spend; it doesn't gate it). It doesn't include SOC 2 or HIPAA at launch — those are Enterprise. Pricing page on the site has the full split with no dark-pattern defaults.

### 6.4 — "When on Windows?"

> macOS and Linux at launch. Windows: WSL2 works today (it's just a Linux binary under the hood); native Windows is on the roadmap behind the Telegram-bot polish and the Anthropic-cost-extractor v2. We'd rather ship a Windows binary that handles long paths and CRLF cleanly than one that "technically runs." If you'd pay for native Windows specifically, reply here — the volume of replies on this comment is how we'll prioritize.

---

## 7. Submission timing rationale — 06:00 UTC vs 12:01 AM PT

PH's day rolls over at **12:01 AM Pacific Time**, and the day's leaderboard begins accumulating votes from that minute forward. Submitting at or seconds-after the rollover gives a product the full 24-hour window to climb. Submitting at, say, 10:00 AM PT cedes the morning vote surge to whatever launched at midnight — typically irrecoverable.

The "06:00 UTC = midnight PT" framing in the brief is approximate; the exact mapping depends on US daylight-saving status:

- **PDT (Mar–Nov):** 12:01 AM PT = **07:01 UTC**.
- **PST (Nov–Mar):** 12:01 AM PT = **08:01 UTC**.

If launch lands in the PDT window (likely for a 2026 H2 launch), target **07:00 UTC submission, 07:01 UTC live**. The 06:00 UTC framing leaves a one-hour buffer for any PH publish lag, the maker comment, and the first hunter ping — that buffer is the actual reason to aim early, not midnight itself.

**Why not later in the day:**

- The first 4 hours determine ranking momentum; PH's algorithm weights early votes more heavily.
- US west-coast tech audience wakes between 06:00–09:00 PT — they need to find SPUR already on the day's leaderboard, not still loading.
- European day-2 amplification (LinkedIn re-share, X re-thread) requires SPUR to be #1–#5 by 09:00 UTC; that's only achievable with a midnight-PT submission.

**Pre-flight checklist for launch hour:**

1. T-60 min (06:00 UTC): final gallery sanity check, hunter DM with the live URL queued in drafts.
2. T-15 min: maker account logged in on two devices (primary laptop + phone), in case PH session expires.
3. T+0 (07:01 UTC PDT): submit. Verify the listing renders and the description didn't truncate.
4. T+2 min: post the first maker comment from § 3.
5. T+5 min: hunter DM goes live; X thread goes live with PH link.

---

## 8. Brand-voice audit on the PH copy in this doc

Internal check before submission, against `marketing/product-marketing.md:148-154` and the Words-to-avoid table in `positioning.md:115-130`. No "AI-powered," no "platform," no "synergy," no "ecosystem," no superlatives, no emoji, no exclamation marks, no countdown timers, no "founding seats."

Tone exceptions — language that is *slightly* warmer than the brand norm because PH culture rewards human framing in the maker comment:

- The maker comment uses "Hi PH — Vu here." This is one register above SPUR's site copy, accepted because PH comment culture punishes pure formality.
- The Slide 4 caption ("Claude hit your weekly cap…") is colloquial; preferred over a more measured "Claude returned a rate-limit response" because PH gallery captions are read at a glance.

Neither exception drifts into "AI-powered" / "platform" territory. See § 9(c) for the one judgement call.

---

## 9. Summary — recommendations for the caller

### (a) Tagline I would ship if forced to pick one

**The cost-ledger lead — "One live cost ledger across every CLI coding agent."**

Three reasons, in order: (1) it's the differentiator no peer can match by design (`marketing/competitors/_summary-indirect.md:44`); (2) Phase-1 research surfaced cost opacity as the *sharpest emotional language* in the VOC corpus (`marketing/research/themes.md:33`), even though rate-limit has more total quotes; (3) PH's listing-page UX puts the tagline directly under the product name with the description below — leading with cost-ledger creates an "oh — that's the one no one else has" pattern that the description then expands. Hero B (control tower) is a credible fallback if the accuracy disclosure in the maker comment isn't ready; Hero C (DIY wedge) is best on HN (which we are not running because the V1.3 PRD ruled out Show HN — `marketing/product-marketing.md:216`), not on PH where the audience is broader.

### (b) Hunter shortlist row most likely to be stale

**Row #2 — Kevin William David.** Two reasons to verify before outreach: (i) historically prolific but PH's hunter-mechanic softening in 2023–24 reduced the incentive for high-volume hunters to keep launching weekly hunts, so 2026 cadence is unknown; (ii) his focus has rotated toward newsletter/community work (SaaS Bytes / Bootstrapped) which may or may not still include active PH hunts. **Action before outreach:** check the last 30 days of his PH profile for hunted launches; if zero in 30 days, deprioritize to row #4 and bump Mubashar Iqbal to row #2. (Andreas Klinger and Hiten Shah on rows #6–#7 are also worth a recency check — both have been quieter on PH in 2025 — but Kevin's is the row whose value is most directly tied to current PH activity.)

### (c) PH-specific copy that contradicts brand voice and should be relaxed for the platform

**Two specific places, by design:**

1. **The "Hi PH — Vu here." opener in § 3.** SPUR's site copy never opens with a personal greeting; the brand voice is "rigorous, pragmatic, terminal-native" (`marketing/product-marketing.md:163`). PH comment culture, however, rewards a human first sentence — comments that open with a feature claim get read as marketing-bot output and downvoted. Recommendation: keep the relaxation for the maker comment only. Do not propagate "Hi [audience] — [name] here" to any site page, email, or X copy.
2. **Gallery captions that use second-person verbs ("Press space to collapse," "Approve from your phone").** SPUR's site copy biases toward third-person product description; PH gallery captions are read in <2 seconds and need imperative-mood verbs to land. Recommendation: keep second-person imperative in PH gallery only; revert to third-person on the homepage and OG images.

Both relaxations are PH-scoped and time-boxed to the launch-day asset set. Neither is a permanent brand-voice change.

---

*End of submission package. Sign-off required from the maker before T-7 days. Gallery assets must be generated and reviewed by T-3 days. Hunter outreach goes out at T-5 days (verify row #2 by T-7).*
