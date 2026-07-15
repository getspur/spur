# TUI Live Journey Contract Repair Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair stale live TUI probes and make their static story contract resistant to timeout-only VHS changes.

**Architecture:** Keep `start_live_tui` session-first and express each probe's destination with shared navigation helpers from `lib.sh`. Extend `story-contract.test.sh` to enforce surface intent and use regexes for duration-independent VHS proof anchors.

**Tech Stack:** Bash, shell-use, VHS tape contracts, ripgrep, ShellCheck.

---

### Task 1: Add failing surface-contract checks

**Files:**
- Modify: `scripts/e2e/demos/tui-live/story-contract.test.sh`
- Test: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Add a regex assertion helper**

Add `assert_matches FILE REGEX LABEL`, implemented with `rg -q --regexp`, so
VHS proof checks can match any positive timeout duration.

- [ ] **Step 2: Add short-probe regression assertions**

Enumerate the eight probe scripts, forbid `wait_text "Lineage"`, forbid bare
`press_key s`, and require explicit helpers: `story_dashboard_land` for lineage,
`open_sessions_picker` for session navigation, `story_session_land` for
session-first probes, and unsent-draft confirmation/cancellation in
`composer-draft`.

- [ ] **Step 3: Verify RED**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: FAIL on stale probe surface contracts and the seven existing
duration-pinned VHS checks.

### Task 2: Repair short probe navigation

**Files:**
- Modify: `scripts/e2e/demos/tui-live/journeys/lineage-dashboard.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/sessions-picker.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/palette-open.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/session-resume.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/explore-browser.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/explore-agents-tab.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/composer-draft.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/agent-send.sh`
- Test: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Make every intended surface explicit**

Use `story_session_land`, `return_to_dashboard`, `story_dashboard_land`,
`open_sessions_picker`, and `return_to_session_detail` as appropriate. Remove
all direct assumptions that startup leaves the TUI on Dashboard.

- [ ] **Step 2: Strengthen composer draft safety**

After typing the draft, open Sessions, press Enter on the new-session target,
wait for `has an unsent draft`, press `n`, and verify the Sessions picker remains
visible. Never submit the draft.

- [ ] **Step 3: Verify probe patterns**

Run:

```bash
! rg 'wait_text "Lineage"|press_key s$' scripts/e2e/demos/tui-live/journeys/{lineage-dashboard,sessions-picker,palette-open,session-resume,explore-browser,explore-agents-tab,composer-draft,agent-send}.sh
```

Expected: exit 0 because neither stale pattern is found.

### Task 3: Make VHS proof assertions duration-independent

**Files:**
- Modify: `scripts/e2e/demos/tui-live/story-contract.test.sh`
- Test: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Replace the seven exact timeout assertions**

Use `assert_matches` with expressions such as
`Wait\\+Screen@[0-9]+s /agent=/`, retaining the exact proof anchor while allowing
timeout tuning.

- [ ] **Step 2: Verify GREEN**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: `All story-contract checks passed` and exit 0.

### Task 4: Verify, document, and commit

**Files:**
- Modify: `scripts/e2e/demos/tui-live/README.md`
- Modify: `scripts/e2e/demos/tui-live/JOURNEY_STORY_REVIEW.md`

- [ ] **Step 1: Document the repaired surface contract**

Clarify that short probes start session-first, then explicitly navigate to their
target, and record the draft-safety proof plus timeout-independent static checks.

- [ ] **Step 2: Run syntax and lint checks**

```bash
for f in scripts/e2e/demos/tui-live/lib.sh scripts/e2e/demos/tui-live/uat.sh scripts/e2e/demos/tui-live/story-contract.test.sh scripts/e2e/demos/tui-live/journeys/*.sh; do bash -n "$f"; done
shellcheck scripts/e2e/demos/tui-live/lib.sh scripts/e2e/demos/tui-live/uat.sh scripts/e2e/demos/tui-live/story-contract.test.sh scripts/e2e/demos/tui-live/journeys/*.sh
```

Expected: both commands exit 0 with no diagnostics.

- [ ] **Step 3: Review the scoped diff**

Run `git diff --` for the design, plan, live journey scripts, contract, and docs.
Confirm unrelated dirty-worktree files are absent.

- [ ] **Step 4: Commit the finished repair**

Stage only the scoped files and commit with:

```bash
git commit -m "fix(e2e-demos): repair session-first journey contracts"
```
