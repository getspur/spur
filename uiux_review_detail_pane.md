# MCTS + Visual Thinking UI/UX Review
## Target: `crates/spur-tui/src/components/detail_pane.rs` + downstream consumers
## Date: 2026-04-22
## Methodology: Multi-round Monte Carlo Tree Search over UI/UX decision space

---

## 0. Methodology Overview

### MCTS Applied to UI/UX
| Phase | UI/UX Mapping |
|-------|--------------|
| **Selection** | Prioritize high-frequency user paths (scrolling, tab switching, reading stream/artifacts) |
| **Expansion** | Decompose each path into layout, interaction, visual, and error sub-states |
| **Simulation** | Mentally render the TUI at 80×24 and 120×40, simulating 4 personas |
| **Backpropagation** | Score findings by (severity × user frequency × fix cost) |

### First Principles Anchors
1. **Terminal UI is a grid of fixed cells** — no anti-aliasing, no animation, no z-depth beyond overlay
2. **All visible content must be reachable** — if it renders, it must be navigable
3. **Input modality parity** — mouse and keyboard must have equivalent access to primary actions
4. **Cognitive load scales with decision uncertainty** — invisible modes are expensive
5. **Feedback must be proximal** — scroll state must be visible where the eye is looking

### Personas Simulated
- **Alex** (First-Time): Discovers SPUR, reads hints, makes mistakes, tries mouse first
- **Blake** (Power Operator): 10+ agents, high tempo, muscle memory expected, uses both kb and mouse
- **Casey** (Reviewer): Monitors delegations, reads long diffs and task specs, scrolls heavily
- **Dana** (Debugger): Traces failures, needs to cross-reference stream + artifacts + review tabs

---

## Round 1: Interaction Architecture — The Scroll Gap

### 1.1 Visual Spatial Model (Current State)

```
┌─────────────────────────────────────────────────────────────┐
│  codex  ·  GH-42  open                                      │  ╮
│  stream │ artifacts │ attempts │ task │ review               │  │ tabs (1)
├─────────────────────────────────────────────────────────────┤  │
│  [2025-04-22 14:32:01] Starting task...                     │  │
│  [2025-04-22 14:32:02] Analyzing codebase...                │  │ body
│  [2025-04-22 14:32:03] Found 3 references to auth module    │  │ (scrollable)
│  ... (50+ more lines of streaming output)                   │  │
│  ...                                                        │  │
│  ...                                                        │  │
│                                                             │  ╯
│  ▲ 42 ↑                                        [I]ssue detail│  ← scroll label + actions
└─────────────────────────────────────────────────────────────┘
```

### 1.2 The Critical Failure: Mouse Scroll is Orphaned

**Simulation (Alex persona):**
Alex opens SPUR, sees a codex executor with a long stream. They try to scroll with their trackpad. Nothing happens. They try clicking in the pane. Still nothing. They eventually discover `j`/`k` or `↑`/`↓`, but the mental model is broken: *"This app doesn't support mouse scrolling."*

**Root Cause Analysis:**
`App::handle_mouse_event` in `app.rs` routes **all** Dashboard mouse scroll events to `activity_log`:
```rust
ViewId::Dashboard => {
    if is_up {
        self.dashboard.scroll_activity_up_by(lines);
    } else {
        self.dashboard.scroll_activity_down_by(lines);
    }
}
```
There is **zero** branching for:
- `focused_node.is_some()` → DetailPane is visible
- `issue_focus.is_loaded()` → IssueDetailPane is visible

**Severity: P0 — Broken Core Interaction**
The DetailPane occupies the majority of the screen real estate when an agent is selected. Mouse scroll is the primary navigation modality for most users in terminal emulators (especially iTerm2, Warp, VS Code integrated terminal). Leaving it unwired is a functional gap, not a polish issue.

### 1.3 Secondary Gap: No `scroll_*_by` APIs

Even if mouse events were routed, `DetailPane` only exposes:
- `scroll_up()` — 1 line
- `scroll_down()` — 1 line
- `scroll_to_top()`
- `scroll_to_bottom()`

There is **no** `scroll_up_by(lines)` or `scroll_down_by(lines)`. Mouse wheels emit batches of 3 lines (standard crossterm behavior). Without batch APIs, we cannot map mouse scroll ergonomically.

`IssueDetailPane` has the same deficiency.

---

## Round 2: Visual Feedback — The Scroll Label

### 2.1 Current Scroll Label States

| State | Label | Interpretation Cost |
|-------|-------|-------------------|
| Empty content | ` ` | Low |
| Content fits | ` ▼ ` | Medium (why a down arrow when nothing to scroll?) |
| Following (Stream) | ` ▼ following ` | Low |
| Following (other) | ` ▼ ` | Medium (no "following" text, same as fits) |
| At top | ` top ` | Low |
| At end | ` end ` | Low |
| Mid-scroll | ` ▲ 42 ↑ ` | **High** — what is 42? Lines? Percent? From top or bottom? |

### 2.2 Simulation (Casey persona)

Casey is reviewing a large diff in the Artifacts tab. They see `▲ 42 ↑`. Questions:
- Is 42 the line number from top? From bottom?
- What is the total line count?
- How far through the document am I?

The label requires decoding. In a TUI where color is the only emphasis channel, the scroll indicator should be **immediately legible**.

### 2.3 Recommended Scroll Label Redesign

```
Current:  " ▲ 42 ↑ "
Proposed: " 42/156 ▲ "   or   " 27% ▲ "
```

But: **this review focuses on the P0 scroll interaction fix.** Label redesign is P2 and out of scope for this pass. The current label is functional once scrolling works.

---

## Round 3: Tab Switching & Content Reset

### 3.1 Current Behavior

Every tab switch (`cycle_tab`, `jump_to_tab`) calls `set_tab`, which:
- Resets `scroll_offset = 0`
- Sets `is_following` per tab kind (`true` for Stream, `false` for others)

### 3.2 Simulation (Dana persona)

Dana is debugging a failed attempt. They scroll down in the Stream tab to find the error. They switch to Artifacts to see the diff. They switch back to Stream. Their scroll position is **lost** — reset to bottom (following).

This is **intentional** per the code comments, but it creates friction for cross-referencing. The reset invariant is defensive (avoids stale offsets when tab content lengths differ), but it could be smarter: remember per-tab scroll state.

**Severity: P2 — Feature Request, not bug.**

---

## Round 4: Keyboard-Only Page Scrolling

### 4.1 Current Keyboard Map

| Key | Action |
|-----|--------|
| `j` / `↓` | scroll_down (1 line) |
| `k` / `↑` | scroll_up (1 line) |
| `g` | scroll_to_top |
| `G` | scroll_to_bottom |

Missing: `PgUp` / `PgDn` — not handled anywhere in `dashboard.rs` or `app.rs` for DetailPane.

For long diffs and streams, page-scrolling is essential. Power users expect `PgUp`/`PgDn` or `Ctrl+u`/`Ctrl+d` (vim) or `Space`/`Shift+Space`.

**Severity: P1 — Accessibility & Power User Gap.**

---

## Round 5: Event Routing Architecture

### 5.1 The Routing Decision Tree

```
Mouse Scroll Event (Dashboard view)
│
├─ IssueFocus::Loaded ──→ IssueDetailPane.scroll_*_by(3)
│
├─ focused_node.is_some() ──→ DetailPane.scroll_*_by(3)
│   │
│   └─ DetailPane.current_tab decides which content scrolls
│       (Stream/Artifacts/Attempts/Task/Review)
│
└─ Otherwise ──→ ActivityLog.scroll_*_by(3)
```

This is the **minimal viable fix** for the P0 issue. It requires:
1. `DetailPane::scroll_up_by(usize)`
2. `DetailPane::scroll_down_by(usize)`
3. `IssueDetailPane::scroll_up_by(usize)`
4. `IssueDetailPane::scroll_down_by(usize)`
5. `DashboardView` forwarding methods
6. `App::handle_mouse_event` branching logic

---

## Summary of Findings

| ID | Finding | Severity | Fix Cost | Decision |
|----|---------|----------|----------|----------|
| F1 | Mouse scroll in Dashboard always routes to ActivityLog, never DetailPane | **P0** | Low | **Fix now** |
| F2 | DetailPane lacks `scroll_*_by(usize)` APIs needed for mouse wheels | **P0** | Low | **Fix now** |
| F3 | IssueDetailPane lacks `scroll_*_by(usize)` APIs | **P0** | Low | **Fix now** |
| F4 | No `PgUp`/`PgDn` handling for DetailPane | P1 | Low | Fix next pass |
| F5 | Scroll label `▲ 42 ↑` is cryptic | P2 | Medium | Fix next pass |
| F6 | Tab switch resets scroll offset (no per-tab memory) | P2 | Medium | Design discussion |
| F7 | `scroll_down()` does not auto-engage `is_following` on non-Stream tabs | P2 | Low | Consider for consistency |

---

## Implementation Plan (F1–F3)

### Step 1: DetailPane — add batch scroll APIs
```rust
pub fn scroll_up_by(&mut self, lines: usize) {
    self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    self.is_following = false;
}

pub fn scroll_down_by(&mut self, lines: usize) {
    self.scroll_offset = self.scroll_offset.saturating_add(lines);
}
```

### Step 2: IssueDetailPane — add batch scroll APIs
```rust
pub fn scroll_up_by(&mut self, lines: u16) {
    self.scroll_offset = self.scroll_offset.saturating_sub(lines);
}

pub fn scroll_down_by(&mut self, lines: u16) {
    self.scroll_offset = self.scroll_offset.saturating_add(lines).min(500);
}
```

### Step 3: DashboardView — add forwarding methods
```rust
pub fn scroll_detail_up_by(&mut self, lines: usize) { ... }
pub fn scroll_detail_down_by(&mut self, lines: usize) { ... }
pub fn scroll_issue_detail_up_by(&mut self, lines: u16) { ... }
pub fn scroll_issue_detail_down_by(&mut self, lines: u16) { ... }
```

### Step 4: App — route mouse events by visible content
```rust
ViewId::Dashboard => {
    if self.dashboard.issue_focus().is_loaded() {
        // scroll issue detail pane
    } else if self.dashboard.focused_node().is_some() {
        // scroll detail pane
    } else {
        // scroll activity log
    }
}
```

### Invariants to Preserve
- `scroll_offset` clamping happens in `render()`, not in scroll methods. Do not duplicate clamp logic.
- `is_following = false` on all upward scrolls (breaks follow mode so user can read).
- Stream tab auto-follow behavior remains unchanged.

---

*Review conducted by L9 UIUX Designer via MCTS + Visual Thinking methodology.*
