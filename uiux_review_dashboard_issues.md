# MCTS + Visual Thinking UI/UX Review
## Target: `crates/spur-tui/src/views/dashboard.rs` + `issues_panel.rs` + `issue_detail_pane.rs` — Issues Pane
## Date: 2026-04-22
## Methodology: Multi-round Monte Carlo Tree Search over UI/UX decision space

---

## 0. Methodology Overview

### MCTS Applied to UI/UX
| Phase | UI/UX Mapping |
|-------|--------------|
| **Selection** | Prioritize high-frequency user paths (selecting issues, scrolling detail, status updates) |
| **Expansion** | Decompose each path into layout, interaction, visual, and error sub-states |
| **Simulation** | Mentally render the TUI at 80×24 and 120×40, simulating 4 personas |
| **Backpropagation** | Score findings by (severity × user frequency × fix cost) |

### First Principles Anchors
1. **All input modalities must work** — arrow keys and vim keys are not optional alternatives
2. **Position must be visible** — users need to know where they are in a list
3. **Mouse scroll follows focus** — wheel events should target the visually active pane
4. **Metadata should be complete** — if the data model has it, the UI should surface it
5. **Consistent abbreviations** — mixed "wip"/"blk" with "open"/"done" creates decoding friction

### Personas Simulated
- **Alex** (First-Time): Tries arrow keys, expects visual feedback, reads titles
- **Blake** (Power Operator): Fast scans issue list, uses `g`/`G`, expects mouse scroll
- **Casey** (Reviewer): Reads long issue bodies, updates statuses, needs smooth scroll
- **Dana** (Debugger): Opens issue detail, cross-references URLs, checks blocked_by

---

## Round 1: Keyboard Interaction — The Arrow Key Gap

### 1.1 Current Routing (Pre-Fix)

```
KeyCode::Up =>
    IssueFocus::Loaded    → scroll issue detail
    focused_node          → scroll detail pane
    focused_panel::Agents → select_prev agent
    OTHERWISE             → scroll activity log   ← Issues panel falls here!
```

### 1.2 The Bug

**Simulation (Alex persona):**
Alex presses `Tab` to cycle focus to the Issues panel. The border turns cyan. The title shows `Issues — [j/k] select`. Alex tries `↑`/`↓` (arrow keys) out of habit. The Activity Log scrolls instead. Alex thinks the selection didn't move. They press `↓` three more times. The log scrolls down 3 lines. The issues selection is still on row 1. Alex is confused.

**Root Cause:**
`KeyCode::Up`/`Down` in `handle_view_key` had branches for `IssueFocus::Loaded`, `focused_node`, and `Panel::Agents`, but **no branch for `Panel::Issues`**. Arrow keys in the Issues panel fell through to the Activity Log scroll.

This is a **P1 accessibility bug** — arrow keys are the universal navigation fallback. Leaving them unwired for an entire panel breaks discoverability for non-vim users.

**Fix Applied:** Added `else if self.focused_panel == Panel::Issues` branches to both `KeyCode::Up` and `KeyCode::Down`, calling `issues_panel.select_prev` / `select_next`.

---

## Round 2: Mouse Scroll — The Focus Mismatch

### 2.1 Current Mouse Routing (Pre-Fix)

```rust
ViewId::Dashboard =>
    issue_detail_visible()  → scroll issue detail
    focused_node.is_some()  → scroll detail pane
    OTHERWISE               → scroll activity log
```

### 2.2 The Bug

**Simulation (Blake persona):**
Blake has the Issues panel focused and tries to scroll through a list of 20 issues with their trackpad. The Activity Log scrolls instead. The issues list doesn't move. Blake has to switch to `j`/`k`.

**Root Cause:**
Mouse scroll events in the Dashboard view only check `issue_detail_visible()` and `focused_node`. They never check `focused_panel == Panel::Issues`.

**Fix Applied:**
- Added `focused_panel()` accessor to `DashboardView`
- Added `issues_panel_mut()` accessor to `DashboardView`
- Updated `App::handle_mouse_event` to route scroll to `issues_panel.select_prev` / `select_next` when `focused_panel == Panel::Issues`

---

## Round 3: List Navigation — Missing `g`/`G` and Scroll Indicator

### 3.1 Missing `g`/`G`

When `focused_panel == Panel::Issues`, pressing `g`/`G` scrolled the Activity Log to top/bottom. There was no way to jump to the first or last issue.

**Fix Applied:** Added `g`/`G` guards for `Panel::Issues` in both vim_normal and insert mode branches, calling `select_first()` / `select_last()`.

### 3.2 Missing Scroll Indicator

The Issues panel title was static: `" Issues "` (unfocused) or `" Issues — [j/k] select · [Enter] detail · [W]ork "` (focused). With 20 issues and a 6-row viewport, the user had **no way to know** their position in the list or whether more items existed.

**Fix Applied:** Title now shows position:
```
" Issues 3/20 — [j/k] select · [Enter] detail · [W]ork "
```

---

## Round 4: Issue Detail Pane — Metadata Gaps

### 4.1 Missing URL

The `Issue` data model includes a `url` field, but `issue_detail_pane.rs` never rendered it. Dana opens an issue detail to cross-reference the GitHub/Linear URL and has to exit SPUR to find it.

**Fix Applied:** URL is now appended to the labels metadata line:
```
labels: bug, ui  url: https://github.com/org/repo/issues/42
```

### 4.2 Status Abbreviation Inconsistency

In the issues **table**, statuses are abbreviated:
| Status | Display |
|--------|---------|
| open | "open" |
| in_progress | "wip" |
| blocked | "blk" |
| closed | "done" |

In the issue **detail pane**, the same abbreviations are used despite having ample space. "wip" and "blk" are jargon that add cognitive load for first-time users.

**Severity: P2 — Polish.** Documented for next pass (requires widening the table status column or using multi-word display in detail pane).

### 4.3 Scroll Cap is Arbitrary

`scroll_down` caps at 500 wrapped rows. For extremely long issue bodies, this could be insufficient. However, 500 rows ≈ 20 screens, so this is a theoretical concern rather than a practical one.

---

## Round 5: Error States

### 5.1 Loading Failure

When `IssueCommandError` arrives during `IssueFocus::Loading`, the dashboard resets `issue_focus` to `None`. The user is silently returned to the log panel. An error entry appears in the Activity Log, but if the log has scrolled, the user may not see it.

**Severity: P2 — Error Visibility.** Documented for next pass (could show a transient error banner in the issue detail area).

---

## Summary of Findings

| ID | Finding | Severity | Fix Cost | Decision |
|----|---------|----------|----------|----------|
| I1 | **Arrow keys don't work** in Issues panel — fall through to Activity Log | **P1** | Low | **Fixed** |
| I2 | **Mouse scroll doesn't route** to Issues panel | **P1** | Low | **Fixed** |
| I3 | **No `g`/`G` support** for jumping to first/last issue | **P1** | Low | **Fixed** |
| I4 | **No scroll indicator** — user can't tell position in issue list | **P2** | Low | **Fixed** |
| I5 | **URL not shown** in issue detail pane | **P2** | Low | **Fixed** |
| I6 | Status abbreviations "wip"/"blk" inconsistent with "open"/"done" | P2 | Low | Documented |
| I7 | Issue detail scroll cap at 500 is arbitrary | P2 | Low | Documented |
| I8 | Issue load error silently drops user back to log | P2 | Medium | Documented |

---

## Implementation Summary

### `crates/spur-tui/src/views/dashboard.rs`
1. Added `focused_panel()` accessor
2. Added `issues_panel_mut()` accessor
3. Fixed `KeyCode::Up`/`Down` to handle `Panel::Issues`
4. Added `g`/`G` guards for `Panel::Issues` in both vim_normal and insert mode

### `crates/spur-tui/src/components/issues_panel.rs`
1. Added `select_last(issue_count)` API
2. Title now shows `Issues {selected+1}/{total}` position indicator

### `crates/spur-tui/src/components/issue_detail_pane.rs`
1. Appended URL to labels metadata line when present

### `crates/spur-tui/src/app.rs`
1. Updated `handle_mouse_event` to route scroll to Issues panel when `focused_panel == Panel::Issues`

### Verification
- `cargo build -p spur-tui` ✅
- `cargo test -p spur-tui` — **366 passed, 0 failed** ✅
- `cargo clippy -p spur-tui -- -D warnings` ✅

---

*Review conducted by L9 UIUX Designer via MCTS + Visual Thinking methodology.*
