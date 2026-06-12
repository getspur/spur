# AI Sidebar Single-Tab Chat Panel - UI/UX Design Spec

- **Status:** Approved for first implementation pass
- **Date:** 2026-06-12
- **Surface:** `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`
- **Scope:** Single active notebook/app chat panel inside the existing right sidebar
- **Related:** `2026-06-09-notebook-sidebar-ai-agent-design.md`

## 1. Goal

Improve the AI Agent sidebar panel so it feels like a first-class notebook/app
assistant, not a form-heavy debug panel. The first pass is explicitly
single-tab: it polishes the active `ChatPanel` experience while preserving the
current scoped chat store and backend command contracts.

The panel should make three things obvious at a glance:

1. Which notebook/app the assistant is scoped to.
2. Which agent/session will receive the next message.
3. Whether the assistant is idle, streaming, blocked on permission, or unable to
   run.

## 2. Current UX Review

The existing `ChatPanel.tsx` is functionally complete. It already supports:

- active notebook/app scope
- agent selection
- session selection
- streaming assistant text
- tool calls and tool results
- scoped errors
- pending permission actions
- bottom composer

The main issue is hierarchy. Scope, agent, session, messages, tool activity, and
permission prompts all render as similarly weighted bordered blocks. In a
320px-wide sidebar this makes the panel harder to scan than it needs to be.

Specific issues:

- The header reads like a generic settings form. Scope should be the primary
  orientation cue; agent/session are secondary metadata.
- Native session IDs can be visually noisy in the narrow panel.
- The empty state is generic and does not explain app-aware capability.
- Tool calls are rendered as peer message cards even though they are supporting
  timeline events.
- Permission prompts are visible but do not feel like the blocking decision
  point of the turn.
- The composer is usable but disconnected from current scope and streaming
  state.

## 3. Non-Goals

- Multi-tab chat UI.
- Cross-tab transcript switching.
- New backend commands or ACP protocol changes.
- New persisted transcript model beyond existing ACP session replay.
- Replacing the generic `NotebookSidebar` shell or its 320px panel width.
- Adding a full custom select/dropdown system unless native selects become the
  implementation blocker.

## 4. Visual Direction

Use a compact operational-tool style inspired by the local PostHog design
system already present in the Open Design library:

- warm parchment surfaces instead of pure white
- sage borders and quiet filled controls
- deep olive/near-black text
- orange hover accent for interactive affordances
- 4px to 6px radii for panel UI
- dense but readable spacing
- no gradients, decorative blobs, or marketing treatment

Suggested tokens:

| Role | Value |
|---|---|
| Panel background | `#fdfdf8` |
| Secondary surface | `#eeefe9` |
| Border | `#bfc1b7` |
| Primary text | `#23251d` |
| Secondary text | `#65675e` |
| Accent hover | `#f54e00` |
| Primary action | `#1e1f23` |
| Permission background | `#fff6dc` |
| Permission border | `#d6b36e` |

Typography should stay within the existing app stack. Use 10px to 14px UI text
inside the sidebar; avoid large headings.

## 5. Layout

The panel keeps the existing sidebar host:

- content panel width: `w-80` / 320px
- rail width: 48px
- full-height flex column
- header fixed at top
- transcript scrolls in the middle
- composer fixed at bottom

Recommended vertical structure:

```text
AI Agent panel
  Scope header
    app/notebook name
    path/app hint
    agent + session controls
    status strip
  Transcript scroll area
    empty state / assistant messages
    tool timeline events
    permission block
    streaming assistant text
  Composer
    scope/status microcopy
    textarea + send button
```

## 6. Header UX

The header should communicate scope first.

Required elements:

- AI mark or bot icon.
- `Active scope` label.
- scope title:
  - app name when `appOpenInfo?.app_name` exists
  - notebook filename otherwise
- secondary hint:
  - app root when app-scoped
  - notebook path or saved-file hint when notebook-scoped
- compact agent selector.
- compact session selector.
- status strip.

The status strip should reflect current state:

| State | Copy |
|---|---|
| ready | `Ready with scoped tools enabled` |
| streaming | `Streaming in this session` |
| unsaved notebook | `Save notebook to chat` |
| no agent | `No chat agent configured` |
| permission pending | `Waiting for permission` |

Agent and session controls remain in the header but should be visually
secondary. They can still use native `select` elements in the first pass, styled
as compact input controls.

## 7. Transcript UX

The transcript should form a hierarchy:

- assistant/user messages are the primary reading content
- tool calls/results are supporting timeline events
- permission prompts are blocking action blocks
- errors are compact but visible

### Empty State

Replace `Ask the agent about this notebook.` with scoped capability copy:

```text
Ask inside this app context
The assistant can inspect notebook cells, call app tools, and update panels.
```

For a plain notebook, use:

```text
Ask inside this notebook
The assistant can inspect cells, draft edits, and explain outputs.
```

Optional quick prompt chips can be added if they are implemented as real prompt
prefill actions:

- `Explain current view`
- `Find next action`
- `Draft a cell`

### Assistant Messages

Assistant messages should use a quiet bordered surface:

- background: warm near-white
- border: sage
- 12px to 13px text
- `whitespace-pre-wrap` and `break-words`

Streaming text should look like an in-progress assistant message. Add a subtle
left accent or live status indicator so the user can distinguish partial output
from committed output.

### Tool Events

Tool calls and results should not look like chat bubbles. Render them as compact
timeline rows:

- small glyph/icon column
- title line: `Tool call: <name>` or `Tool result`
- monospace metadata line with truncated args/summary
- secondary surface background

The existing `ChatMessage.kind === "toolCall" | "toolResult"` data is enough for
this first pass.

### Permission Prompt

Permission prompts should visually interrupt the transcript without taking over
the whole panel:

- amber background and border
- title as the strongest text
- short explanatory body if available
- primary approve/allow button
- secondary deny button

Buttons should remain reachable at 30px to 36px height in the narrow panel. The
prompt must stay scoped to the current conversation and use the existing
`respondToPermission` flow.

### Errors

Errors should be compact, red-tinted transcript rows. Avoid repeated large error
cards for recurring setup failures; if the same agent-list/session-list error is
emitted repeatedly, future implementation should dedupe, but that is not a
required first-pass behavior.

## 8. Composer UX

The composer stays pinned to the bottom.

Required elements:

- status microcopy tied to active scope, for example `Ready in Notebook` or
  `Ready in Code Graph Workbench`
- textarea
- icon send button

Textarea behavior:

- disabled when there is no saved `notebookPath`
- disabled when no `selectedAgentName`
- disabled while submitting
- send button disabled while streaming

Placeholder copy:

| State | Placeholder |
|---|---|
| unsaved notebook | `Save the notebook to chat` |
| no selected agent | `Select an agent` |
| ready | `Message the agent` |

The first pass does not need new keyboard shortcuts. If Enter-to-send is added,
Shift+Enter must insert a newline.

## 9. Implementation Mapping

Primary file:

- `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`

Likely local changes:

- Replace `messageClassName` with more specific class helpers for assistant,
  tool, error, and streaming rows.
- Restructure the header markup so scope is primary and agent/session controls
  are compact secondary metadata.
- Change empty-state copy and optional prompt chips.
- Add composer status microcopy.
- Preserve all current invoke calls and store interactions:
  - `chat_agents_list`
  - `chat_sessions_list`
  - `chat_new_session`
  - `chat_switch_session`
  - `chat_turn`
  - `chat_permission_respond`
  - `applyEventForScope`
  - `clearPendingPermissionForScope`

No changes are required to:

- `stores/chat.ts`
- Rust sidebar chat manager
- Tauri command payloads
- sidebar registry

## 10. Acceptance Criteria

- The panel still works in the existing 320px sidebar width without horizontal
  overflow.
- The active scope is the most prominent header content.
- Agent and session controls remain available but no longer dominate the panel.
- Empty state explains notebook/app-aware assistant behavior.
- Tool calls/results are visually subordinate to assistant text.
- Pending permission is visually distinct and actionable.
- Composer clearly indicates what scope the next message will target.
- Existing unit tests for `ChatPanel` continue to pass, or are updated only for
  intentional copy/structure changes.
- No backend command signatures change.

## 11. Test Plan

Run focused notebook frontend tests:

```bash
scripts/spur-pnpm test -- src/ui/notebook/sidebar/ChatPanel.test.tsx
```

If style refactoring touches shared sidebar shell behavior, also run:

```bash
scripts/spur-pnpm test -- src/pages/NotebookPage.test.tsx
```

Before merging a code implementation, run the notebook frontend typecheck:

```bash
scripts/spur-pnpm run typecheck
```

## 12. Deferred Follow-Up

The approved multi-tab/session concept is deferred until after this single-tab
panel polish lands. That later pass should handle:

- visible chat scope tabs
- mounted hidden tab event routing affordances
- richer session history UI
- per-tab streaming indicators
- session rename/archive/copy actions
