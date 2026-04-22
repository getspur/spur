# MCTS + First Principles + Visual Thinking UI/UX Review
## Target: `crates/spur-tui/src/views/dashboard.rs` + related components
## Date: 2026-04-21
## Methodology: Multi-round Monte Carlo Tree Search over UI/UX decision space

---

## 0. Methodology Overview

### MCTS Applied to UI/UX
| Phase | UI/UX Mapping |
|-------|--------------|
| **Selection** | Prioritize high-frequency user paths (scrolling, focusing, reading status) |
| **Expansion** | Decompose each path into layout, interaction, visual, and error sub-states |
| **Simulation** | Mentally render the TUI at 80×24 and 120×40, simulating 4 personas |
| **Backpropagation** | Score findings by (severity × user frequency × fix cost) |

### First Principles Anchors
1. **Terminal UI is a grid of fixed cells** — no anti-aliasing, no animation, no z-depth beyond overlay
2. **Color is the only channel for emphasis** — beyond position and whitespace
3. **Cognitive load scales with decision uncertainty** — invisible modes are expensive
4. **Information scent must be proximal** — related data should co-locate or chain visually

### Personas Simulated
- **Alex** (First-Time): Discovers SPUR, reads hints, makes mistakes
- **Blake** (Power Operator): 10+ agents, high tempo, muscle memory expected
- **Casey** (Reviewer): Monitors delegations, cares about outcomes and blockers
- **Dana** (Debugger): Traces failures, needs deep context, frustrated by noise

---

## Round 1: Layout Architecture & Information Hierarchy

### 1.1 Visual Spatial Model
```
┌─────────────────────────────────────────────────────────────┐  ╮
│ Lineage                                                     │  │ agents_height
│ ◆ brain   BRAIN  Running   12m 34s  $0.42                   │  │ (4–12 rows)
│   └─ codex EXEC  Succeeded  8m 12s  $0.18                   │  │
├─────────────────────────────────────────────────────────────┤  │
│ Issues — [j/k] select · [Enter] detail · [W]ork             │  │ issues_height
│ ID       P Type    Status Assignee  Title                   │  │ (0–6 rows)
├─────────────────────────────────────────────────────────────┤  │
│ Activity ▼ following                                        │  │ log_chunk
│ 14:32:01 [brain] Delegating to codex: fix auth…             │  │ (remainder)
│ 14:32:15 [codex] ✓ approved "fix auth" (attempt 1/3)       │  │
│ ...                                                         │  │
├─────────────────────────────────────────────────────────────┤  │
│ [brain: streaming] ▏Type a task...                          │  │ input_height
├─────────────────────────────────────────────────────────────┤  │
│ [i]nput [Enter]focus …     3 issues · 2 running · $1.24 spur│  │ status (1)
└─────────────────────────────────────────────────────────────┘  ╯
```

### 1.2 Constraint Analysis (Exploration)
The populated-state layout uses dynamic vertical constraints:
```rust
let agents_height = (node_count as u16 + 2).clamp(4, area.height * 40 / 100).min(12);
```

**Simulation at 80×24:**
- `agents_height` = min( (8+2).clamp(4,9), 12 ) = **10 rows**
- `issues_height` = min( (5+3), 24/4=6 ) = **6 rows** (if 5 issues)
- `input_height` = **2 rows** (typical multi-line)
- `status` = **1 row**
- `log_chunk` = 24 − 10 − 6 − 2 − 1 = **5 rows**

**Finding F1.1 — CRITICAL: Log viewport starvation**
- At 24 rows with 8 agents and 5 issues, the activity log — the primary information surface — gets **5 rows**.
- The detail pane (when focused) REPLACES the log entirely, making this a modal switch on already scarce real estate.
- **Score**: 9/10 (affects all personas, every session >3 agents)
- **Recommendation**: Add a "zoom" or panel-maximize mode (e.g., `z` toggles agents tree collapse, `Z` maximizes log). Consider making issues panel horizontally collapsible into a compact badge strip.

### 1.3 Empty State vs Populated State Transition
**Empty state** (lines 375–441): Centered splash with "SPUR" branding.
**Populated state**: Instant switch to dense 4+ panel layout.

**Finding F1.2 — MODERATE: Jarring density cliff**
- Alex sees a calm splash, types a task, and the UI explodes into 4 panels with 12+ data points.
- No progressive disclosure: the first spawned agent immediately triggers full layout.
- **Score**: 5/10
- **Recommendation**: Keep a simplified "single-agent" layout for node_count == 1 — full-width detail pane, no issues panel, agents tree as a single compact header bar.

### 1.4 Issues Panel Width Budget
```rust
let widths = [
    Constraint::Length(8),   // ID
    Constraint::Length(2),   // P
    Constraint::Length(7),   // Type
    Constraint::Length(4),   // Status (abbrev: "open"→4 fits, but "in_progress"→"wip" 3 fits)
    Constraint::Length(10),  // Assignee
    Constraint::Min(20),     // Title
];
// Sum of fixed = 31; Title gets remainder
```
At 80 columns with borders: inner width ≈ 78. Title gets ~47. Acceptable.
At 60 columns (common in tmux splits): inner width ≈ 58. Title gets ~27. Truncation kicks in aggressively.

**Finding F1.3 — LOW: Table columns not responsive**
- `Constraint::Min(20)` on title assumes generous width. No `Percentage` or proportional fallback.
- **Score**: 3/10
- **Recommendation**: Use `Constraint::Percentage(40)` for title, or hide Assignee/Type columns below 70 cols.

---

## Round 2: Interaction Design & Input Handling

### 2.1 The Key Routing State Machine
`handle_key_inner` implements a 3-priority system:
1. **P0**: Tab-cycling in detail pane (Left/Right)
2. **P1**: Editing keys → InputBar
3. **P2**: Non-editing navigation keys

Within P1, there's a **Vim Normal mode intercept** when `input_bar.is_empty()`:
```rust
if self.input_bar.is_empty() && self.input_bar.is_vim_normal() {
    if let KeyCode::Char(ch) = key.code { ... }
}
```

**Visual simulation**: Alex is in Vim Normal mode. The cursor is a reversed block. InputBar is empty. Alex types `j`. Does it scroll the log? Move selection in agents tree? Scroll issue detail? Scroll detail pane?

The answer depends on INVISIBLE state:
```rust
'j' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => { scroll issue detail }
'j' if self.focused_panel == Panel::Issues => { select_next issue }
'j' if self.focused_panel == Panel::Agents => Some(Action::SelectNext)
'j' => { // default: scroll detail pane if focused, else activity log }
```

**Finding F2.1 — CRITICAL: Contextual `j`/`k` routing is invisible**
- The same physical key does 4 different things based on 3 invisible booleans.
- There is NO on-screen indicator of which interpretation is active.
- Alex will scroll the activity log, press Tab to "cycle focus" (border goes cyan on Issues), press `j`, and accidentally change issue selection instead of scrolling.
- **Score**: 9/10
- **Recommendation**: Render a micro-hint in the status bar or input bar showing the active context: `"Log" | "Agents" | "Issues" | "Detail"`. At minimum, prefix the input hint with the focused panel name.

### 2.2 The Single-Char Navigation Pattern
When InputBar has exactly 1 character and it's a nav key:
```rust
if self.input_bar.text().len() == 1 {
    let ch = self.input_bar.text().chars().next().unwrap();
    match ch {
        'j' if self.focused_panel == Panel::Issues => { self.input_bar.clear(); ... }
        ...
    }
}
```

**Simulation**: Alex types `j` intending to search for something starting with J. InputBar shows `j`. Instead of waiting for more input, the `j` is instantly consumed as a navigation command, InputBar is cleared, and an action fires.

**Finding F2.2 — HIGH: Single-char nav trap**
- This is a modal interaction that violates the principle of least astonishment.
- In Emacs mode, typing `j` should insert `j`. The fact that it gets intercepted after being inserted is disorienting.
- **Score**: 7/10
- **Recommendation**: Remove single-char nav. Navigation should ONLY work when InputBar is completely empty. If the user wants nav, they can press Esc first (which already clears in most modes).

### 2.3 Issue Detail Overlay Hotkey Shadowing
When `IssueFocus::Loaded`, keys `o`, `w`, `b`, `d` set issue status:
```rust
'o' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
    return Some(Action::Issue(...UpdateStatus { status: "open".into() }));
}
```

**Simulation**: Dana is reading an issue detail. Dana wants to scroll to top (`g`) or search for "open" (which would start with `o` if not in vim normal). But in Vim Normal + empty InputBar, `o` instantly marks the issue as open — no confirmation, no "are you sure?"

**Finding F2.3 — HIGH: Destructive hotkeys without confirmation**
- `o`, `w`, `b`, `d` mutate issue status directly from the dashboard.
- There is no undo path visible to the user.
- These keys are also common in Vim (`o` = open line below, which is explicitly listed as a mode-entry key that falls through... wait, no: `'i' | 'a' | 'A' | 'I' | 'o' | 'O' => None` at line 780. So `o` is NOT a mode-entry key in this block — it's intercepted BEFORE reaching the fallthrough.
- **Score**: 7/10
- **Recommendation**: Require Shift for destructive issue status changes (`O`, `W`, `B`, `D`) OR add a confirmation mini-prompt. At minimum, flash a transient message: `"Status changed to open — press u to undo"`.

### 2.4 Enter Key Ambiguity
```rust
if key.code == KeyCode::Enter && self.input_bar.is_empty() {
    if self.focused_panel == Panel::Issues { ... view detail ... }
    if self.focused_panel == Panel::Agents { return Some(Action::FocusNode); }
    return None;
}
```

**Simulation**: Alex is in Issues panel, wants to focus an issue. Presses Enter. But Alex was in the middle of a command — no, InputBar is empty. Still, Alex expected Enter to maybe do nothing or start input. Instead it navigates to a new view.

**Finding F2.4 — MODERATE: Enter on empty input is context-dependent**
- Same key, different action based on focused panel — but focus is only visible via border color.
- **Score**: 5/10
- **Recommendation**: Make Enter consistently "activate primary action of focused item" and add a label in the panel title indicating the primary action: `"Issues — [Enter] detail"` is already present. For Agents panel, add `"Agents — [Enter] focus"` when focused.

### 2.5 Review Decision Keys in Vim Normal
```rust
if self.focused_node.is_some() && self.detail_pane.current_tab == DetailTab::Review {
    if let Some(decision) = crate::components::review_card::decision_for_key(ch, None) { ... }
}
```

**Simulation**: Casey is on Review tab. Casey presses `a` to approve. The action fires immediately. But `a` is also the Vim mode-entry key for append. In Vim Normal, `a` should enter Insert mode. Here it's intercepted for review.

**Finding F2.5 — MODERATE: Review keys shadow Vim mode-entry**
- `a`, `d`, `m`, `R` are review decisions that override Vim Normal mode behavior.
- This is documented in help (`a / d / m / R      Approve / deny / modify / retry`), but violates Vim user expectations.
- **Score**: 5/10
- **Recommendation**: Use uppercase review decisions (`A`, `D`, `M`, `R`) when in Vim Normal mode, or require `Alt` modifier. Preserve lowercase for Vim mode-entry.

---

## Round 3: Visual Design & Accessibility

### 3.1 Focus Visibility (The Cyan Border Problem)
```rust
pub fn focused_border_style(focused: bool) -> Style {
    if focused { Style::default().fg(Color::Cyan) }
    else { Style::default().fg(Color::DarkGray) }
}
```

**Visual simulation on 256-color terminal**:
- Unfocused border: dark gray (#555555)
- Focused border: cyan (#00aaaa)
- On some terminals with poor contrast calibration, the difference is subtle.
- The panel TITLE does not change when focused — only the border.

**Finding F3.1 — HIGH: Focus indication is too subtle**
- Users (especially Alex and Dana) lose track of which panel is active.
- This directly causes the wrong-action problems in F2.1 and F2.4.
- **Score**: 7/10
- **Recommendation**: When focused, render panel title in **bold + cyan** and add a `▸` prefix. Unfocused titles should be dimmed.

### 3.2 Status Bar Information Density
The right side of the status bar accumulates:
```
"{issue_count} issues · {total} alerts · {license} · {running} running · {pending} review · ${cost} · {elapsed} · [mode] · ctx {pct}% · [Ctrl+K: go] · ?: help · spur"
```

At moderate load (5 issues, 2 alerts, 3 running, 1 review, $1.24, 12m 34s, vim mode, 45% ctx):
→ `"5 issues · 2 alerts · 3 running · 1 review · $1.24 · 12m 34s · [Normal] · ctx 45% · [Ctrl+K: go] · ?: help · spur"`

That's ~110 characters. At 80 columns, the layout splits:
```rust
let [hints_area, right_area] = Layout::horizontal([
    Constraint::Min(0),
    Constraint::Length(right_width.max(1))
]).areas(area);
```

So hints get 0 space and wrap/truncate. The hints are the MOST important thing for Alex.

**Finding F3.2 — CRITICAL: Status bar overflow pushes out hints**
- On 80-column terminals, metrics crowd out the actionable hints.
- **Score**: 8/10
- **Recommendation**: Implement a priority truncation system:
  - Tier 1 (always): hints left side
  - Tier 2 (truncate first): license badge, context %, mode
  - Tier 3 (collapse to symbols): alerts → `!2`, review → `⚠1`, running → `▶3`
  - Example compact: `"5i · !2 · ▶3 · ⚠1 · $1.24 · 12m · spur"` = ~35 chars

### 3.3 Agents Tree Readability
```rust
spans.push(Span::styled(format!("{:<12} ", node.agent), row.fg(Color::White)));
spans.push(Span::styled(format!("{:<5} ", role_label), ...));
spans.push(Span::styled(format!("{:<14} ", format!("{:?}", node.phase)), ...));
```

**Visual simulation**:
```
└─ codex       EXEC  Running        8m 12s $0.18
```

**Finding F3.3 — MODERATE: Phase uses Debug formatting**
- `format!("{:?}", node.phase)` produces strings like `"Running"`, `"AwaitingReview"`, `"Spawning"`.
- In a constrained column of 14 chars, `"AwaitingReview"` (16 chars) is already over.
- Debug format exposes Rust enum names, not user-friendly labels.
- **Score**: 5/10
- **Recommendation**: Add a `Display` or `label()` method to `LifecycleState` with compact labels: `Run`, `Review`, `Spawn`, `Done`, `Fail`, `Cancel`, `Resume`.

### 3.4 Activity Log Scannability
```rust
Line::from(vec![
    Span::styled(format!(" {} ", entry.timestamp), Style::default().fg(Color::DarkGray)),
    Span::styled(format!("{} ", entry.prefix), Style::default().fg(Color::Cyan)),
    Span::styled(&entry.message, Style::default().fg(kind_color)),
])
```

**Visual simulation**:
```
14:32:01 [brain]     Delegating to codex: fix auth refactor
14:32:15 [codex]     ✓ approved "fix auth" (attempt 1/3)
14:32:16 [worker:12] 🔧 Tool: cargo test
```

**Finding F3.4 — MODERATE: Log entries lack visual grouping**
- All prefixes are cyan; all timestamps dark gray. No vertical spacing between "events".
- When the same executor emits 10 lines in rapid succession, they blend together.
- **Score**: 5/10
- **Recommendation**: Add subtle alternation or group spacing. When prefix changes from previous line, insert a blank line. Or use prefix-specific colors (brain = magenta, worker = cyan, spur = yellow, pm = green).

### 3.5 Color Accessibility
- Red/green discrimination is used for status (`open` = green, `blocked` = red, `Failed` = red, `Succeeded` = blue).
- No secondary channel (bold, underline, icon shape) for color-only information.
- **Finding F3.5 — LOW: Colorblind-unfriendly status encoding**
- **Score**: 3/10
- **Recommendation**: Add shape/weight redundancy: `✓ Succeeded`, `✗ Failed`, `⚠ Blocked`, `▶ Running`.

---

## Round 4: Edge Cases & Failure Modes

### 4.1 Issue Focus Loading State Trap
```rust
IssueFocus::Loading { id } => {
    IssueDetailPane::render_loading(id, frame, chunks[log_chunk]);
}
```

If `IssueDetailFetched` never arrives (network error, issue deleted, race condition), the user is stuck in Loading state. `Esc` clears it, but there's no timeout or error feedback.

**Finding F4.1 — HIGH: Loading state has no timeout**
- Dana clicks an issue, sees "Loading..." forever.
- **Score**: 6/10
- **Recommendation**: Add a 5-second timeout in `tick()` that transitions `IssueFocus::Loading` to an error message if no response arrives.

### 4.2 Text Batch Unbounded Growth (Minor)
```rust
if entry.0.len() > 200 {
    let mut start = entry.0.len() - 200;
    while !entry.0.is_char_boundary(start) { start += 1; }
    entry.0 = entry.0[start..].to_string();
}
```
The `text_batch` HashMap is pruned on flush (500ms), but if a session emits text every 100ms, the entry never expires and constantly reallocates.

**Finding F4.2 — LOW: Minor allocation pressure under high-frequency streaming**
- **Score**: 2/10
- **Recommendation**: Use a fixed-size ring buffer (`VecDeque<char>`) instead of string truncation.

### 4.3 Truncation Logic Inconsistency
`truncate_display` decrements to find char boundary (correct).
`format_issue_badge` INCREMENTS:
```rust
let mut end = max_title;
while end < issue.title.len() && !issue.title.is_char_boundary(end) {
    end += 1;
}
```

This can produce truncation points that are LONGER than `max_title`, not shorter.

**Finding F4.3 — MODERATE: Badge title truncation may exceed max width**
- **Score**: 4/10
- **Recommendation**: Use `truncate_display` consistently, or fix `format_issue_badge` to decrement.

### 4.4 Scroll Clamp Lag
`DetailPane::scroll_to_bottom` sets `is_following = true` but does NOT clamp `scroll_offset`. The clamp happens in `render()`:
```rust
if self.is_following {
    self.scroll_offset = max_offset;
}
```

If `scroll_to_bottom` is called between renders (e.g., via action), `scroll_offset` remains stale until next frame.

**Finding F4.4 — LOW: One-frame scroll offset staleness**
- **Score**: 2/10
- **Recommendation**: Apply clamp in `scroll_to_bottom()` directly, or document the render-dependent invariant.

### 4.5 ActivityLog `scroll_down(20)` Magic Number
```rust
pub fn scroll_down(&mut self, visible_height: usize) {
    self.scroll_offset = self.scroll_offset.saturating_add(1);
    if self.scroll_offset >= self.entries.len().saturating_sub(visible_height) {
        self.is_following = true;
    }
}
```

Called as `self.activity_log.scroll_down(20)` in multiple places. If the actual visible height is 5 (see F1.1), the user must scroll to row 15 past the end before auto-follow re-engages.

**Finding F4.5 — MODERATE: Scroll-down uses hardcoded visible height**
- **Score**: 5/10
- **Recommendation**: Pass actual `area.height` from render context, or compute visible lines based on entry wrap counts.

---

## Summary: Scored Findings

| ID | Finding | Severity | Personas | Effort | Score | Priority |
|----|---------|----------|----------|--------|-------|----------|
| F1.1 | Log viewport starvation at 24 rows | Critical | All | Medium | 9 | P0 |
| F2.1 | Invisible `j`/`k` context routing | Critical | Alex, Blake | Low | 9 | P0 |
| F3.2 | Status bar overflow crowds hints | Critical | Alex | Low | 8 | P0 |
| F2.2 | Single-char nav trap | High | Alex | Low | 7 | P1 |
| F2.3 | Destructive issue hotkeys without confirm | High | All | Medium | 7 | P1 |
| F3.1 | Focus indication too subtle | High | Alex, Dana | Low | 7 | P1 |
| F1.2 | Jarring empty→populated transition | Moderate | Alex | Medium | 5 | P2 |
| F2.4 | Enter ambiguity across panels | Moderate | Alex | Low | 5 | P2 |
| F2.5 | Review keys shadow Vim mode-entry | Moderate | Blake | Low | 5 | P2 |
| F3.3 | Phase uses Debug format | Moderate | All | Low | 5 | P2 |
| F3.4 | Log lacks visual grouping | Moderate | Dana | Medium | 5 | P2 |
| F4.1 | Issue loading no timeout | High | Dana | Low | 6 | P1 |
| F4.5 | Hardcoded visible height in scroll | Moderate | Blake | Low | 5 | P2 |
| F1.3 | Issues table not responsive | Low | Blake | Low | 3 | P3 |
| F3.5 | Colorblind-unfriendly status | Low | All | Low | 3 | P3 |
| F4.3 | Badge truncation logic bug | Moderate | Casey | Low | 4 | P2 |
| F4.2 | Text batch allocation pressure | Low | Blake | Low | 2 | P3 |
| F4.4 | Scroll clamp one-frame lag | Low | Blake | Low | 2 | P3 |

---

## Recommended Action Plan

### Immediate (P0) — Merge-blocking UX issues
1. **Add focused panel indicator** to status bar or input hint (fixes F2.1, F3.1)
2. **Implement status bar compact mode** when `right_width > area.width / 2` (fixes F3.2)
3. **Add panel zoom key** (`Alt+↑` / `Alt+↓` or `z`) to collapse/expand agents tree and issues panel (fixes F1.1)

### Short-term (P1) — High user friction
4. Remove single-char nav interception; require empty InputBar (fixes F2.2)
5. Add confirmation or undo flash for issue status changes (fixes F2.3)
6. Add 5s timeout on `IssueFocus::Loading` (fixes F4.1)

### Medium-term (P2) — Polish and refinement
7. Add compact `LifecycleState` labels (fixes F3.3)
8. Pass real visible height to scroll methods (fixes F4.5)
9. Add log entry grouping by prefix (fixes F3.4)
10. Implement progressive layout for 1-agent state (fixes F1.2)
11. Fix `format_issue_badge` truncation (fixes F4.3)

### Long-term (P3) — Accessibility and edge cases
12. Add icon/shape redundancy to color-coded status (fixes F3.5)
13. Make issues table columns responsive (fixes F1.3)
14. Optimize text batch buffer (fixes F4.2)
15. Sync scroll clamp immediately (fixes F4.4)

---

## Appendix: Persona Journey Maps

### Alex (First-Time User) — Before Fixes
```
[0:00]  Sees splash: "Type a task below to start" → feels invited
[0:05]  Types "/help" → sees command hint → feels guided
[0:10]  Presses Enter → help overlay appears → positive
[0:15]  Types "fix the auth bug" → submits → agents tree appears
[0:16]  UI suddenly has 4 panels, 20 data points → overwhelmed
[0:20]  Wants to scroll log → presses j → agents tree selection moves
[0:21]  Presses Tab → subtle border color change → doesn't notice
[0:22]  Presses j again → issue selection moves → very confused
[0:25]  Presses Enter → unexpectedly opens issue detail → frustrated
[0:30]  Presses Esc → back to dashboard → tries to find hint for "how to scroll"
[0:35]  Status bar hints are truncated off-screen → can't find answer
[0:40]  Quits, writes off SPUR as "too confusing"
```

### Blake (Power Operator) — Before Fixes
```
[0:00]  Opens SPUR, 12 agents already running from previous session
[0:01]  Agents tree capped at 12 rows, can't see bottom 3 without scrolling
[0:02]  Activity log is 4 rows tall, streaming output unreadable
[0:03]  Presses z → nothing happens → no zoom key exists
[0:04]  Collapses agents tree with 'c' → still only gains 2 rows
[0:05]  Switches to SessionDetail for primary brain → better but loses overview
[0:06]  Context-switches between Dashboard and SessionDetail repeatedly
[0:10]  Wishes for tmux-like pane zoom or detachable panels
```

### Casey (Reviewer) — Before Fixes
```
[0:00]  Opens SPUR, status bar shows "3 review" in yellow
[0:01]  Presses 'r' → jumps to first review → good
[0:02]  Review card is 4 rows tall in a 6-row detail pane → cramped
[0:03]  Wants to see all pending reviews as a list → no such view exists
[0:04]  Must hunt through agents tree for ⚠ symbols → inefficient
[0:05]  Approves review with 'a' → works, but wishes for bulk approve
```

### Dana (Debugger) — Before Fixes
```
[0:00]  Notices red ✗ in agents tree for worker "codex"
[0:01]  Navigates to node, detail pane shows Stream tab
[0:02]  Cycles to Attempts tab → sees "#3: Failed cost=$0.42"
[0:03]  Error message is not in Attempts tab → must check Stream or ActivityLog
[0:04]  Activity log has 5000 entries, scrolls for 30s to find codex errors
[0:05]  Wishes for "filter by executor" or "show errors only"
[0:06]  Wishes Attempts tab showed the actual error message, not just status
```

---

*Review generated via MCTS exploration of 4 rounds × 4 personas × 3 viewport sizes. All findings are grounded in direct code analysis of `dashboard.rs`, `detail_pane.rs`, `agents_tree.rs`, `activity_log.rs`, `issues_panel.rs`, `status_bar.rs`, `issue_detail_pane.rs`, and `input_bar.rs`.*
