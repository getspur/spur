# Chat Input: Slash Commands and `@` Mentions

**Status:** implemented
**Date:** 2026-04-13
**Owner:** TUI
**Related file:** `crates/spur-tui/src/views/session_detail.rs`

## 1. Goal & scope

Add autocomplete-driven slash commands (`/`) and resource mentions (`@`) to the
chat input in `SessionDetailView`, grounded in the Agent Client Protocol (ACP)
spec and the concrete conventions of the two ACP agents that spur talks to
today: `claude-agent-acp` (standard ACP) and `kiro-cli` (vendor extension).

### In scope (v1)

- Slash-command popup merging three sources:
  - spur-local static commands
  - standard ACP `AvailableCommandsUpdate`
  - kiro vendor extension `_kiro.dev/commands/*`
- Prefix-only-on-collision grammar with `:` as the namespace separator
  (`/spur:help` vs `/claude:help`). Bare names when unambiguous.
- `@` mentions resolving to files and directories under the session cwd.
- Trait-based `MentionSource` registry so later sources (symbols, threads,
  rules) plug in without refactor.
- Mentions serialize as ACP `ResourceLink` content blocks.
- `ProtectedRange` atoms in `InputBar` so mentions edit as single units
  (backspace deletes whole mention; arrows skip it).
- New `CompletionPopup` component, modeled after the existing `HelpOverlay`.

### Explicitly out of scope (v2+)

- User-authored workflows (e.g., `.spur/commands/*.md` templates).
- `EmbeddedResource` inlining for small files — v1 uses `ResourceLink` only.
- Mention sources for symbols, threads, rules, or diagnostics.
- Sub-argument autocomplete (`_kiro.dev/commands/options`).
- Directory-expand-to-files auto-unrolling (`@src/` → every file under `src/`).
- File-watcher-backed live index (v1 uses time-based cache invalidation).

## 2. Motivation & grounded prior art

ACP (per the schema at `github.com/zed-industries/agent-client-protocol`) says:

- Slash commands travel over the wire as plain text `ContentBlock::Text`. The
  agent interprets the leading `/`. `AvailableCommand` carries `name`,
  `description`, and an optional `UnstructuredCommandInput { hint }` that is a
  placeholder for arguments.
- Mentions are purely client UX. The client expands `@<something>` into
  `ContentBlock::ResourceLink { uri, name, … }` (cheap reference) or
  `ContentBlock::EmbeddedResource` (inlined content, gated by
  `promptCapabilities.embeddedContext`).

`claude-agent-acp` emits standard `AvailableCommandsUpdate` populated at
runtime from the Claude Agent SDK. The user-reachable catalog today includes
`/compact`, `/model`, `/clear`, `/resume`, `/continue`, `/plan`, `/review`,
`/memory`, `/config`, `/agents`, `/hooks`, `/init`, `/mcp`, `/permissions`,
`/statusline`, `/doctor`, `/export`, `/add-dir`, `/security-review`, `/bashes`,
`/pr_comments`, `/help`. The bridge itself handles `/context`, `/heapdump`,
`/extra-usage` locally and filters `/cost`, `/login`, `/logout`,
`/release-notes`, `/todos`, `/keybindings-help`, `/output-style:new` from the
advertised list. MCP-sourced commands are renamed to `mcp:<name>`.

`kiro-cli` does **not** emit standard ACP `AvailableCommandsUpdate`
(kirodotdev/Kiro#5976 is open as of 2026-03). It uses a vendor extension:

- Notification `_kiro.dev/commands/available` — pushes the catalog.
- Request `_kiro.dev/commands/execute` — structured dispatch with body
  `{command, args}`.
- Request `_kiro.dev/commands/options` — sub-argument autocomplete.

An ACP client that does not implement the vendor extension sees an empty
slash-command list when talking to kiro. Core kiro commands include `/help`,
`/context` (with `/context add`), `/model`, `/agent`.

Spur today stores commands as `SessionDetailView.available_commands:
Vec<String>` (see `session_detail.rs:35` and `app.rs:772-777`), discarding
description and hint. The current submit path wraps the InputBar text in a
single `ContentBlock::Text` in the orchestrator (`orchestrator.rs:553`).

## 3. Design overview

```
SessionDetailView
├── InputBar                         (existing; adds ProtectedRange + atoms)
├── ReactTrace                       (unchanged)
├── CompletionPopup (new)            (overlay; rendered above InputBar)
│   ├── CompletionTrigger            (detects '/' or '@'; extracts query)
│   ├── CommandRegistry              (merges CommandSources → CommandEntry)
│   │   ├── SpurLocalSource          (static commands)
│   │   ├── AcpStandardSource        (from AvailableCommandsUpdate)
│   │   └── KiroVendorSource         (from _kiro.dev/commands/available)
│   └── MentionRegistry              (merges MentionSources → MentionEntry)
│       ├── FileSource               (ignore::Walk, per-session cache)
│       └── DirectorySource          (same walker, dir-filter)
└── SubmitRouter (new)               (InputBar text+ranges → Vec<ContentBlock>
                                      + Dispatch for local/kiro commands)
```

Only `SessionDetailView` consumes these components. The trait-based registries
make both v2 sources (for mentions) and v2 workflow commands (for slash) drop
in as new source implementations.

## 4. Data model

```rust
// In spur-tui
pub struct CommandEntry {
    pub name: String,                  // "help" (no leading slash)
    pub description: String,
    pub hint: Option<String>,          // ACP UnstructuredCommandInput.hint
    pub source: CommandSource,
    pub dispatch: Dispatch,
}

pub enum CommandSource {
    Spur,
    Agent { handle: String },          // "claude", "kiro", "gemini"
}

pub enum Dispatch {
    SpurLocal(crate::action::Action),  // fire Action, close popup, no submit
    PromptText { normalized: String }, // "/help" — sent as ContentBlock::Text
    KiroExecute {                      // _kiro.dev/commands/execute
        command: String,
        args: serde_json::Value,
    },
}

pub struct MentionEntry {
    pub uri: String,                   // "file:///abs/path"
    pub display: String,               // "src/foo.rs" (relative, no leading '@')
    pub kind: MentionKind,
}

pub enum MentionKind {
    File,
    Directory,
}

// In InputBar
pub struct ProtectedRange {
    pub start: usize,                  // byte offset in `text` (inclusive)
    pub end: usize,                    // byte offset (exclusive)
    pub uri: String,
    pub name: String,
}
```

`SessionDetailView.available_commands: Vec<String>` is replaced by a
`CommandRegistry` field. The registry stores the full `Vec<AvailableCommand>`
under the hood (so description and hint survive) and yields `Vec<CommandEntry>`
on demand by merging with spur-local and kiro sources.

## 5. Data flow

### Slash command

1. User types `/` → `CompletionTrigger` detects prefix at cursor, opens
   `CompletionPopup` with `CommandRegistry.list()` filtered by the query using
   `nucleo`.
2. User navigates (Up/Down) and accepts (Enter or Tab). The popup resolves to a
   `CommandEntry`, inserts the canonical typed form into `InputBar` (`/name`
   when unique, `/<source>:<name>` on collision), and closes.
3. User presses Enter on the now-closed popup (normal submit). The new
   `SubmitRouter` parses the leading token:
   - Matches to a `CommandEntry` in the registry:
     - `SpurLocal` → fire the `Action`, push a confirmation trace entry, **do
       not send a message**.
     - `PromptText` → build `vec![ContentBlock::Text(normalized + rest)]` and
       submit through the existing send path.
     - `KiroExecute` → invoke the new spur-acp method
       `Connection::kiro_execute(session, command, args)`, push a trace entry,
       **do not send a message**.
   - No match → send the text as a single `ContentBlock::Text` (unchanged
     behavior).

### Mention

1. User types `@` → `CompletionTrigger` opens `CompletionPopup` with
   `MentionRegistry.query(prefix)` fuzzy-matched via `nucleo` against the
   cached file/dir list.
2. User accepts a candidate → popup calls
   `InputBar::insert_atom(range, uri, name)`. The InputBar replaces the
   `@<query>` text with a styled `@<name>` atom and records a `ProtectedRange`.
3. User presses Enter to submit → `SubmitRouter` walks `text` and sorted
   `ranges` interleaved, producing:
   ```
   [Text("look at "),
    ResourceLink { uri: "file:///abs/src/foo.rs", name: "src/foo.rs", … },
    Text(" and tell me what it does")]
   ```
4. Orchestrator receives the list via `Action::SendMessage` and forwards the
   blocks via the existing `PromptRequest` path in `spur-acp` (see
   `orchestrator.rs:553`).

## 6. Key routing (`handle_key` priority)

Session detail's current priorities (auth-dismiss → Alt-m → permission →
editing-key → scroll) gain a new tier between permission and editing:

```
1. Dismiss auth banner (existing).
2. Alt-m → Action::TogglePlanMode (existing).
3. Permission handling if pending (existing).
4. POPUP IS OPEN:
   - Up/Down → popup.select_prev / popup.select_next
   - Enter / Tab → popup.accept
   - Esc → popup.dismiss (keep current InputBar text)
   - Ctrl-C → popup.dismiss
   - Backspace with empty query → popup.dismiss + InputBar.backspace
   - Any printable char → InputBar.insert + popup.refilter
5. Editing keys → InputBar (existing, now ProtectedRange-aware).
   After every edit, CompletionTrigger re-evaluates and may open/close
   the popup.
6. Non-editing keys → scroll / navigate (existing).
```

The popup handles no keys unless it is open. Trigger detection runs after
every InputBar edit in tier 5.

## 7. Collision grammar

- **Separator:** `:` (e.g. `/spur:help`). Chosen to match
  `claude-agent-acp`'s own `mcp:<name>` rename convention.
- **Display:** every popup row shows `⟨source⟩` in dim text regardless of
  collision. Source tag uses the agent handle lowercased: `⟨spur⟩`,
  `⟨claude⟩`, `⟨kiro⟩`.
- **Canonical typed form:** bare `/name` when unique across all merged sources;
  prefixed `/<source>:<name>` when at least two sources define the same name.
- **Submit-time resolution order:**
  1. Explicit prefix `/<source>:<name>` → that source, always.
  2. Bare `/<name>` unambiguous → the unique source.
  3. Bare `/<name>` ambiguous → **spur-local wins** (documented).
  4. No match → plain `ContentBlock::Text`.
- **User override:** manual typing of `/claude:help` forces the
  `claude-agent-acp` variant even when spur-local would win bare.

## 8. Kiro vendor extension plumbing

New in `spur-acp`:

- Handle the inbound `_kiro.dev/commands/available` notification. Normalize
  into the same `AvailableCommand` struct used for standard ACP, tagged in
  spur-tui with `CommandSource::Agent { handle: "kiro" }`.
- Add a method on the ACP connection (tentatively
  `kiro_execute(session, command, args) -> Result<serde_json::Value>`; the
  implementation plan picks the exact surface) that issues a JSON-RPC request
  with method `_kiro.dev/commands/execute` and params `{command, args}`.
- `_kiro.dev/commands/options` is deferred. V1 kiro commands accept zero
  structured args; if the user types text after the command, spur passes
  `{ args: { raw: "<rest>" } }` best-effort. Documented limitation; structured
  arg entry arrives with the v2 `options` work.

## 9. `InputBar` `ProtectedRange` semantics

1. Ranges are sorted, non-overlapping, end-exclusive byte offsets into `text`.
2. **Backspace** inside a range, or with cursor immediately after the range's
   `end`, deletes the whole range atomically. Other backspaces behave
   normally.
3. **Delete** (forward) inside a range, or with cursor immediately before the
   range's `start`, deletes the whole range atomically.
4. **Left/Right arrows** skip a range atomically: stepping into the range
   jumps the cursor to the opposite boundary.
5. **Typing a printable char** with cursor inside a range deletes the range
   first, then inserts at the now-vacated position.
6. Any edit that changes byte positions outside a range shifts subsequent
   ranges accordingly.
7. New public method
   `insert_atom(&mut self, at: usize, text: &str, uri: String, name: String)`
   inserts text and creates a matching range atomically.
8. **Rendering:** `render()` iterates text spans, applying
   `Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)` for
   byte slices that fall inside any range.

Roughly 100 LOC of boundary math, fully unit-testable.

## 10. Indexing for mentions

- Library: `ignore` (ripgrep's walker). Respects `.gitignore`, `.ignore`,
  `.rgignore`.
- Cache: a `HashMap<SessionId, MentionIndex>` inside `MentionRegistry` where
  `MentionIndex = { paths: Vec<PathBuf>, built_at: Instant }`.
- **Invalidation:** on `@` trigger, if `built_at` is older than **60 seconds**,
  re-walk (blocking; capped by `ignore`'s filters). First trigger after
  session open blocks worst-case ~100ms on a ~10k-file repo — acceptable for
  v1.
- **Fuzzy matching:** `nucleo::Matcher` with defaults. Rank by descending
  score, with a secondary ascending sort by path length (shorter paths =
  typically more relevant).
- **Display:** top 20 matches. Relative paths. Directories render with a
  trailing `/`.

## 11. Popup UI

- Style: extends the `HelpOverlay` pattern — `Clear` widget beneath, then a
  `Block::bordered()` rectangle.
- Placement: **above** `InputBar`. Rect is computed as
  `x = InputBar.x + 2` (aligned to caret column),
  `y = InputBar.y - popup_height`,
  `height = min(visible_entries, 8) + 2`,
  `width = min(max_entry_width, area.width / 2)`.
- Row format: `<icon>  /name<match-highlights>  description…  ⟨source⟩`,
  truncated with `…` to fit width.
- Match highlighting: bold the nucleo-reported match positions.
- Selected row: inverse style.
- Empty state: `"No matches. Type to refine, Esc to dismiss."`.

## 12. Action & submit-pathway changes

Current:

```rust
Action::SendMessage { session, text: String, interrupt: bool }
```

New:

```rust
Action::SendMessage { session, blocks: Vec<ContentBlock>, interrupt: bool }
```

`orchestrator.rs:553` currently wraps `text` in
`vec![ContentBlock::Text(TextContent::new(text))]`. Update to forward `blocks`
directly to `Connection::prompt`. Every other programmatic `Action::SendMessage`
call site wraps its text into a single-`Text`-block vector at the call site
— one-line change per site.

Local-command dispatch introduces no new action; local commands reuse
existing `Action` variants (for example `Action::TogglePlanMode` for
`/mode plan`).

## 13. Error handling

- **Cache walk error** (e.g. permission denied on a subdir): log via
  `tracing::warn` with the offending path, skip the entry, continue.
- **Kiro execute failure:** surface as a `TraceKind::Observe` entry prefixed
  `⟨kiro⟩ command failed: <error>`.
- **Unknown `/<source>:<name>` prefix** (source not known): fall through to
  plain-text rule (order 4 in §7); no error dialog.
- **File URI invalid chars:** percent-encode in `uri`; `name` keeps a
  human-readable relative path.
- **ProtectedRange invariant violated** (should never happen in practice):
  `debug_assert!` in debug builds; recover in release by stripping all ranges
  and submitting the text as a single `ContentBlock::Text`.

## 14. Testing strategy

- **`InputBar` unit tests:** 20+ synthetic key-sequence cases covering
  insert, backspace, forward-delete, left/right arrow, and paste crossing
  `ProtectedRange` boundaries.
- **`SubmitRouter` unit tests:** collision resolution order (spur-local vs
  agent with identical names, explicit `/source:name` override, ambiguous
  bare, no-match fallthrough), local-Action dispatch, kiro-execute dispatch,
  and mention-interleaved content-block assembly.
- **`CompletionTrigger` unit tests:** `/` fires only at start of line (or
  after whitespace — define explicitly in implementation); `@` fires after
  whitespace; both close on space or on backspace over the trigger char.
- **`MentionRegistry` unit tests:** cache TTL behavior, `nucleo` scoring
  against a fixed path set, directory vs file distinction.
- **Integration test:** end-to-end submit of `"look at @src/foo.rs"` produces
  `[Text, ResourceLink, Text]` in the orchestrator.
- **Manual smoke test:** the plan (next document) records exact keystroke
  sequences and expected trace output.

## 15. Implementation phasing (for the plan document)

1. Replace `SessionDetailView.available_commands: Vec<String>` with a
   `CommandRegistry` holding full `Vec<AvailableCommand>`; no UI change yet.
2. `CommandRegistry` + `SpurLocalSource` + merger; still no popup.
3. `CompletionPopup` component (rendering only, against a fixed list).
4. `CompletionTrigger` + popup open/close + `nucleo` filtering for slash
   commands.
5. `Dispatch` + `SubmitRouter`; rewire `Action::SendMessage` to carry
   `Vec<ContentBlock>`.
6. Kiro vendor extension in `spur-acp`: inbound `available` + outbound
   `execute`.
7. `MentionRegistry` + file and directory sources + `ignore`-based indexing.
8. `InputBar::insert_atom` + `ProtectedRange` semantics + tests.
9. Mention integration with popup + submit-time slicer.

Each step is independently mergeable.

## 16. Open questions (to resolve during planning, not blocking design)

- Should `/` fire only at byte offset 0 of the InputBar text, or also after
  any whitespace? V1 recommendation: **offset 0 only**, to match Zed; widen
  later if users ask.
- Should the InputBar interpret `!`-prefix interrupt (today's behavior at
  `input_bar.rs:81`) when the prefix-before-`!` is a mention or a slash
  command? V1 recommendation: **keep `!`-prefix at byte offset 0 only**,
  unchanged from today.
- Should the local `/mode` command accept `plan` / `default` as an argument
  (e.g. `/mode plan`), or toggle like Alt-m? V1 recommendation: **accept
  both** — with no argument, toggle; with `plan` or `default`, set
  explicitly.

## 17. Local slash-command set (v1)

| Command | Effect | Rationale |
|---|---|---|
| `/help` | Open `HelpOverlay` | Collides with agent `/help`; spur wins bare. |
| `/mode [plan\|default]` | Dispatch `Action::TogglePlanMode`-equivalent | Discoverable alternative to Alt-m. |
| `/cost` | Push trace entry with `self.cost` | `claude-agent-acp` filters `/cost`, so no collision; useful for kiro too. |
| `/quit` | Exit spur | Ubiquitous convention; no known agent defines it. |

`/clear` is deliberately **not** defined locally, because `claude-agent-acp`
already ships `/clear` (resets agent context) and a spur-local "clear the
trace view" would be confusing. If that feature is desired, we can add
`/clear-view` in a follow-up.
