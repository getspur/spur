# SPUR — 90-second demo script

*Screen-recorded product demo. Not an animated explainer. Real terminal, real SPUR commands, real vendor CLIs. Audience: terminal-native developers ($200–600/mo AI spend) who already run 2–10 CLI agents and have hit at least one rate-limit wall in the last 30 days.*

*Built from `marketing/messaging/positioning.md` (Hero A primary; fail-Claude-to-Codex called out as the single strongest proof-point video), `marketing/research/themes.md` (rate-limit ambush + cost opacity = load-bearing pain), `marketing/competitors/_summary-indirect.md` (cross-vendor failover is the unique-to-SPUR move). Skill: `marketing/marketingskills/skills/video/SKILL.md` § "Product Demo Video".*

---

## The arc — five beats in 90 seconds

| Beat | Time | What the viewer sees | What the viewer feels |
|---|---|---|---|
| **1. The ambush** | 0:00 – 0:15 | Claude Code prints `Claude usage limit reached. Your limit will reset at 3pm.` mid-task | "Yes. That. I have lived this." |
| **2. The fail-over** | 0:15 – 0:30 | `Alt-p` opens the Plan Inspector; brain switches `claude-code-acp` → `codex`; the plan resumes from the same task node | "Wait — it just kept going." |
| **3. Cross-vendor ledger** | 0:30 – 0:50 | Work continues on Codex; `Alt-a` opens Insights; live ledger shows spend across both vendors in one number | "I can finally see what I'm actually paying." |
| **4. The return** | 0:50 – 1:10 | A toast: `Claude window reset.` Brain switches back; the plan continues from the next task node via event replay | "I never lost context." |
| **5. The line + the install** | 1:10 – 1:30 | Closing card: tagline + `curl -sSL https://getspur.dev/install.sh | sh` | "I want to try this." |

---

## Shot list — sequential, single take where possible

Every shot is a real terminal. No mocks, no After Effects. The only post-production is the subtitle track and the closing install-card overlay.

### Shot 1 — `0:00 – 0:05` — Cold open inside Claude Code

- **Setup:** Claude Code already running in a clean `spur` worktree. Mid-task: it has just finished a `Read` tool call on `crates/spur-tui/src/events.rs` and is about to write a fix.
- **Action:** A single keystroke (`Enter`) commits the next prompt. Claude returns `Claude usage limit reached. Your limit will reset at 3pm.` in red.
- **Capture:** Full terminal, 1920×1080. Hold on the red message for ~2 seconds. Cursor blinks, idle.
- **Why this shot:** This is the moment everyone in the audience has lived. It must feel cold and quiet — no music swell, no cut.

### Shot 2 — `0:05 – 0:15` — The buremba-style anguish (hold)

- **Setup:** Same frame as Shot 1. No transition.
- **Action:** Nothing. The cursor blinks. A small clock in the status bar advances.
- **Capture:** Hold for 10 seconds with a 2-second subtitle slowly fading in mid-frame: `Locked out. Still paying.`
- **Why this shot:** The pause is the proof we understand the pain. Cutting away too fast turns this into an ad; holding makes it a recognition.

### Shot 3 — `0:15 – 0:22` — `Alt-p` opens Plan Inspector

- **Setup:** Press `Alt-p`. The SPUR Plan Inspector opens over the dead Claude window — kanban-style columns, one task highlighted in `running`, two in `done`, three `queued`.
- **Capture:** Hold on the inspector. Keyboard callout overlay (bottom-right): `Alt-p` rendered in the same monospace as the terminal.
- **Why this shot:** Establishes the control-tower frame without saying the words. The plan didn't die — it's just waiting.

### Shot 4 — `0:22 – 0:30` — Brain swap

- **Setup:** In the inspector, focus the active task. Press `Ctrl-K` for the Command Palette. Type `switch worker`. Select `codex`.
- **Action:** The task's brain badge flips from `claude-code-acp` → `codex`. The task re-enters `running`. The Stream tab on the right shows Codex picking up the same context.
- **Capture:** Tight on the badge change (frame-by-frame readable). Keyboard callout: `Ctrl-K → switch worker → codex`.
- **Why this shot:** This is **the** moment. The one no peer in the indirect-competitor set can record (`marketing/competitors/_summary-indirect.md:21–26`). It must be clean, single-take, no edit.

### Shot 5 — `0:30 – 0:45` — Codex resumes the work

- **Setup:** Back to the full TUI. Codex is mid-`apply_patch` on `crates/spur-tui/src/events.rs` — same file Claude was reading 25 seconds ago.
- **Action:** Let it run. Show ~3 lines of tool output ticking by.
- **Capture:** Full terminal. No callouts. Let the viewer read.
- **Why this shot:** Proof that "resumes exactly where it left off" is a verb, not a marketing noun.

### Shot 6 — `0:45 – 0:50` — `Alt-a` opens Insights

- **Setup:** Press `Alt-a`. Insights overlay slides in.
- **Capture:** Cost ledger panel front-and-center. Two vendor rows visible: `claude-code-acp $1.42` · `codex $0.31` · **`session total $1.73`**. A small badge: `live`.
- **Why this shot:** This is the single visual most likely to be screenshotted and shared independently (see Summary §a). Frame it like a hero image — bottom-third of the screen, breathing room around the total.

### Shot 7 — `0:50 – 1:00` — The return notification

- **Setup:** Stay in Insights for ~3 seconds. A toast appears in the status bar: `Claude window reset.`
- **Action:** Press `Esc` to close Insights. Back to the Plan Inspector. Press `Ctrl-K`, `switch worker`, `claude-code-acp`. Badge flips back.
- **Capture:** Same beat as Shot 4 in reverse. Keyboard callout: `Ctrl-K → switch worker → claude-code-acp`.

### Shot 8 — `1:00 – 1:10` — Claude continues from the next task

- **Setup:** Plan Inspector with the next queued task entering `running` under the Claude badge.
- **Action:** Stream tab shows Claude reading the diff Codex just wrote — explicit continuity.
- **Capture:** Hold. Subtitle: `Same plan. Different brain. No reset.`

### Shot 9 — `1:10 – 1:25` — Closing card

- **Setup:** Fade terminal to 60% brightness. Overlay one line, monospace, centered:

  > The control tower for your CLI coding agents.

  Beneath it, the same monospace, one line:

  > `curl -sSL https://getspur.dev/install.sh | sh`

- **Capture:** Hold for 15 seconds. No animation on the install command — viewers should be able to pause and read.
- **Layout note:** The curl-pipe command is 46 characters. At Berkeley Mono 22pt on 1920×1080, that is ~600px wide — comfortably under the 1600px safe-margin width (160px gutters left/right). Render it as a single centered line; do **not** split. If the recording surface ever changes (e.g. 1280×720 for a mobile-first cut), re-measure before locking the frame — at 1280-wide the same string sits at ~470px and still fits, but the gutter shrinks.

### Shot 10 — `1:25 – 1:30` — End slate

- One line, bottom-left: `spur.dev`
- No "subscribe," no logos parade, no call to action stack. Out.

---

## Narration script — ~150 words, ~100 wpm

*Voiceover. Calm, mid-register, no enthusiasm spikes. Read it like you are explaining a tool to one engineer at a desk — not pitching a room. Pause where indicated. No music with lyrics. Optional bed: a single sustained ambient pad at -28 dB, or silence. Developer audience reads silence as confidence.*

> **(0:00, over Shot 1, after the red message lands)**
> You're paying for it. And you're locked out of it.
>
> **(0:15, as Alt-p opens)**
> SPUR runs your CLI coding agents side by side.
> When one hits a wall, you swap the brain — not the plan.
>
> **(0:30, as Codex resumes)**
> Codex picks up exactly where Claude stopped. Same worktree. Same task. Same context.
>
> **(0:45, as Insights opens)**
> And it shows you what you're actually spending — across every agent — in one number.
>
> **(0:55, after the reset toast)**
> Claude's window resets. Swap it back. The plan continues.
>
> **(1:10, over the closing card)**
> SPUR. The control tower for your CLI coding agents.
> Install it with one line — `curl -sSL https://getspur.dev/install.sh | sh`.

Word count: 118 spoken words across ~75 seconds of voiced time (the rest is intentional silence — the 10-second hold at the open, the closing card). Pace lands at ~95 wpm, deliberately under the 100 wpm target so the rate-limit pause and the install card both breathe.

---

## Screen captures needed (asset checklist)

- [ ] **`limit-reached.cast`** — Claude Code session recorded with `asciinema` until the rate-limit message lands. Single take. Real account.
- [ ] **`plan-inspector-running.png`** — Plan Inspector with one task `running`, two `done`, three `queued`. Used as a thumbnail candidate.
- [ ] **`brain-badge-flip.gif`** — 4-second loop of the badge transition `claude-code-acp` → `codex`. Reused across social.
- [ ] **`codex-apply-patch.cast`** — Codex mid-`apply_patch`. Must be the same file Claude was reading. Continuity is the proof.
- [ ] **`insights-cross-vendor.png`** — Cost ledger panel with two vendor rows and one session total. This is the hero still.
- [ ] **`window-reset-toast.png`** — Status bar with `Claude window reset.` toast.
- [ ] **`closing-card.png`** — Tagline + install command, static overlay.

All `.cast` files are recorded with `asciinema rec` so they can be re-rendered at any size without re-shooting. PNGs are pulled as still frames from the recording, not separately staged.

---

## Recording settings — terminal, font, cursor

Subtitle-style overlays are unreadable on mobile when the underlying terminal is the developer default of 14pt and a 16:10 window. These settings are non-negotiable for this shoot.

### Terminal emulator

- **Emulator:** Ghostty (preferred) or Kitty. Both render TUI box-drawing characters identically across macOS / Linux.
- **Window size:** 1920×1080, native, recorded with `screencapture` on macOS or `wf-recorder` on Wayland. Do **not** scale up.
- **Window chrome:** Hidden. No tab bar, no title bar. Use Ghostty's `window-decoration = false`.
- **Background:** `#0B0E14` (near-black, not pure black — pure black banding crushes on YouTube re-encode).
- **Foreground:** `#E6EDF3`.

### Font

- **Family:** Berkeley Mono (preferred) or JetBrains Mono. Both have unambiguous `l / I / 1` and a wide colon.
- **Size:** **22pt** at 1920×1080. This is roughly 1.6× the developer-desk default. It looks oversized on a desktop monitor and *correct* on a phone in a feed — which is where the audience watches.
- **Line height:** 1.35.
- **Ligatures:** Off. Ligatures make `==` and `=>` unreadable in scaled screen captures.

### Cursor

- **Shape:** Solid block.
- **Blink:** On, at 1 Hz (slower than default). Faster blink reads as anxious; slower reads as patient.
- **Color:** `#FFC857` (warm yellow). Stands out against `#0B0E14` without being the brand red.

### Subtitle overlay

- **Font:** Inter, **48pt** semibold. Sans-serif against the mono terminal — separation matters.
- **Position:** Bottom third, 80px from the bottom edge, never overlapping the SPUR status bar.
- **Color:** `#E6EDF3` on a `rgba(11, 14, 20, 0.85)` rounded rectangle. Subtitles must remain legible when YouTube auto-captions stack on top.
- **No emoji.** No exceptions.

### Audio

- Voiceover recorded with a Shure SM7B or equivalent broadcast condenser at 24-bit / 48 kHz.
- Hum-noise gated at -55 dB. No de-essing — developers can hear it.
- Optional bed: a single sustained pad at -28 dB. **No lyrics. No drums. No build.**

### Keyboard-action callouts

Every shortcut press gets a bottom-right callout rendered as if it were a chiclet key.

- **Font:** Berkeley Mono 36pt on a 1.5px outlined rounded rectangle.
- **Color:** `#0B0E14` text on `#E6EDF3` fill.
- **Duration on screen:** 1.2 seconds from the moment the key is pressed.
- Sequence shown across the demo: `Alt-p`, `Ctrl-K`, `switch worker codex`, `Alt-a`, `Esc`, `Ctrl-K`, `switch worker claude-code-acp`.

---

## What is deliberately not in this script

- **No comparison frames** ("Devin can't do this," "Cursor can't do this"). The competitive position is implied by the demo, not stated. Stating it would break the brand-voice rule from `marketing/messaging/positioning.md` line 130 ("no enterprise jargon, no over-the-shoulder selling").
- **No founder face.** This is a tool demo, not a personality demo. The audience pre-screens out founder talking heads.
- **No "introducing SPUR" beat.** The name lands once, in the closing line, under the install command. The first time the audience sees the word "SPUR" should be ~78 seconds into a 90-second video.
- **No social-proof slate.** No logos. No stars. Save it for the page below the fold.

---

## Summary

### (a) The single visual most likely to be screenshotted and shared independently

The **`insights-cross-vendor.png` still from Shot 6** — the Insights overlay showing two vendor rows (`claude-code-acp $1.42` · `codex $0.31`) and a `session total $1.73` line, with the `live` badge in the corner. Rationale:

- It is the only image in the demo where you can hand someone a single frame and they immediately understand both halves of SPUR's value prop (cross-vendor *and* live cost).
- `marketing/competitors/_summary-indirect.md:44` explicitly identifies the unified ledger as "the one differentiator no peer can address by design" — which means this still has no competing image in the category. It is uniquely SPUR.
- It survives losslessly as a 1024×512 OG card, a 1080×1080 Twitter post, and a 1200×630 LinkedIn share. The brain-badge GIF doesn't — it requires motion to read.
- Theme #2 in `marketing/research/themes.md` flags cost opacity as the "sharpest emotional language in the batch." This still is the answer-image to that pain.

Frame Shot 6 deliberately for screenshot extraction: extra padding, no overlapping subtitle, hold for 5 seconds so the YouTube preview-frame picker will land on it by default.

### (b) Product gaps that must close before this demo can record without cuts

Recording the script as written requires the following to be true in `main` today. Each row is a gap — if any is missing, the demo either cannot be recorded or must be edited to disguise a manual workaround.

| Beat | Required behavior | Status | Gap → bead |
|---|---|---|---|
| Shot 2 | Plan Inspector reachable via `Alt-p` from the dead-Claude state (after a rate-limit error has terminated the active session). | Confirmed via `docs/user-docs/04-issues-and-planning.md:72`. | None. |
| Shot 4 | Mid-task brain swap from `claude-code-acp` to `codex` via Command Palette → `switch worker`, with the new worker inheriting the prior worker's task context (the in-flight prompt + worktree state). | **Uncertain.** `docs/user-docs/01-core-navigation.md:80` mentions "switch workers" via `Ctrl-K`, but cross-vendor task-state handoff (including the in-flight prompt buffer) is not documented end-to-end. | **File: `spur-tui: verify mid-task brain swap inherits in-flight prompt + worktree state cross-vendor`** — needed before shoot. |
| Shot 5 | Codex resumes from the same task node Claude was on, including reading the same file Claude was about to write. | Depends on the gap above; event-replay claim in `marketing/product-marketing.md:78`. | Same bead. |
| Shot 6 | Insights (`Alt-a`) displays per-vendor *and* aggregate cost in a single panel, with a `live` indicator, for the **current session**, not just historical. | `Alt-a` and the analytics dashboard confirmed (`docs/user-docs/01-core-navigation.md:74`). The "two vendor rows + one session total + live badge" layout is **not confirmed**. | **File: `spur-tui: insights cross-vendor live ledger panel — confirm or build`** — needed before shoot. |
| Shot 7 | Status-bar toast `Claude window reset.` fires when the upstream vendor returns from a rate-limit window. | **Unknown.** No reference in `docs/user-docs` to a window-reset toast. | **File: `spur-tui: claude window-reset toast`** — needed before shoot. May be a polish item; alternative is to cut Shot 7 and write the line "Claude is back." as voiceover-only over the badge flip. |
| Shot 8 | Claude reads the diff Codex just wrote on the next task node, with no manual context re-priming. | Same dependency as Shots 4–5. | Same bead. |

**Net:** three beads to file before the shoot. If gap #2 (brain swap handoff) does not hold today, the demo cannot ship as recorded — and the entire `messaging/positioning.md` line *"Brain-swap mid-flow is impossible inside any of Devin / Cosine / Cursor / Aider / Claude Code"* becomes a forward-looking claim, not a present-tense one. That is the riskiest claim in the whole shoot and should be verified end-to-end before a single frame is recorded.

### (c) Single 90s video, or 30s teaser + full demo?

**Both — but ship the 90s first.** Rationale:

- A 30s teaser without the 90s anchor is a riddle. The teaser's payoff is the brain swap in Shot 4, and the brain swap reads as magic only if the viewer has been let down by the rate-limit ambush in Shot 1 — that pain beat takes 15 seconds to land. Compressing the ambush into 5 seconds turns it into a marketing claim ("you've been there") instead of a recognition. The full 90 is the load-bearing asset.
- Once the 90 exists, the 30 is one editor-day, not a re-shoot. Cut: Shot 1 (compressed to 6s, hold on the red message), Shot 4 (verbatim, the brain-badge flip is the hook), Shot 6 (verbatim, the cross-vendor ledger still is the proof), Shot 9 (closing card). Total: 28 seconds + 2-second pad = 30. Use it on Twitter/X as the auto-play preview where the audience is one swipe from gone. Use the 90 on the homepage, in HN/Reddit threads, and in cold outreach where the viewer has already chosen to click.
- A single 60s middle-ground is the worst of both. It loses the patient pain-recognition pause (which is the brand) *and* fails to fit the 30s feed-native slot. Don't ship a 60.

**Recommendation:** Record the 90 once gaps in §(b) close. Cut the 30 from the same recording in post — no second shoot. Distribute the 30 on the feed (X / LinkedIn auto-play), the 90 on the homepage, the still from Shot 6 everywhere else.
