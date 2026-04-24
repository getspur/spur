# MCTS + Visual Thinking UI/UX Review
## Target: `crates/spur-tui/src/views/dashboard.rs` + `agents_tree.rs` — Lineage Panel
## Date: 2026-04-22
## Methodology: Multi-round Monte Carlo Tree Search over UI/UX decision space

---

## 0. Methodology Overview

### MCTS Applied to UI/UX
| Phase | UI/UX Mapping |
|-------|--------------|
| **Selection** | Prioritize high-frequency user paths (navigating agents, collapsing, focusing, monitoring status) |
| **Expansion** | Decompose each path into layout, interaction, visual, and error sub-states |
| **Simulation** | Mentally render the TUI at 80×24 and 120×40, simulating 4 personas |
| **Backpropagation** | Score findings by (severity × user frequency × fix cost) |

### First Principles Anchors
1. **All visible content must be reachable** — if it renders, it must be navigable
2. **Selection must be visible** — an off-screen selected item is a broken interaction
3. **Input modality parity** — arrow keys and vim keys must have equivalent access
4. **Affordances must be visible** — hidden state (collapsed/expanded) needs glyphs
5. **Tree structure must be readable** — connectors communicate hierarchy without decoding

### Personas Simulated
- **Alex** (First-Time): Discovers SPUR, reads hints, tries arrow keys, unsure what `c` does
- **Blake** (Power Operator): 10+ agents, deep trees, lives in vim keys, expects `g`/`G`
- **Casey** (Reviewer): Monitors delegations, collapses completed branches, scans for ⚠review
- **Dana** (Debugger): Traces failures through nested sub-executors, needs deep tree navigation

---

## Round 1: Layout Architecture & The Clipping Bug

### 1.1 Visual Spatial Model (Current State, Pre-Fix)

```
┌──────────────────────────────────────────┐
│ Lineage                                  │  ╮ agents_height
│ ◆ brain    BRAIN  Running   12m 34s      │  │ (4–12 rows)
│   └─ codex  EXEC  Succeeded  8m 12s      │  │
│   └─ gemini EXEC  Running   3m 45s       │  │
│     └─ sub1  SUB  Failed    1m 02s       │  │
│     └─ sub2  SUB  Running   0m 30s       │  │
│     └─ sub3  SUB  Spawning  0m 05s       │  │
│   └─ kiro   EXEC  AwaitingR …            │  │
│   └─ claude EXEC  Succeeded              │  │  ← CLIPPED — no scroll!
│   └─ codex2 EXEC  Running                │  │  ← CLIPPED — no scroll!
├──────────────────────────────────────────┤  ╯
```

### 1.2 The Critical Failure: Agents Tree is Unscrollable

**Simulation (Dana persona):**
Dana is debugging a brain that spawned 8 workers, each with 2–3 sub-executors. The agents tree shows 12 rows max. The remaining 14 nodes are **completely inaccessible**. Dana tries `j`/`k` — selection moves, but off-screen items are invisible. They try arrow keys — same. They try mouse wheel — routes to activity log. Dana is stuck.

**Root Cause:**
`AgentsTree::render` builds all lines into a `Paragraph` without a `scroll` parameter. Ratatui's `Paragraph` clips overflow. There is **no** `scroll_offset` field, no scroll methods, and no viewport awareness.

```rust
let paragraph = Paragraph::new(lines).block(block);
frame.render_widget(paragraph, area);  // overflow clipped silently
```

**Severity: P0 — Broken Core Interaction**
The lineage tree is the primary navigational surface of the dashboard. Capping it at 12 rows with no scroll makes large deployments unusable.

### 1.3 Secondary Failure: Selection Can Wander Off-Screen

Even within the 12 visible rows, `select_next`/`select_prev` move `selected` but do not ensure it is visible. If the tree has 20 nodes and the user selects node 15, nodes 13–15 are off-screen. The selection highlight is invisible. The user has no idea which node will be focused on Enter.

**Fix Applied:**
- Added `scroll_offset: usize` to `AgentsTree`
- Added `scroll_up`, `scroll_down`, `scroll_to_top`, `scroll_to_bottom` APIs
- `render()` now applies `Paragraph::scroll((scroll_offset, 0))`
- `render()` auto-scrolls to keep `selected` within the viewport
- Dashboard `j`/`k` and `↑`/`↓` now navigate the tree when `focused_panel == Agents`
- Dashboard `g`/`G` jump to first/last tree item and scroll to it

---

## Round 2: Visual Feedback — The Invisible Collapse State

### 2.1 Current Collapse Interaction

The `c` key toggles collapse on the selected node. But there is **zero visual indication** of which nodes are collapsed vs expanded. The only feedback is that children disappear/appear.

**Simulation (Casey persona):**
Casey presses `c` on a completed `codex` branch. Its 3 sub-executors vanish. Casey comes back 5 minutes later and can't remember whether `codex` has hidden children or was simply a leaf node. They press `c` again — children reappear. "Oh, it was collapsed."

### 2.2 Missing Affordance

A tree without collapse glyphs violates the principle of visible affordances. Every file manager, IDE outline, and tree widget uses `▶`/`▼` or `+`/`-`.

**Fix Applied:**
Added collapse glyphs to `build_line`:
```
├─ ▼ codex  ● EXEC  Succeeded   (expanded — children visible)
└─ ▶ gemini ● EXEC  Running     (collapsed — children hidden)
```

---

## Round 3: Tree Connector Graphics

### 3.1 Current Connectors (Pre-Fix)

```
brain   BRAIN  Running
  └─ codex  EXEC  Succeeded
  └─ gemini EXEC  Running
  └─ kiro   EXEC  Succeeded
```

Every child uses `└─ ` regardless of whether it has siblings below. This makes it **impossible to visually trace** which nodes are siblings and which are the last child. The tree looks like a flat list with indentation.

### 3.2 Proper Tree Drawing

A readable tree uses:
- `├─ ` for nodes with siblings below
- `└─ ` for the last child
- `│  ` for vertical continuation lines from ancestors

**Fix Applied:**
`render_subtree` now tracks `is_last` per node and passes `ancestor_states` (a boolean stack of "was last child") down the recursion. The indent builder produces:
```
brain   BRAIN  Running
  ├─ ▼ codex  EXEC  Succeeded
  │  └─ ▼ sub1  SUB  Failed
  │     └─ ▶ sub2  SUB  Spawning
  └─ ▶ gemini EXEC  Running
```

---

## Round 4: Keyboard Routing Bugs

### 4.1 Arrow Keys Don't Work in Agents Panel

**Simulation (Alex persona):**
Alex Tabs to the Agents panel. They try `↑`/`↓` to move selection. Instead, the Activity Log scrolls. Alex concludes the app is broken and reaches for the mouse.

**Root Cause:**
`KeyCode::Up`/`Down` in `handle_view_key` had **no** branch for `focused_panel == Panel::Agents`. They unconditionally routed to `activity_log.scroll_up()` / `detail_pane.scroll_down()`.

### 4.2 Detail Pane Scroll Override Bug

When `focused_node` is Some (detail pane is open), the context hint says:
```
[Detail: codex] ←/→ tabs · j/k scroll · ...
```

But if `focused_panel == Panel::Agents`, pressing `j`/`k` **moved the agents tree selection** instead of scrolling the detail pane. The context hint lied.

**Root Cause:**
The match arm `'j' if self.focused_panel == Panel::Agents` had higher precedence than the bare `'j'` arm that scrolls the detail pane.

**Fix Applied:**
- Added `&& self.focused_node.is_none()` to all `focused_panel == Panel::Agents` and `Panel::Issues` guards for `j`/`k`
- Added `Panel::Agents` branches to `KeyCode::Up`/`Down`
- Added `g`/`G` branches for Agents panel (select first/last + scroll)
- Detail pane scroll now takes precedence over panel navigation when a node is focused

---

## Round 5: Status Bar Hint Gap

### 5.1 Current Dashboard Status Bar

```
[i]nput [Enter]focus [r]eview [s]essions [Esc]back [Ctrl+C]quit [?]help
```

This hint is **static** — it never changes based on which panel is focused. When Alex is in the Agents panel, there's no mention of `c` (collapse), `j`/`k` (move), or `g`/`G` (jump). The only guidance is the one-line context hint above the input bar, which is easy to miss.

### 5.2 Simulation (Blake persona)

Blake knows `c` toggles collapse because they read the help overlay once. But a new operator wouldn't discover it from the status bar. The status bar has 45 chars reserved for hints; there's room for at least `[c]ollapse` when the Agents panel is active.

**Severity: P2 — Discoverability Gap**
Not fixed in this pass because it requires threading `focused_panel` into `StatusBarProps` and adding conditional hint logic. Documented for next pass.

---

## Summary of Findings

| ID | Finding | Severity | Fix Cost | Decision |
|----|---------|----------|----------|----------|
| L1 | Agents tree has no scroll — overflow nodes are permanently clipped | **P0** | Medium | **Fixed** |
| L2 | Selected item can be off-screen with no auto-scroll | **P0** | Low | **Fixed** |
| L3 | No collapse indicator (`▶`/`▼`) — hidden state is invisible | **P1** | Low | **Fixed** |
| L4 | Tree connectors use `└─` for all children — breaks visual hierarchy | **P1** | Low | **Fixed** |
| L5 | Arrow keys (`↑`/`↓`) don't work in Agents panel | **P1** | Low | **Fixed** |
| L6 | `j`/`k` in Agents panel overrides detail pane scroll when node is focused | **P1** | Low | **Fixed** |
| L7 | `g`/`G` don't work in Agents panel | P2 | Low | **Fixed** |
| L8 | Status bar hints don't adapt to active panel | P2 | Medium | Documented |
| L9 | `Succeeded` status is Blue instead of Green | P2 | Low | Documented |
| L10 | Agent name padding uses bytes, not display cells (CJK overflow) | P2 | Low | Documented |

---

## Implementation Summary

### `crates/spur-tui/src/components/agents_tree.rs`
1. Added `scroll_offset: usize` with scroll APIs (`scroll_up`, `scroll_down`, `scroll_to_top`, `scroll_to_bottom`)
2. Changed `render` to `&mut self`; applies `.scroll((offset, 0))` to Paragraph
3. Auto-scrolls viewport to keep `selected` visible
4. Added `select_first` / `select_last` for `g`/`G` navigation
5. Replaced flat `└─ ` connectors with `├─ `/`└─ ` + `│  `/`   ` vertical continuation
6. Added `▶`/`▼` collapse glyphs for nodes with children
7. Updated `render_lineage_to_strings` test helper to match new tree drawing

### `crates/spur-tui/src/views/dashboard.rs`
1. Added `&& self.focused_node.is_none()` guards to `j`/`k` panel routing in both vim_normal and insert mode
2. Added `Panel::Agents` branches to `KeyCode::Up`/`Down`
3. Added `g`/`G` branches for Agents panel (`select_first`/`select_last` + scroll)
4. Ensured detail pane scroll takes precedence over panel navigation when a node is focused

### Verification
- `cargo build -p spur-tui` ✅
- `cargo test -p spur-tui` — **366 passed, 0 failed** ✅
- `cargo clippy -p spur-tui -- -D warnings` ✅

---

*Review conducted by L9 UIUX Designer via MCTS + Visual Thinking methodology.*
