# SPUR — Press Screenshots List

Eight screenshots we will provide on request. Each is captured at 2560×1600 (Retina), exported as PNG with no compression artifacts, and offered in both dark-theme and light-theme variants where applicable. Terminal screenshots use a 16px monospaced font for legibility at print sizes.

Each entry below: **what's in the frame**, **why it matters**, **framing notes** for whoever captures it.

## 1. Insights cost ledger

- **In frame:** SPUR Insights panel showing live cross-vendor spend — Claude, Codex, Gemini, OpenCode, Kimi — with per-session and weekly totals, broken down by model.
- **Why it matters:** This is the screenshot that proves the cost-ledger claim in one image. It is the highest-leverage frame in the kit.
- **Framing notes:** Use a real session with at least three vendors active and visible spend on each. Do not redact dollar amounts; they're the proof. Crop to show the panel plus enough chrome that the viewer knows it's a terminal, not a dashboard.

## 2. Lineage tree

- **In frame:** Collapsible ASCII tree of an in-flight plan — brain at root, workers as children, each annotated with agent name, status (running / waiting-review / approved), and elapsed time.
- **Why it matters:** Shows the "control tower" thesis better than any prose. Reporters who don't run terminals will still understand the hierarchy at a glance.
- **Framing notes:** Use a plan with 4–6 workers across at least two different agent vendors. At least one worker should be `waiting-review` so the next screenshot (review card) has a natural narrative bridge.

## 3. Review card

- **In frame:** Approve / Reject / Modify / Retry surface for a completed worker attempt, with the diff visible in the top half and the four action keys in the bottom half.
- **Why it matters:** The review-as-state-machine claim concretized. Also: the diff in the frame should be readable — pick a diff that tells a small story (a real bugfix or refactor).
- **Framing notes:** Choose a diff that's 15–25 lines, syntax-highlighted, with a clear before/after. Avoid diffs containing anything that looks like an API key or customer data.

## 4. Plan inspector

- **In frame:** DAG view of a multi-task plan — nodes for each task, edges for dependencies, color-coded by status.
- **Why it matters:** Shows that plans are a real data structure SPUR reasons about, not a chat transcript.
- **Framing notes:** 5–8 node plan minimum. Include at least one parallel fan-out and one join. Show one node mid-execution and one node already cherry-picked onto the staging branch.

## 5. Brain-swap moment

- **In frame:** Two stacked terminal frames or a split — the upper showing a Claude rate-limit notice, the lower showing the same plan resumed on Codex with the previous context loaded. A status-bar element should indicate the brain switch.
- **Why it matters:** This is the single most uniquely-SPUR moment we can capture. No competitor can fake this image.
- **Framing notes:** This may require a staged capture — real rate limits don't time well. That's fine; the configuration shown must still be a real working flow, not a mockup.

## 6. Telegram approval

- **In frame:** Phone screen (iOS or Android — pick one and stay consistent across the kit) showing the SPUR Telegram bot with a review card and inline Approve / Reject buttons.
- **Why it matters:** Defends the "review on the go" promise visually. Pairs naturally with any quote about reviewing on a phone.
- **Framing notes:** Use a real Telegram client, not a mockup. Time on the phone status bar should match (or be omitted from) the desktop screenshots. Diff content visible in the message should match screenshot #3 if used in the same story.

## 7. Install-script terminal

- **In frame:** A terminal session running `curl -sSL getspur.dev/install.sh | sh`, with the install completing and `spur --version` running successfully immediately after.
- **Why it matters:** Proves the install promise in one frame. Useful for reporters who want to convey "this is one command."
- **Framing notes:** Capture as a static PNG and as a recorded `.cast` (asciinema) — some publications will prefer the recording. Show the full command, the install output, and the version line. Do not edit the timestamps.

## 8. Pricing page

- **In frame:** The `getspur.dev/pricing` page with all four tiers visible (Community, Pro, Team, Enterprise) and the monthly/annual toggle in the position the reporter is likely to want.
- **Why it matters:** Reporters covering pricing want to grab this without manual screenshotting.
- **Framing notes:** Provide one capture in light mode and one in dark mode. If the page is still in flux at the time of a press request, capture once the prices have settled and store the date in the filename.
