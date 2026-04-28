# Session Picker Recall Revamp — Design

Date: 2026-04-28
Status: Spec (pre-implementation)
Owner: TUI / Session UX
Touches: `crates/spur-tui` only (no ACP / brain / protocol changes)

## Problem

The session picker (`crates/spur-tui/src/views/session_picker.rs`) lists sessions
by an agent-generated `title` (e.g. "Build fix"). These titles carry weak
semantic weight, so users returning to the picker days later struggle to recall
which session was about what. The current preview pane (toggled with `P`) is
dominated by administrative metadata — session ID, full CWD, ISO timestamp,
pinned/archived flags — that does not help recall. The only content signal in
the preview today is a `Draft` row, and only when unsent text exists.

The picker fails the canonical recall journey at two steps:

1. **Scan** — agent titles do not differentiate sessions in the same project.
2. **Probe** (press `P`) — the preview pane shows admin metadata, not the
   user's intent or last activity.

## Goal

Replace weak agent-generated titles in the row with the user's first message
to that session ("intent recall"), and invert the preview pane so the user's
last message and unsent draft ("state recall") are the dominant elements,
with metadata relegated to a single muted footer.

Both signals must be available **without backend protocol changes** and
**without blocking the render path on disk I/O**.

## Non-goals

- No ACP protocol changes. `SessionInfo` stays as-is.
- No new persistence file. Synopsis lives in the existing
  `.spur/sessions/metadata.json` store.
- No AI-generated summaries. Snippets are verbatim slices of stored messages.
- No per-session cost rendering (out of scope; future work).
- No 2nd-line snippet under the selected row in the list (would break the
  scroll math; rejected during brainstorming).

## Decisions (locked during brainstorming)

| Question | Choice | Rationale |
|---|---|---|
| Where does enrichment go? | Hybrid: row label uses first-msg; preview pane carries last-msg, draft, count | Row stays compact; preview becomes content-first |
| Data source / persistence | Hybrid: persisted authoritative + on-demand log-scan backfill for legacy | Instant for new sessions; retroactive for old ones |
| Row label precedence | `title_override` → first-msg → agent title → cwd | Manual user rename wins; otherwise snippet beats agent auto-title |
| Per-row 2nd line for selected row | Rejected | Breaks `visible_height` math; row stays single-line |
| Preview pane height | Grow from 8 to 12 rows when `P` is on | Needed to fit wrapped first-msg + last-msg + draft + footer |

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│ Live event handler (existing)                                    │
│   on user_message_chunk      → write synopsis (push)             │
│   on assistant_message_chunk → bump msg_count (push)             │
└──────────────────────────────────────────────────────────────────┘
              │
              ▼
┌──────────────────────────────────────────────────────────────────┐
│ SessionMetadata.sessions[id].synopsis = Option<SessionSynopsis>  │
│   persisted via existing debounced metadata save                 │
└──────────────────────────────────────────────────────────────────┘
              ▲
              │
┌──────────────────────────────────────────────────────────────────┐
│ Backfill task (new module: session_synopsis.rs)                  │
│   triggered on SessionPickerView::set_sessions()                 │
│   for sessions where synopsis.is_none()                          │
│   reads .spur/events/*.ndjson off the render thread              │
│   writes synopsis back via Action::SessionMetadataUpdated        │
└──────────────────────────────────────────────────────────────────┘
              ▲
              │
┌──────────────────────────────────────────────────────────────────┐
│ Render path (synchronous, zero I/O)                              │
│   row label   = resolve_label(session, entry)                    │
│   preview     = build_preview(session, entry)                    │
└──────────────────────────────────────────────────────────────────┘
```

The render path never reads disk. The synopsis field IS the cache.

## Data model

Add to `crates/spur-tui/src/session_metadata.rs`:

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionSynopsis {
    /// User's first message in this session, capped at 120 chars at write time.
    /// Empty Option = unknown (synopsis never populated).
    pub first_user_msg: Option<String>,
    /// Most recent USER message (not assistant), capped at 120 chars.
    pub last_user_msg: Option<String>,
    /// ISO 8601 timestamp of last_user_msg.
    pub last_msg_at: Option<String>,
    /// Total user + assistant message chunks counted.
    pub msg_count: u32,
    /// None when synopsis came from live writes; Some(ts) when backfilled
    /// from logs (used to skip re-backfill).
    pub backfilled_at: Option<String>,
}

// In SessionEntry, add:
pub synopsis: Option<SessionSynopsis>,
```

Storage rules:
- Capped at 120 graphemes at WRITE time (not read time), measured via
  `unicode-segmentation`. Render trims further to 70 graphemes for row
  display; preview shows the stored value verbatim.
- Truncation cuts at first `.`, `?`, `!`, `\n`, or 120 graphemes — whichever
  comes first. Trailing `…` appended if the cut shortened the text.
- Empty messages and whitespace-only messages are skipped (not stored).
- All timestamps stored as RFC 3339 strings (matches existing
  `last_opened_at` convention in `SessionEntry`).

## Live write path

Hook into the existing event router that handles ACP `SessionUpdate`s. On
each user message chunk for session `S`:

```text
entry = metadata.sessions.entry(S).or_default()
synopsis = entry.synopsis.get_or_insert_with(SessionSynopsis::default)
if synopsis.first_user_msg.is_none() {
    synopsis.first_user_msg = Some(truncate_120(text))
}
synopsis.last_user_msg = Some(truncate_120(text))
synopsis.last_msg_at   = Some(now_rfc3339())
synopsis.msg_count    += 1
schedule_metadata_save()  // existing debounced save
```

On each assistant message chunk: only `msg_count += 1`.

The existing metadata save path already debounces, so per-keystroke chunks
will not thrash disk.

## Backfill path

New module: `crates/spur-tui/src/session_synopsis.rs`.

Public surface:

```rust
pub struct SynopsisBackfill { /* opaque */ }

impl SynopsisBackfill {
    /// Spawn a single backfill task for sessions whose `synopsis` is None.
    /// Returns immediately. Task runs off the render thread.
    pub fn spawn(
        sessions: Vec<SessionId>,
        events_dir: PathBuf,
        result_sink: tokio::sync::mpsc::Sender<Action>,
    ) -> Self;

    /// Cancel any in-flight backfill (called on view close).
    pub fn cancel(&self);
}
```

Triggered from `SessionPickerView::set_sessions()`:

1. Identify session IDs with `entry.synopsis.is_none()` AND
   `entry.synopsis.backfilled_at.is_none()`.
2. If non-empty, spawn `SynopsisBackfill::spawn(...)`.
3. Backfill task scans `.spur/events/*.ndjson` ONCE, groups
   `AgentNotification`s by `session_id`, extracts:
   - `first_user_msg` = first `user_message_chunk.content.text` for that
     session
   - `last_user_msg` = last `user_message_chunk.content.text` for that
     session
   - `last_msg_at` = RFC 3339 timestamp of last `user_message_chunk` (from
     outer event envelope; the chunk itself has no ts)
   - `msg_count` = count of all `user_message_chunk` and
     `assistant_message_chunk` events
4. For each session it found data for, send
   `Action::SessionSynopsisBackfilled { session_id, synopsis }` over the
   result sink.
5. App handles the action by writing into metadata
   (`backfilled_at = Some(now)`) and triggering a re-render.

Backfill is fire-and-forget per picker open. At most one backfill task in
flight per picker view; cancelled on view close (e.g., user hits `Esc` to
return to dashboard).

The 128MB log rotation means backfill is best-effort: if the source events
have rotated out, the synopsis stays `None` and the row falls through to
agent title / cwd. This is acceptable.

## Row composition

Replace `resolved_title()` (line 521 of `session_picker.rs`) with
`resolve_label()`:

```rust
fn resolve_label(
    session: &SessionInfo,
    entry: Option<&SessionEntry>,
    show_cwd: bool,
) -> String {
    // 1. user-set rename wins
    if let Some(t) = entry.and_then(|e| e.title_override.as_deref())
        .filter(|t| !t.is_empty())
    {
        return t.to_string();
    }
    // 2. first-user-msg from synopsis
    if let Some(snippet) = entry
        .and_then(|e| e.synopsis.as_ref())
        .and_then(|s| s.first_user_msg.as_deref())
        .filter(|s| !s.is_empty())
    {
        return truncate_for_row(snippet, 70);
    }
    // 3. agent-generated title
    if let Some(t) = session.title.as_deref().filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    // 4. cwd basename
    if show_cwd {
        return format!("{}/", cwd_basename(&session.cwd));
    }
    // 5. fallback
    "(untitled session)".to_string()
}
```

`truncate_for_row` cuts at first sentence punctuation (`. ? !`), newline,
or 70 graphemes. Adds `…` on cut. Strips leading whitespace.

The list row stays a single line. Composition stays at line 786:

```
  > <label>                              <brain>  <relative_ts>  <short_id>
```

`<label>` is right-padded / truncated so `<brain> <relative_ts> <short_id>`
form a fixed right gutter (predictable at 80 cols).

Filter haystack (in `filtered_indices`, line 372): now
`format!("{label} {first_user_msg} {last_user_msg} {cwd} {id}")`.
Search input `/auth` matches sessions whose synopsis mentions auth even if
the resolved label does not.

## Preview pane

Replace the row-list rendering in `session_preview.rs` with a structured
layout. New `PreviewContent`:

```rust
pub struct PreviewContent {
    pub first_user_msg: Option<String>,   // wrapped, dim italic, top
    pub last_user_msg: Option<String>,    // single line, "Last (Xh ago):"
    pub draft: Option<String>,            // single line, yellow
    pub footer: Option<String>,           // muted: "27 msgs · /path · brain · id"
    pub placeholder: Option<String>,      // for [+ New session] cursor
}
```

Render order, top-to-bottom:

1. **First user message** — wrapped to pane width, dim italic
   (`Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC)`).
   Up to 4 lines tall. Truncates with `…` if longer.
2. Blank line.
3. **Last user message** — `Last (2h ago): "..."`. Single line, single-line
   truncation. Skipped if no `last_user_msg`.
4. **Draft** — `Draft: "..."`. Yellow (`Color::Yellow`). Skipped if
   `entry.draft` is empty.
5. Blank line.
6. **Footer** — single muted line:
   `27 msgs · /Users/k/spur · claude · 8f41a2c9`. Dark gray.

The conditional preview pane (rendered when `P` is on) grows from 8 to 12
rows tall to fit the wrapped first-msg + last-msg + draft + footer. Layout
in `render_populated()` (line 802) changes the `preview_height: u16 = 8`
constant to `12`. When `P` is off the layout is unchanged.

Graceful degradation when the terminal is short (`area.height < threshold`):
drop sections in priority order — first the footer, then last-msg, then
collapse first-msg to 2 lines.

## Filter / search widening

`filtered_indices` (line 318-382) currently builds:
`format!("{title} {cwd} {id}")`.

Change to:
```rust
let label = resolve_label(session, metadata.sessions.get(...), false);
let first = entry.and_then(|e| e.synopsis.as_ref())
    .and_then(|s| s.first_user_msg.as_deref()).unwrap_or("");
let last = entry.and_then(|e| e.synopsis.as_ref())
    .and_then(|s| s.last_user_msg.as_deref()).unwrap_or("");
let haystack = format!("{label} {first} {last} {cwd} {id}");
```

Nucleo matcher and scoring stay identical.

## Action / event additions

```rust
// In Action enum:
SessionSynopsisBackfilled {
    session_id: String,
    synopsis: SessionSynopsis,
},
```

App handler: writes synopsis (with `backfilled_at = Some(now_rfc3339())`)
into metadata, schedules a metadata save, and re-renders.

## Performance & invariants

- **Synchronous render:** no I/O on the render path. All synopsis access is
  in-memory `SessionMetadata`.
- **Single cache layer:** the `synopsis` field IS the cache. No second
  cache.
- **Backfill rate-limit:** at most one in-flight backfill task per picker
  view. Cancelled on view close.
- **Truncation at write time:** stored value capped at 120 graphemes;
  render trims further as needed. Avoids accidental terabyte values.
- **Visible-height math unchanged:** all rows remain one line; existing
  scroll math is preserved.
- **Per-keystroke debounce reused:** existing metadata save path already
  debounces; live-write hot path does not change disk write cadence.

## Test surface

Unit tests:
- `resolve_label` precedence (5 cases: title_override / first-msg /
  agent title / cwd / fallback). Plus empty-string and whitespace-only
  edge cases for each precedence layer.
- `truncate_for_row` — sentence boundary cut, char cap, ellipsis behavior,
  unicode grapheme correctness.
- `truncate_120` (write-time cap) — same edge cases.
- Filter haystack includes synopsis fields — search `/auth` matches a
  session whose first-msg contains "auth" but whose label does not.
- Live-write path — first user message populates `first_user_msg` AND
  `last_user_msg`; second user message updates only `last_user_msg`;
  assistant messages bump `msg_count` only.

Integration tests:
- Backfill task: given fixture `.spur/events/*.ndjson` with two sessions,
  populates `synopsis` for both and emits two
  `SessionSynopsisBackfilled` actions.
- App handler: applying `SessionSynopsisBackfilled` writes synopsis into
  metadata and schedules a save.

Snapshot tests (insta):
- Row render with: synopsis present, synopsis absent, title_override
  present.
- Preview render with: full synopsis + draft, synopsis without draft,
  empty synopsis.

## Migration / compatibility

- `SessionSynopsis` is `#[serde(default)]` and `Option`-wrapped on
  `SessionEntry`, so old `metadata.json` files load without migration.
- Sessions persisted before this change get `synopsis = None`. They are
  picked up by the backfill path on next picker open.
- If `.spur/events/` has been pruned (or never existed for some sessions),
  backfill silently leaves synopsis `None` and rows fall through to agent
  title / cwd. Acceptable.

## Open follow-ups (out of scope for this spec)

- Per-session cost in the preview footer (`spur-cost` integration).
- AI-generated semantic summary for very long sessions (would replace
  the verbatim first-message snippet).
- "Recent activity" section in preview (last 3 user messages instead of
  just the most recent one).
- Snippet redaction for sensitive prompts (probably never needed —
  metadata.json is already in `.spur/`).
