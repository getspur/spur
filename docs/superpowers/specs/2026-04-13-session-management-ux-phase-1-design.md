# Session Management UX Redesign — Phase 1 (Single Session)

**Status:** Approved via brainstorming, ready for implementation planning
**Scope:** Phase 1 — single session per spur instance with excellent switch UX. Multi-session / tabs deferred to Phase 2.
**Related:**
- Phase 2 (deferred): multi-session concurrency with tab bar
- Prior work: `/` commands + `@` mentions, quit-confirm dialog, proper-ACP graceful shutdown

## Goal

Make the session-switching experience in single-session spur feel **fluid, safe, searchable, and recoverable**. Fix latent bugs in session-lifecycle state handling. Build the foundation (metadata store, picker-as-hub) that Phase 2 reuses unchanged.

## Problem statement

Spur's current session manager is functional but crude:
- **Switch is destructive and slow:** resuming a session tears down and respawns the brain; any in-flight draft is lost.
- **No way to start fresh while attached:** if a brain is active, dashboard typing routes to it — there's no affordance to spawn a new blank session without quitting spur.
- **Picker is bare:** no search, no filter, no preview, no rename, no delete, no pin. For repos with >20 sessions it degrades to linear scrolling.
- **No memory across invocations:** spur doesn't know which session the user was last in, forcing a manual find-and-resume on every start.
- **Latent data-integrity bug:** `pending_user_messages` (app.rs:348) replays buffered text into whatever session spawns next, not necessarily the one the user typed into.
- **Misleading placeholders:** Dashboard's `SendMessage` ships a fresh UUID (`SessionId::new()`) that the orchestrator ignores (dashboard.rs:328). Real intent is "new session" but the code lies about it.
- **Help overlay documents a non-existent `i` keybinding.**

## UX principles (the invariants)

All design decisions below reduce to these five principles:

1. **Single path, visible state.** One surface for session management: the picker. No competing palette / sidebar / modal. One muscle memory.
2. **Search-first.** `/` focuses a visible search field; fuzzy-narrows the list in real time. No hidden modes.
3. **Inline actions.** Rename, delete, pin happen on the highlighted row without modal dialogs.
4. **Non-destructive by default.** Drafts persist across switches via metadata; archive is reversible; hard delete is deferred to a future PR.
5. **Contextual landing.** Auto-resume last session on startup with a dismissible banner. No session → Dashboard empty state with placeholder.

## Architecture

### Components (new or significantly changed)

| Component | Role | Status |
|-----------|------|--------|
| `spur-tui::views::session_picker::SessionPickerView` | Hub picker (existing, redesigned) | Redesigned |
| `spur-tui::components::session_picker_search` | Inline search field at top of picker | New |
| `spur-tui::components::session_picker_prompt` | Inline rename/confirm prompt at bottom of picker | New |
| `spur-tui::components::session_preview` | Preview pane (first + last turn) | New |
| `spur-tui::components::resume_banner` | Top-of-session banner shown on auto-resume | New |
| `spur-tui::metadata` or `spur-core::session_metadata` | `.spur/session_metadata.json` CRUD | New module |
| `spur-tui::views::session_detail` | Draft persistence integration | Modified |
| `spur-tui::views::dashboard` | Empty-state placeholder, no behavior change when brain attached | Modified |
| `spur-tui::app` | Auto-resume on startup, landing logic, draft routing | Modified |
| `spur-cli::main` | Startup flow reads metadata to decide auto-resume vs picker vs dashboard | Modified |

### Data model — `.spur/session_metadata.json`

```json
{
  "version": 1,
  "last_active_session_id": "abc12345-…",
  "last_active_at": "2026-04-13T18:42:00Z",
  "sessions": {
    "abc12345-…": {
      "title_override": "Refactor auth module",
      "last_opened_at": "2026-04-13T18:40:15Z",
      "draft": "Can we also add the retry logic before…",
      "pinned": true,
      "archived": false
    },
    "def67890-…": {
      "title_override": null,
      "last_opened_at": "2026-04-13T16:12:00Z",
      "draft": "",
      "pinned": false,
      "archived": false
    }
  }
}
```

**Field semantics:**
- `title_override`: user-provided title via rename; null means use agent-provided title or cwd basename (existing fallback).
- `last_opened_at`: updated on every session open and on every user turn.
- `draft`: last unsent InputBar text; empty string = no draft. Debounced 500ms after keystroke + flushed on switch/quit.
- `pinned`: boolean; pinned sessions sort first.
- `archived`: boolean; archived sessions hidden by default, visible via `a` toggle.

**File-write strategy:** write to `session_metadata.json.tmp` then atomic rename. Prevents partial corruption on crash.

**Orphan GC:** on picker load, compare metadata session ids with agent's `list_sessions` response. Drop entries whose session no longer exists agent-side.

### Landing flow

```
spur watch
   │
   ▼
┌──────────────────────────────────────┐
│ Read .spur/session_metadata.json     │
│  (missing file → treat as no state)  │
└──────────────────────────────────────┘
   │
   ├── last_active_session_id exists ──▶ try agent.load_session(id)
   │                                      ├── success ─▶ auto-resume + show banner
   │                                      └── failure ─▶ fall through to picker
   │                                          (agent down, session deleted, etc.)
   │
   ├── sessions exist, no resumable "last active" ──▶ open picker
   │                                                   cursor on most-recent live session
   │
   └── no sessions ──▶ Dashboard with empty-state placeholder:
                      "Type to start a new session · `s` sessions · `?` help"
```

**CLI overrides (escape hatches):**
- `--session <id>` — resume specific session (existing behavior)
- `--sessions` — force-open picker, overrides auto-resume
- `--dashboard` (new) — force Dashboard landing, for users who want old behavior

## The picker layout

```
┌─── Sessions ─ claude-code-acp ──── /Volumes/Projects/spur ────────────┐
│                                                                        │
│  Search  ⌊ _________________________________ ⌋     (press / to focus) │
│                                                                        │
│  ▸ + Start new session                                                 │
│  ────                                                                  │
│  ⭐ abc12345 · Refactor auth module                                    │
│       14m ago · draft: "Can we also add the retry logic…"              │
│                                                                        │
│    def67890 · Debug race condition                                     │
│       2h ago                                                           │
│                                                                        │
│    ghi24680 · (untitled)                                               │
│       yesterday · /other-repo                                          │
│                                                                        │
├── j/k nav · Enter resume · / search · n new · R rename · d archive    │
│    · a show-archived · p pin · P preview · r refresh · Esc back ──────┤
└────────────────────────────────────────────────────────────────────────┘
```

### Layout zones

| Zone | Position | Content |
|------|----------|---------|
| Header | top | `Sessions · <agent> · <cwd>` + `[showing archived]` flag when `a` is on |
| Search field | below header | Always visible; `/` focuses; typing narrows list |
| `[+ Start new session]` row | first list entry | Persistent; `Enter` or `n` spawns |
| Session rows | main list | Sorted: pinned first → live (recency desc) → archived (when `a`) |
| Preview pane | bottom third | Appears only when `P` toggled on |
| Inline prompt | bottom bar | Rename target / confirm banner — appears contextually |
| Footer hint | below everything | Current keybinding legend |

### Session row content

- `▸` cursor marker (only on highlighted row)
- `⭐` if pinned
- Short session id (first 8 chars, cyan)
- `·` separator
- Title — `title_override` > agent-provided title > `(untitled)`
- Sub-line: `<relative time> [· draft: "<first 50 chars>"] [· <cwd if cross-repo>] [· archived]`

Sub-line badges are space-separated, optional — only shown when applicable.

## Interaction reference

### Navigation

| Key | Behavior |
|-----|----------|
| `j` / Down | Move cursor down |
| `k` / Up | Move cursor up |
| `g` | Jump to top |
| `G` | Jump to bottom |
| `Enter` on live row | Resume session (with draft-switch safety if needed) |
| `Enter` on `[+ Start new session]` | Spawn new session (with draft-switch safety if needed) |
| `Esc` | If search has filter → clear filter. Else → return to previous view (Dashboard). |

### Session management

| Key | Behavior |
|-----|----------|
| `n` | Shortcut for `[+ Start new session]` (jump-to-top + Enter) |
| `R` | Rename highlighted row — opens bottom-bar prompt pre-filled with current title; Enter commits to `title_override`, Esc cancels |
| `d` on live row | Archive — sets `archived: true`; row disappears (or dims if `a` toggle is on) |
| `d` on archived row (when `a` on) | Unarchive — sets `archived: false` |
| `p` | Toggle pin on highlighted row |
| `a` | Toggle "show archived" — header shows `[showing archived]` flag |
| `r` | Force-refresh session list (bypass cache) |

### Search

| Key | Behavior |
|-----|----------|
| `/` | Focus search field |
| (typing) | Fuzzy-narrow list via nucleo-matcher on title + cwd + session_id |
| `Esc` in search field | Return focus to list, **keep filter active** |
| `Esc` in list with filter | Clear filter |
| `Esc` in list without filter | Exit picker back to Dashboard |

### Preview

| Key | Behavior |
|-----|----------|
| `P` | Toggle preview pane on/off |
| While on | Preview content updates as cursor moves; cached per-session within picker lifetime |
| On `[+ Start new session]` row | Shows "Press Enter to start a new session · any unsent draft will be saved" |
| On picker close | Preview state resets (next open starts off) |

## Safety flows

### Draft preservation on switch

Triggered when `Enter` on a different session or `n` to start new, AND the current session has non-empty `draft` OR the active InputBar has unsent text.

1. Flush any in-memory InputBar text to `session_metadata.json[current_session].draft`.
2. Show inline confirm banner at top of picker list: `Session "X" has an unsent draft — save and switch? [y/N]`
3. `y` or `Enter` → proceed with switch (draft already saved).
4. `n` / `Esc` → cancel switch, return to list cursor.

### Draft restoration on resume

When a session is opened (from picker or auto-resume):
1. Read `session_metadata.json[session_id].draft`.
2. If non-empty, pre-fill the SessionDetailView's InputBar with it. Place cursor at end.
3. User can continue typing, send, or clear.
4. On any InputBar change, debounced-write back to metadata (500ms after last keystroke).

### Auto-resume banner

Shown at the top of `SessionDetailView` when the session was auto-resumed on startup:

```
┌ Resumed: Refactor auth module · quit 2m ago · [s] picker · [n] new · [Esc] dismiss ─┐
```

- Auto-fades after 3s or on first keystroke.
- Banner dismissal on keystroke is **passive** — the keystroke still routes normally to the view (typing goes to InputBar; navigation keys navigate). Banner absorbs nothing, just fades.
- `s` / `n` / `Esc` are the only keys that are handled *specifically* while the banner is visible; all other keys dismiss-and-pass-through.

## Latent bug fixes

### BUG-1: `pending_user_messages` cross-session replay

**Current (app.rs:348-350):** buffered text is a flat `Vec<String>`; drained into whichever session spawns next, even if that's a different session than the user typed into.

**Fix:** key buffered messages by intended session id. When a user's typed text predates session creation (only happens from Dashboard typing with no brain attached), the intent is "spawn a new session with this as first message" — so the buffer is tied to the NEW session created, not the first session that happens to spawn.

Concretely: `pending_user_messages: Option<Vec<String>>` dropped; replaced with direct routing through the orchestrator's `InteractiveInput::Message { session, .. }` which already has correct routing when `session` is properly known.

### BUG-2: `SessionId::new()` placeholder in Dashboard `SendMessage`

**Current (dashboard.rs:328):** Dashboard's InputBar `Enter` emits `SendMessage { session: SessionId::new(), ... }` — a fresh UUID the orchestrator ignores.

**Fix:** introduce `Action::NewSessionWithMessage { blocks, interrupt }` distinct from `SendMessage { session, blocks, interrupt }`. Dashboard emits `NewSessionWithMessage` when no brain is attached; orchestrator handles it as "spawn brain + prompt with blocks" atomically. When brain IS attached, Dashboard emits real `SendMessage` with the active session id.

No more lying UUIDs. Intent is explicit in the action variant.

## Micro-polish

| Item | Change |
|------|--------|
| Dashboard empty-state | InputBar shows placeholder: `Type to start · s sessions · ? help` |
| Help overlay fix | Remove the non-existent `i   Chat with brain` line |
| Footer hint on picker | Current keybinding line visible at all times |

## Out of scope (Phase 2 or later)

- Multi-session concurrency / tab bar
- Multi-brain (claude + kiro simultaneously)
- Hard delete (`D` key) — documented escape hatch: `rm` agent's on-disk session file
- Session tags / categories beyond pin+archive
- Cross-repo session listing (picker is cwd-scoped)
- Split view (dashboard lineage inline in session detail)
- Transcript export / fork-from-turn
- Session-level lock file (two spur instances on same session)
- Agent crash UX (red banner + `R` to restart) — orthogonal, separate PR

## Testing strategy

### Unit tests

- **Metadata store:** CRUD roundtrip, atomic-rename on write, malformed JSON recovery, orphan GC on load
- **Sort comparator:** pinned > live (recency desc) > archived; tie-breaking by session id
- **Fuzzy search:** reuse existing nucleo-matcher tests; verify title + cwd + session_id fields all searchable
- **Draft debounce:** 500ms debounce behavior verified with deterministic timer
- **Landing decision:** auto-resume vs picker vs dashboard logic given metadata fixtures

### Integration tests (spur-tui)

- **Picker keybindings:** every key (j/k/g/G/n/R/d/p/a/r/P/Esc/Enter) produces expected action in appropriate state
- **Rename roundtrip:** `R` → type → Enter → metadata updated → row re-renders with new title
- **Archive roundtrip:** `d` hides row; `a` shows archived section; `d` on archived unhides
- **Pin roundtrip:** `p` adds ⭐ + re-sorts; `p` again removes
- **Search roundtrip:** `/` focuses field; typing narrows; Esc keeps filter then clears
- **Draft preservation:** type in session A, switch to B with `s`+Enter on B, verify confirm banner, `y` saves A's draft; resume A, InputBar pre-filled
- **Auto-resume:** fixture with `last_active_session_id` → startup resumes + banner shown; banner dismisses on keystroke

### Regression tests

- Quit-confirm flow still works (we added this recently)
- `pending_user_messages` fix: verify message typed on Dashboard during brain-spawn races into the correct session
- `SessionId::new()` replacement: verify `NewSessionWithMessage` action path

## Success criteria

1. **"Continue yesterday's work"** — `spur watch` → 0 keystrokes (auto-resume) or 1 keystroke (Enter in picker with last session highlighted).
2. **"Find a specific session among 50"** — `/` + fragment + Enter ≤ 6 keystrokes.
3. **"Fork off new work without losing draft"** — `s` + Enter on `[+ New]` + `y` = 3 keystrokes, draft preserved.
4. **No data loss:** a user with a 2-paragraph draft who switches sessions via any path has their draft recoverable by resuming the original session.
5. **No lingering processes:** existing Phase-0 guarantee (proper ACP shutdown) extended to every session-switch path.
6. **Phase 2 compatibility:** every artifact built here (metadata schema, picker, preview, drafts, rename, archive, pin) works unchanged when multi-session tabs are layered on top.
