# TUI Live Journey Contract Repair Design

## Goal

Restore trust in the live TUI journey suite after the shared launcher moved its
post-start home from Dashboard to Session Detail, while keeping the short probes
useful as regression coverage and the five problem stories as the marketing
path.

## Context

`start_live_tui` intentionally cold-starts through Dashboard and then attaches a
Session Detail. Eight short probes still wait for the old `Lineage` landing, and
`sessions-picker` still presses the former dashboard shortcut `s`. Separately,
the static VHS contract pins exact wait durations, so reliability-only timeout
increases make the contract fail even when the required proof anchors remain.

## Considered approaches

1. Revert `start_live_tui` to Dashboard. This would make the old probes pass but
   undo the session-first operator model used by the five value stories.
2. Patch only the eight stale assertions. This is small, but another shared-home
   migration could silently break the probes again.
3. Keep Session Detail as home, make every probe declare its intended surface,
   and enforce those declarations in the static contract. This is the selected
   approach because it fixes the root cause and leaves a regression guard.

## Design

### Explicit surface contracts

- Session-first probes call `story_session_land` after startup.
- `lineage-dashboard` explicitly calls `return_to_dashboard` and
  `story_dashboard_land` before checking lineage/activity chrome.
- Sessions navigation always uses `open_sessions_picker`; no probe uses a bare
  dashboard shortcut.
- Probes that dismiss an overlay reassert Session Detail through
  `return_to_session_detail` and `story_session_land` instead of waiting for
  Dashboard text.

### Valuable draft-safety proof

`composer-draft` remains spend-free, but it will prove more than typing: after
creating an unsent draft it opens Sessions, attempts to switch, verifies the
unsent-draft confirmation, cancels, and confirms it remains in the picker. It
does not send, rename, archive, or otherwise mutate session metadata.

### Durable VHS contract

The static contract will match the presence of `Wait+Screen@<duration>` proof
anchors with regular expressions. It will not prescribe a particular duration;
timeout tuning is an execution reliability concern, while the contract owns the
proof text and story order.

The same contract will enumerate the eight short probes and reject stale direct
`Lineage` landing waits. It will also require the explicit navigation helpers
for Dashboard, Sessions, Explore, and draft-safety flows.

## Safety and scope

- No Rust TUI behavior changes.
- No provider/model calls are added; `agent-send` remains opt-in.
- No plan, loop, rename, pin, archive, or issue mutation is added.
- Existing journey names and VHS stems remain stable.
- Full live UAT remains an operator-run check because it opens real sessions and
  can depend on available history; static checks remain deterministic.

## Verification

1. Run the static contract before implementation and observe failures for the
   stale probes plus the existing seven timeout-pinned checks.
2. Run it after implementation and require all checks to pass.
3. Run `bash -n` and ShellCheck across the live journey scripts.
4. Verify the stale `wait_text "Lineage"` and bare `press_key s` patterns are
   absent from short probes.
