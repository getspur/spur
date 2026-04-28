# SPUR TUI Keybindings — Complete Reference Map

**Date**: 2026-04-28
**Scope**: `crates/spur-tui/src/`
**Method**: Six parallel exploration passes over app routing, dashboard, session_detail, session_picker, secondary views, and input_bar component.

---

## 0. How to read this document

The TUI has **multiple layers of key routing** stacked on top of each other. A keystroke flows through this pipeline:

```
crossterm KeyEvent
  → app.rs modal-overlay intercepts (quit/collision/upgrade/help/palette)
    → app.rs global hotkeys (Ctrl+K, Ctrl+C/Q chord)
      → ViewId dispatch → active view.handle_key()
        → view-level mode routing (Navigate / Compose / Search / Rename / …)
          → KeyOwner decision (Picker / Composer / View)
            → Composer → input_bar.handle_key()
                          → editor-mode dispatch (Vim Normal/Insert/Visual/Operator | Emacs)
            → Picker  → completion.handle_picker_key() → PickerShell.handle_key()
            → View    → handle_view_key() → action emission
        → Action returned to app.rs process_action()
```

A given physical key may be claimed at any layer. **Higher layers strictly preempt lower ones.** Understanding which layer owns a key in a given state is the prerequisite to debugging UX bugs (see §11).

---

## 1. App-level routing (`app.rs`)

All cross-view routing lives in `crates/spur-tui/src/app.rs`. `lib.rs` and `tui.rs` carry no key handling. `landing.rs` is a pure data enum.

### 1.1 Truly global (work in any view, any mode)

| Key | Modifiers | Precondition | Action | file:line |
|---|---|---|---|---|
| `c` / `q` | Ctrl | No modal open | Open quit-confirm dialog (1st press) | `app.rs:909` |
| `c` / `q` | Ctrl | Quit-confirm dialog open | **Confirm quit immediately** | `app.rs:857` |
| `k` | Ctrl | No higher overlay open | Open command palette | `app.rs:980` |

`is_quit_chord` defined at `app.rs:2783` matches `Ctrl+C` or `Ctrl+Q`.

### 1.2 Modal overlay precedence (highest first)

Each overlay short-circuits the dispatcher and returns before the active view sees the key.

#### 1.2.a Quit-confirm dialog (`app.rs:856–872`)

| Key | Action |
|---|---|
| `y` / `Y` / Enter | Confirm quit |
| `n` / `N` / Esc | Dismiss dialog |
| Any other | Swallowed (no-op) |

#### 1.2.b Collision modal (`app.rs:874–902`)

Triggered when the user attempts a session action that conflicts with current state.

| Key | Action |
|---|---|
| Esc | Dismiss |
| `n` / `N` | Dismiss + `NewSessionRequested` |
| `p` / `P` | Dismiss + `RequestSessions` (open picker) |
| Enter | Dismiss + `ResumeSession` with stored ACP id |
| Any other | Swallowed |

#### 1.2.c Upgrade modal (`app.rs:919–942`)

| Key | Action |
|---|---|
| Esc / `q` | Dismiss |
| `s` | Dismiss + show "spur auth status" warning banner |
| `l` | Dismiss + show "spur auth login" warning banner |
| Any other | Swallowed |

#### 1.2.d Help overlay (`app.rs:944–953`)

| Key | Action |
|---|---|
| `?` / Esc | Close |
| Any other | Swallowed |

#### 1.2.e Palette overlay (`app.rs:955–976`)

All keys forwarded to `palette_state.handle_key`. On `PaletteIntent::Accept(result)` → dismiss + `result_to_action(result)`. On `Dismiss` → dismiss only.

### 1.3 View dispatch (`app.rs:995–1056`)

Once all overlays are clear, the key flows to the active `ViewId`'s `handle_key`:

| ViewId | Target | file:line |
|---|---|---|
| `Dashboard` | `dashboard.handle_key_with_worker_streams(key, &lineage, &mut worker_streams)` | `app.rs:996` |
| `SessionDetail(_)` | `session_detail.handle_key(key, &ctx)` | `app.rs:1001–1006` |
| `SessionPicker` | `session_picker.handle_key(key, &ctx)` | `app.rs:1008–1011` |
| `PlanInspector(_)` | `plan_inspector.handle_key(key, &ctx)` | `app.rs:1012–1015` |
| `IssueBrowser` | `issue_browser.handle_key(key, &ctx)` | `app.rs:1016–1019` |
| `MermaidOverlay(_)` | Special: `[`/`]` cycle diagrams; else `viewer.handle_key(key, &ctx)` | `app.rs:1021–1041` |

#### 1.3.a MermaidOverlay app-level intercept

| Key | Action |
|---|---|
| `[` | Cycle to previous diagram |
| `]` | Cycle to next diagram |

### 1.4 Post-view dispatch (`app.rs:1043–1056`)

| Key | Precondition | Action |
|---|---|---|
| Esc | `user_warning` banner up AND view returned `NavigateBack` or `NavigateTo(Dashboard)` | Dismiss banner instead of navigating |

### 1.5 ViewId transitions (`process_action`, `app.rs:1596+`)

| Action | Source view | Target |
|---|---|---|
| `NavigateTo(Dashboard)` | any | Dashboard |
| `NavigateTo(SessionDetail)` | any (if exists) | SessionDetail |
| `NavigateTo(SessionPicker)` | any | SessionPicker |
| `NavigateTo(PlanInspector)` | any | PlanInspector (lazy-init) |
| `NavigateTo(IssueBrowser)` | any | IssueBrowser (lazy-init) |
| `NavigateTo(MermaidOverlay)` | any | MermaidOverlay (lazy-init) |
| `NavigateBack` | MermaidOverlay | SessionDetail |
| `NavigateBack` | PlanInspector | SessionDetail |
| `NavigateBack` | IssueBrowser | Dashboard |
| `NavigateBack` | Dashboard (session exists) | SessionDetail |
| `NavigateBack` | other | Dashboard |

---

## 2. Dashboard view (`views/dashboard.rs`)

The most complex view. Has Navigate/Compose modes × vim/emacs editor modes × multiple panels (Agents/Detail/Log) × DetailPane tabs (Stream/Artifacts/Attempts/Task/Review) × focused-vs-not.

### 2.1 Cross-mode handlers (fire regardless of mode)

| Key | Modifiers | Action | file:line |
|---|---|---|---|
| `p` | Ctrl | `input_bar.history_prev()` (no Action) | `dashboard.rs:1408` |
| `n` | Ctrl | `input_bar.history_next()` (no Action) | `dashboard.rs:1412` |
| `i` | Alt | `Action::ToggleVimMode` | `dashboard.rs:1416` |
| `o` | Ctrl | Routed to View; falls through `handle_view_key` as `_ => None` | `dashboard.rs:901, 1395` (DEAD — see §11.2) |

### 2.2 Picker routing (`KeyOwner::Picker`, `dashboard.rs:879–894`)

When `completion.is_active()`:
- **Trigger-driven picker** (e.g. `/cmd`, `@mention`): `Up/Down/Esc/Tab/Enter/Ctrl+C/Ctrl+P/Ctrl+N` → picker.
- **Non-trigger-driven picker**: all keys → picker.

Picker internals: §8.

### 2.3 Compose mode (`dashboard.rs:911–921`)

Almost all keys → `KeyOwner::Composer` → `input_bar.handle_key()`.

| Key | Precondition | Action |
|---|---|---|
| Esc | `input_bar.wants_esc()` is true (Vim Insert/Visual/Operator) | Composer (vim handles Esc internally) |
| Esc | `input_bar.wants_esc()` is false | View → `mode = Navigate`, return `None` |

### 2.4 Navigate mode — routing logic

The Navigate branch was patched on 2026-04-28 (commit `bd5c4b5c`, F8). Current shape (`dashboard.rs:923–948`):

```rust
DashboardMode::Navigate => match key.code {
    KeyCode::Char(c) if self.input_bar.is_vim_normal() => {
        if self.is_view_action_char(c) { KeyOwner::View }
        else if matches!(c, 'i' | 'a' | 'A' | 'I' | 'o' | 'O') { KeyOwner::Composer }
        else { KeyOwner::View }
    }
    KeyCode::Char(c)
        if !self.input_bar.is_vim_normal() && !self.is_view_action_char(c) => KeyOwner::Composer,
    _ => KeyOwner::View,
}
```

`is_view_action_char` (`dashboard.rs:954–968`):
- Always: `j k g G r v ? s q z N P`; plus `c` when `focused_panel == Agents`.
- When `focused_node.is_some()`: also `h l o`; plus `A D M R` when `current_tab == Review`.

### 2.5 Navigate mode — focused_node = None

#### 2.5.a Agents panel

| Key | Precondition | Action | file:line |
|---|---|---|---|
| `j` / Down | — | `Action::SelectNextBy(1)` | `:1125, :1326` |
| `k` / Up | — | `Action::SelectPrevBy(1)` | `:1140, :1318` |
| PgDown | — | `Action::SelectNextBy(5)` | `:1352` |
| PgUp | — | `Action::SelectPrevBy(5)` | `:1344` |
| `g` | — | select_first + scroll_to_top | `:1163` |
| `G` | — | select_last + scroll_to_bottom | `:1181` |
| `c` | — | `Action::ToggleCollapse` | `:1162` |
| Enter | — | `Action::FocusNode` | `:1394` |
| Tab | input empty | `cycle_example()` (cycles example prompts) | `:1366` |
| Tab | otherwise | toggle Agents↔Log; `Action::CycleFocus` | `:1370` |
| Shift+Tab | — | toggle Agents↔Log; `Action::CycleFocus` | `:1381` |
| `1` | — | focus Agents panel | `:1113` |
| `2` | — | `Action::NavigateTo(IssueBrowser)` | `:1052` |
| `3` | — | focus Log panel | `:1119` |
| `Ctrl+d` | — | `activity_log.scroll_down_by(5)` | `:1061` |
| `Ctrl+u` | — | `activity_log.scroll_up_by(5)` | `:1069` |
| Esc | — | `Action::NavigateBack` | `:1393` |

#### 2.5.b Log panel

| Key | Action | file:line |
|---|---|---|
| `j` / Down | scroll_down(20) → `Action::ScrollDown` | `:1131, :1334` |
| `k` / Up | scroll_up → `Action::ScrollUp` | `:1146, :1322` |
| `g` | scroll_to_top → `Action::ScrollToTop` | `:1172` |
| `G` | scroll_to_bottom → `Action::ScrollToBottom` | `:1190` |
| PgUp | scroll_up_by(10) | `:1347` |
| PgDown | scroll_down_by(10,20) | `:1358` |
| `Ctrl+d` | scroll_down_by(5) | `:1061` |
| `Ctrl+u` | scroll_up_by(5) | `:1069` |

#### 2.5.c Shared globals (any panel, no focused node)

| Key | Precondition | Action | file:line |
|---|---|---|---|
| `r` | not (focused_node + Review tab) | `Action::JumpToReview` | `:1155` |
| `N` | — | `Action::JumpToReview` | `:1160` |
| `P` | — | `Action::JumpToPreviousReview` | `:1161` |
| `v` | — | toggle verbose, `Action::ToggleVerbose` | `:1199` |
| `z` | Vim Normal only | toggle `layout_zoomed` | `:1203` |
| `?` | — | `Action::ShowHelp` | `:1207` |
| `s` | — | `Action::RequestSessions` | `:1208` |
| `q` | no Ctrl/Alt | `Action::Quit` | `:1078` |

### 2.6 Navigate mode — focused_node = Some

#### 2.6.a Detail tab navigation

| Key | Action | file:line |
|---|---|---|
| Left | `cycle_tab(false)` (prev) | `:979` |
| Right | `cycle_tab(true)` (next) | `:983` |
| `h` | `cycle_tab(false)` | `:988` |
| `l` | `cycle_tab(true)` | `:997` |
| `Ctrl+1` | jump to Stream | `:1007` |
| `Ctrl+2` | jump to Artifacts | `:1013` |
| `Ctrl+3` | jump to Attempts | `:1019` |
| `Ctrl+4` | jump to Task | `:1025` |
| `Ctrl+5` | jump to Review | `:1031` |

#### 2.6.b Detail scroll (any tab)

`j/k/g/G/Up/Down/PgUp/PgDown/Ctrl+d/Ctrl+u` mirror the Log panel keys but route to `detail_pane.scroll_*` instead of activity_log.

#### 2.6.c Stream tab

| Key | Action | file:line |
|---|---|---|
| `o` | `trace.toggle_observe_collapsed()` (returns None) | `:1038` |

This is the binding the F8 fix unblocked. `is_view_action_char('o')` returns true when `focused_node.is_some()`, routing to View before the vim-entry whitelist.

#### 2.6.d Review tab

| Key | Action | file:line |
|---|---|---|
| `A` | `SubmitReview { Approve }` | `:1093, review_card.rs:64` |
| `D` | `SubmitReview { Reject }` | `:1093, review_card.rs:65` |
| `M` | `SubmitReview { Modify }` | `:1093, review_card.rs:68` |
| `R` | `SubmitReview { Retry }` | `:1093, review_card.rs:71` |
| `r` | (guarded out — falls to None) | `:1155` |

#### 2.6.e Unfocus

| Key | Action |
|---|---|
| Esc | `Action::UnfocusNode` |

### 2.7 Vim-Normal swallowed entry chars

In the View handler's match (`:1111`), `i/a/A/I/O` are explicitly listed and return `None` (`:1209`). They reach View only when `is_view_action_char` claimed them (e.g. `A` on Review tab). `o` is consumed by §2.6.c.

---

## 3. SessionDetail view (`views/session_detail.rs`)

Three implicit owners per keystroke: **Composer** (input_bar), **Picker** (completion/history shell), **View** (scroll/nav). Input_bar may be in Plain/Vim Normal/Vim Insert sub-modes.

### 3.1 Global priority (always evaluated first)

| Key | Modifiers | Precondition | Action | file:line |
|---|---|---|---|---|
| Esc | — | Stream in flight, not cancelling, not `wants_esc` | `Action::CancelStream` | `:1167` |
| `M` | Alt | — | `Action::TogglePlanMode` | `:1185` |
| `S` | Alt | — | `Action::RequestSessions` | `:1194` |
| `W` | Alt | — | `Action::InspectWorkers` | `:1200` |
| `D` | Alt | — | toggle inline workers panel | `:1205` |
| `I` | Alt | — | `Action::ToggleVimMode` | `:1211` |
| `P` | Alt | plan exists | `Action::NavigateTo(PlanInspector)` | `:1538` |
| `P` | Alt | no plan | show "No tracked plan" banner | `:1548` |

Any keystroke also dismisses the auth-error banner.

### 3.2 Resume banner

If active (`:1526–1534`), all keys forward to banner; Esc may swallow or fall through depending on banner state.

### 3.3 Picker routing

Same pattern as dashboard (§2.2). Trigger-driven keys: `Up/Down/Esc/Tab/Enter/Ctrl+C/Ctrl+P/Ctrl+N`.

### 3.4 View owner — scroll & navigation

#### 3.4.a Permission prompt (when `react_trace.has_pending_permission()`)

| Key | Action |
|---|---|
| `y` | `PermissionGrant(Allow)` |
| `n` | `PermissionGrant(Deny)` |
| `a` | `PermissionGrant(AlwaysAllow)` |

#### 3.4.b History recall + picker

| Key | Action | file:line |
|---|---|---|
| `Ctrl+P` | history_prev | `:1412` |
| `Ctrl+N` | history_next | `:1421` |
| `Ctrl+R` / `Alt+R` | open fuzzy history picker | `:1459` |

#### 3.4.c Observe toggle

| Key | Action | file:line |
|---|---|---|
| `Ctrl+O` | toggle Observe entries collapse | `:1404` |

(Note: Dashboard's `Ctrl+O` is a dead no-op — see §11.2. SessionDetail's `Ctrl+O` is wired.)

#### 3.4.d Mermaid (markdown feature)

| Key | Precondition | Action | file:line |
|---|---|---|---|
| `Alt+V` | render picker present | `Action::NavigateTo(MermaidOverlay)` | `:1432` |

#### 3.4.e Scroll & navigation

| Key | Precondition | Action | file:line |
|---|---|---|---|
| PgUp / PgDown | always | `Action::ScrollUp/Down` | `:1471, :1475` |
| `j` / `k` / `g` / `G` | input bar empty | scroll | `:1485–1497` |
| Up / Down | input bar empty | scroll | `:1501, :1505` |
| Esc | input bar empty | `Action::NavigateBack` | `:1509` |

### 3.5 Composer

Same vim/emacs entry pattern as dashboard. `i/a/A/I/o/O` enter Insert when the bar is empty + Vim Normal. Otherwise text edit keys forward to input_bar.

---

## 4. SessionPicker view (`views/session_picker.rs`)

Five modes evaluated in priority order.

### 4.1 Confirm-switch banner (highest)

Triggered when current session has unsent draft and user navigates to a different session. Intercepts ALL keys.

| Key | Action |
|---|---|
| `y` / `Y` / Enter | Commit pending action |
| Anything else (`n` / `N` / Esc / etc.) | Cancel banner |

### 4.2 Rename mode

Entered via `R` in browse mode.

| Key | Action |
|---|---|
| Enter | `RenameSession` |
| Esc | Cancel |
| Backspace | Delete last char |
| Any printable | Append to buffer |

### 4.3 Search/filter mode (`search_focused = true`)

| Key | Action |
|---|---|
| Esc | Exit search (preserve filter text) |
| Enter | Commit filter (preserve filter text) |
| Backspace | Delete + reset cursor to row 0 |
| Any printable | Append + reset cursor to row 0 |

### 4.4 Browse/list mode

#### Navigation

| Key | Action |
|---|---|
| `/` | Enter search mode |
| Up / `k` | Cursor up |
| Down / `j` | Cursor down |
| `P` | Toggle preview pane |

#### Selection

| Key | Precondition | Action |
|---|---|---|
| Enter | cursor=0 (New row), no draft | `NewSessionRequested` |
| Enter | cursor=0, draft exists | confirm-switch banner |
| Enter | cursor≥1, current session | `NavigateTo(SessionDetail)` |
| Enter | cursor≥1, different session, no draft | `ResumeSession` |
| Enter | cursor≥1, different session, has draft | confirm-switch banner |

#### Session ops

| Key | Action |
|---|---|
| `n` | `NewSessionRequested` (or banner) |
| `R` | Enter rename mode |
| `p` | `ToggleSessionPin` |
| `d` | `ToggleSessionArchive` |
| `y` | `CopySessionId` |
| `a` | `ToggleShowArchived` |
| `r` | `RefreshSessions` |

#### Esc

| Key | Precondition | Action |
|---|---|---|
| Esc | filter non-empty | clear filter |
| Esc | filter empty | `NavigateTo(Dashboard)` |

### 4.5 Loading / Error

Esc → `NavigateTo(Dashboard)`. Footer hint shows `r retry` for error state but **no `r` handler exists** in that arm — see §11.3.

---

## 5. IssueBrowser (`views/issue_browser.rs`)

Two implicit modes.

### 5.1 List mode (no detail open)

| Key | Action | file:line |
|---|---|---|
| Esc | `NavigateTo(Dashboard)` | `:109` |
| `q` | Quit | `:117` |
| `?` | `ShowHelp` | `:118` |
| `s` | `RequestSessions` | `:119` |
| `j` / Down | next issue | `:122` |
| `k` / Up | prev issue | `:126` |
| `g` | first | `:130` |
| `G` | last | `:133` |
| Enter | `IssueAction::ViewDetail` | `:141` |
| `o` | status → open | `:162` |
| `w` | status → in_progress | `:163` |
| `b` | status → blocked | `:164` |
| `d` | status → closed | `:165` |
| `W` | `IssueAction::WorkOn` | `:166` |

### 5.2 Detail mode (`IssueFocus::Loaded`)

| Key | Action |
|---|---|
| Esc | Close detail (no nav) |
| Enter | Toggle detail off |
| PgUp | scroll up 10 |
| PgDown | scroll down 10 |
| `o`/`w`/`b`/`d`/`W` | same status actions, target focused issue |

---

## 6. PlanInspector (`views/plan_inspector.rs`)

Single mode. Layout-adaptive: wide (≥90 cols) vs stacked (<90 cols).

| Key | Action |
|---|---|
| `h` / Left | prev stage lane |
| `l` / Right | next stage lane |
| `j` / Down | next task (in lane wide / globally stacked) |
| `k` / Up | prev task |
| `g` | first task in current lane |
| `G` | last task in current lane |
| Esc | `NavigateBack` |
| `Alt+p` | `NavigateBack` |

No filter, sort, or search keys.

---

## 7. MermaidViewer (`views/mermaid_viewer.rs`, `cfg(feature = "markdown")`)

| Key | Action |
|---|---|
| `q` / Esc | `NavigateBack` |

Diagram cycling (`[`/`]`) is intercepted at app level (§1.3.a). No zoom/pan/scroll keys in this view.

---

## 8. InputBar component (`components/input_bar.rs`)

Editor with five modes: Plain (no vim), Vim Normal/Insert/Visual/Operator-pending. The whole component is also where command/mention/history pickers attach.

### 8.1 Vim Normal

Motions: `h l j k w b e 0 ^ $ gg G Up Down`.

Edits: `D` (delete to EOL, stay Normal), `C` (delete to EOL, → Insert), `x` (delete char), `p` (paste from register), `dd cc yy` (line operators), `d c y {motion}` (operator-pending).

Mode entry: `i a A I o O` (with cursor positioning), `v V` (Visual).

Scroll: `Ctrl+d/u/f/b/e/y` (half/full page, single row).

Submit: Enter (if non-empty). Insert newline: `Alt+Enter`.

**Unimplemented vim**: `r ; , f F t T n N : / ? u Ctrl+r P ~ >> << ci" da(` — and counted motions (e.g. `5j`).

### 8.2 Vim Insert

Esc → Normal. `Ctrl+j` / `Alt+j` / `Alt+Enter` insert newline. Up/Down use sticky-goal-column visual-line. All other keys delegate to `tui-textarea`.

### 8.3 Vim Visual

All motion keys extend selection. `y` copy → Normal. `d` cut → Normal. `c` cut → Insert. Esc cancels selection → Normal. `v` toggles off Visual. `Alt+Enter` inserts newline (stays Visual).

### 8.4 Vim Operator-pending (`Operator(d|c|y)`)

Doubled-key shortcut: `dd cc yy`. `gg` jump-then-complete. Any motion key completes the operator. Esc aborts.

### 8.5 Emacs

Native Emacs bindings (Ctrl+A/E/B/F/D/Y, Alt+B/F/D) come from tui-textarea's fallback `input()`. Explicitly handled:

| Key | Action |
|---|---|
| Up / Down | visual-line move (sticky goal-column) |
| Left / Right | atom-aware char move |
| Backspace / Delete | atom-aware delete |
| Home / End | line start/end |
| `Ctrl+j` | newline |
| `Ctrl+p` | history_prev |
| `Ctrl+n` | history_next |
| `Ctrl+u` | delete to line start |
| `Ctrl+k` | delete to line end |
| `Ctrl+w` | delete previous word |
| `Alt+Enter` | newline |
| Enter | submit (if non-empty) |
| Any printable | insert (replacing protected range if inside) |

### 8.6 PickerShell (`components/picker_shell.rs:103`)

Common to all pickers:

| Key | Action |
|---|---|
| Esc | cancel/close |
| Up | prev row (wraps) |
| Down | next row (wraps) |
| Tab / Enter | accept |

`OwnedByShell`-only (history picker): also Backspace/Delete/Left/Right/Home/End/printable → query field edits.

### 8.7 Trigger pickers

#### `@mention`

`QueryMode::ReadFromInputBar`. Trigger fires when `@` typed at offset 0 or after whitespace, NOT inside a protected range. Picker accepts via `RetrievalAccept::InsertAtom` (replaces `@…` with a protected atom). Picker closes on whitespace, paste, cursor exit, submit, dismiss.

#### `/command`

`QueryMode::ReadFromInputBar`. Trigger fires only when `/` typed at offset 0. Picker accepts via `RetrievalAccept::ReplaceTriggerToken`.

#### `/cmd <arg>` sub-picker

After `/<registered-cmd> ` (with trailing space), launches a sub-picker (`ConfigOptionQuerySource` / `CommandInputQuerySource`). Same key contract.

### 8.8 History picker (`Ctrl+R` / `Alt+R` in session_detail)

`QueryMode::OwnedByShell` — separate query field. Full keyboard editing in the shell's MiniInput. Accepts via `RetrievalAccept::ReplaceState` (whole InputBar state replaced).

### 8.9 Multi-line / soft-wrap

| Key | Mode | Behavior |
|---|---|---|
| Up / Down | Emacs / Vim Normal / Vim Insert | visual-line move, sticky goal-column |
| `j` / `k` | Vim Normal | logical-line move (NOT visual) |

`goal_vcol` set on first vertical move; preserved across consecutive vertical moves; reset by any horizontal move or edit.

### 8.10 History recall

| Key | Source | Action |
|---|---|---|
| `Ctrl+p` | dashboard.rs:1408 (cross-mode), Emacs path input_bar.rs:338 | history_prev |
| `Ctrl+n` | dashboard.rs:1412 (cross-mode), Emacs path input_bar.rs:342 | history_next |
| `Ctrl+R` / `Alt+R` | session_detail.rs:1459 only | open fuzzy history picker |

History capped at `HISTORY_CAP` (FIFO). `history_cursor = None` = editing live draft; `Some(i)` = browsing entry `i`.

### 8.11 Paste

- Single line (≤1 newline): inline insert; protected ranges shifted.
- Multi-line (≥2 lines): atomized as `ProtectedRange { kind: PasteRef(id) }`, displayed as `[Paste #N · M lines]`. Capped at `PASTE_STORE_CAP = 50`.
- Vim `p` uses tui-textarea's internal register.
- Trigger detector closes any active picker on paste.

### 8.12 Mode introspection

| Method | Returns true when | Used by |
|---|---|---|
| `wants_esc()` | Vim Insert / Visual / Operator | dashboard, session_detail Esc routing |
| `is_vim_normal()` | Vim Normal | dashboard `key_owner`, session_detail empty-bar nav |
| `mode()` | current `EditMode` | various |
| `is_active()` | InputBar has visual focus | dashboard render gating |

---

## 9. Cross-cutting matrices

### 9.1 Esc behavior by context

| Context | Esc → |
|---|---|
| App overlay open | dismiss overlay |
| User-warning banner + view returns Back/Dashboard | dismiss banner |
| Compose mode + `wants_esc()` | composer (vim Insert/Visual/Operator → Normal) |
| Compose mode + not `wants_esc()` | Navigate mode |
| Dashboard Navigate, focused node | `UnfocusNode` |
| Dashboard Navigate, no focused node | `NavigateBack` |
| SessionDetail, stream in flight, not `wants_esc()` | `CancelStream` |
| SessionDetail, input bar empty | `NavigateBack` |
| SessionPicker confirm-switch | dismiss banner |
| SessionPicker rename | cancel rename |
| SessionPicker search | exit search (preserve filter) |
| SessionPicker browse, filter non-empty | clear filter |
| SessionPicker browse, filter empty | `NavigateTo(Dashboard)` |
| IssueBrowser detail open | close detail |
| IssueBrowser list | `NavigateTo(Dashboard)` |
| PlanInspector | `NavigateBack` |
| MermaidViewer | `NavigateBack` |

### 9.2 Scroll keys cheat-sheet

Across views: `j/k` line, `g/G` top/bottom, PgUp/PgDown page, `Ctrl+d/u` half-page (dashboard), `Ctrl+f/b` full-page (input_bar vim only). `Up/Down` mirror `j/k` everywhere except input_bar where they do visual-line move.

### 9.3 History recall keys

| Key | Where | Notes |
|---|---|---|
| `Ctrl+P/N` | dashboard, session_detail, input_bar emacs | linear history |
| `Ctrl+R` / `Alt+R` | session_detail only | fuzzy picker |

Dashboard does NOT have `Ctrl+R` — fuzzy history is a session_detail-only feature.

### 9.4 Modifier conventions

- `Ctrl+` = control-flow / scroll / history.
- `Alt+` = mode toggles + navigation jumps (`Alt+I` vim toggle, `Alt+M` plan mode, `Alt+S` sessions, `Alt+W` workers, `Alt+D` workers panel, `Alt+P` plan inspector, `Alt+R` history picker, `Alt+V` mermaid).
- `Shift+` = capitalized variant of base char (treated as different key by handler).

---

## 10. ViewId × mode summary

| View | Modes | Notes |
|---|---|---|
| Dashboard | Navigate × {Vim Normal, Emacs} × {Agents, Detail, Log} × {focused, not} × DetailTab; Compose | Most complex. F8 patched 2026-04-28. |
| SessionDetail | Implicit per-key owner (Composer/Picker/View) | Has its own `Alt+*` global hotkeys |
| SessionPicker | Browse / Search / Rename / ConfirmSwitch / Loading / Error | Confirm-switch banner preempts all |
| IssueBrowser | List / Detail | Status keys `o w b d W` work in both |
| PlanInspector | Single | No filter/sort |
| MermaidViewer | Single | Cycle keys at app level |

---

## 11. Known issues / smells uncovered during this audit

### 11.1 ✅ FIXED 2026-04-28 — vim-`o` on focused node didn't toggle observe

Commit `bd5c4b5c` (F8). `dashboard.rs:923` now puts `is_view_action_char(c)` first in the vim arm. Regression test: `vim_normal_focused_node_o_toggles_observe_not_compose`.

### 11.2 Dead `Ctrl+O` global bypass on dashboard

`dashboard.rs:901` adds `Ctrl+O` to the global-bypass list, forcing `KeyOwner::View`. But `handle_view_key`'s `o` arm at `:1038` explicitly excludes `KeyModifiers::CONTROL | KeyModifiers::ALT`. Result: `Ctrl+O` on dashboard is a silent no-op. Either wire it (e.g. observe-toggle alias) or remove from the bypass list. Note: SessionDetail has a working `Ctrl+O` at `:1404` — naming consistency would suggest the dashboard alias.

### 11.3 SessionPicker error-state `r retry` hint is aspirational

Footer at `session_picker.rs:29` shows `r retry · Esc back` for `PickerState::Error`, but the `Loading | Error` arm (`:1483`) doesn't have a `KeyCode::Char('r')` branch. `r` only fires `RefreshSessions` in `Populated` state. Either add the handler or fix the hint.

### 11.4 Hint row overdraws bottom border

(Not in this audit; previously identified.) `dashboard.rs:render_input_hint` paints at `input_bar_area.y - 1`, which is inside `chunks[1]`'s area — overwriting the bordered widget's bottom row. Add a dedicated 1-row layout constraint.

### 11.5 Vim/Emacs split in `key_owner` is the remaining structural smell

After F8, `key_owner` still has separate vim/emacs arms. They could be unified into a 2-arm match (codex's F7 shape). Punted as a separate refactor.

### 11.6 Unimplemented vim commands in InputBar

`r ; , f F t T n N : / ? u Ctrl+r P ~ >> << ci" da(` and **counted motions** (e.g. `5j`). These silently fall through to `tui-textarea.input()` which may handle some unexpectedly. Not a bug per se but a UX surprise vector.

### 11.7 Mode-mixing risk: input_bar in Vim Visual + dashboard in Navigate

If reachable, `is_vim_normal()` returns false, the vim arm guard fails, and `o` would route through the emacs arm logic. Esc handling at dashboard.rs:914-920 should drain Visual/Operator before entering Navigate, so this is unreachable in practice — but no assertion enforces it. A debug_assert in `set_mode(Navigate)` would close the gap.

---

## 12. Method note

This document was assembled by 6 parallel Explore agents on 2026-04-28, each scoped to one of: app-routing, dashboard, session_detail, session_picker, secondary views (issue_browser/plan_inspector/mermaid_viewer), input_bar component. Line numbers cited reference `main` at commit `bd5c4b5c` (post-F8 patch). Agent reports synthesized into this single mapping by the brain agent.

For per-file deep-dive, re-run the targeted agent with the same scope.
