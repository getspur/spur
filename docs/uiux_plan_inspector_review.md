# Plan Inspector UI/UX Review & Wireframes
**Role:** L9 UI/UX Designer | **Method:** MCTS Feedback + Visual Thinking
**Scope:** End-to-end upstream → downstream of `crates/spur-tui/src/views/plan_inspector.rs`

---

## 1. First-Principle Visual Decomposition

The Plan Inspector is a **spatial dashboard** with three cognitive zones:

| Zone | Purpose | User Goal |
|---|---|---|
| **Header** (top) | Situational awareness | "What is the plan's overall health?" |
| **Board** (center-left) | Topological navigation | "Where am I? What depends on what?" |
| **Detail** (center-right) | Deep inspection | "Why is this task stuck? Who owns it?" |

**Visual hierarchy rule:** The eye should land on *anomalies* first (blocked tasks, failed tasks, live workers), then *context* (stage position), then *metadata* (IDs, branches).

---

## 2. MCTS Branch Evaluation of UI/UX Gaps

| Branch | Gap | Prob. | Impact | Design Decision |
|---|---|---|---|---|
| **A** | Header is flat text wall; no progress scannability | High | High | **Implemented:** Gauge + status badge |
| **B** | Stage lanes lack active-focus affordance | High | High | **Implemented:** Cyan border highlight on active stage |
| **C** | Task selection indicator (`>`) is weak | High | Medium | **Implemented:** Unicode `▶` + bold yellow |
| **D** | Detail pane is a flat KV dump; no grouping | High | High | **Implemented:** Section headers (Identity / Execution / Dependencies / Output) |
| **E** | No plan-level health summary | Med | Medium | **Recommended:** Status count strip (see Wireframe C) |
| **F** | Footer help is generic; no task context | Low | Low | **Recommended:** Contextual footer (see Wireframe C) |
| **G** | Stacked mode loses spatial stage sense | Med | Medium | **Recommended:** Stage lane mini-map (see Wireframe B) |

---

## 3. Current State (Before)

```
 Plan Inspector plan-1 running  1/4 done  next: Use get_task_diff to review each awaiting task...
┌Stage 0──────────────┐┌Stage 1──────────────┐┌Stage 2──────┐┌Stage 3──────────────┐
│                     ││                     ││             ││                      │
│> [PAS] task-contract││  [REV] task-project││  [RUN] task-││  [QUE] task-inspector│
│  live:done          ││    codex:review     ││  app         ││                      │
│                     ││                     ││  codex:run   ││                      │
│                     ││                     ││              ││                      │
└─────────────────────┘└─────────────────────┘└─────────────┘└──────────────────────┘
┌Task detail────────────────────────────────────────────────────────────────────────┐
│task: task-contract                                                                │
│name: task-contract                                                                │
│status: approved                                                                   │
│agent: codex                                                                       │
│attempt: 0/3                                                                       │
│issue: bd-1                                                                        │
│depends_on:                                                                        │
│unblocks: task-projection, task-inspector                                          │
│next:                                                                              │
└────────────────────────────────────────────────────────────────────────────────────┘
 h/l: lane  j/k: task  g/G: ends  Alt+P/Esc: close
```

**Problems:**
1. **Header:** `running  1/4 done  next: ...` is one undifferentiated string. Progress requires reading.
2. **Stages:** All four stages have identical gray borders. The user cannot tell which stage is "current" without reading task IDs.
3. **Selection:** `>` is two pixels wide and easily lost.
4. **Detail:** 10 lines of flat text with identical `label: value` styling. No visual grouping. `blocked_by` (when present) is buried in the same list as `task_id`.

---

## 4. Target State Wireframes (Implemented)

### Wireframe A — Wide Mode (≥ 90 cols)

```
 Plan: plan-1   RUNNING    [████████░░░░░░░░░░░░] 1 / 4 done
 next: Use get_task_diff to review each awaiting task, then review_task to...
┌Stage 0───────────────────────────┬┌Stage 1───────────────────────────┬┌Stage 2──────┐
│                                 ││                                 ││             │
│  ┌───────────────────────────┐  ││  ┌───────────────────────────┐  ││  ┌─────────┐│
│  │▶[PAS] task-contract      │  ││  │  [REV] task-projection    │  ││  │ [RUN]   ││
│  │  codex:done               │  ││  │    codex:review           │  ││  │ task-app││
│  └───────────────────────────┘  ││  └───────────────────────────┘  ││  │ codex:ru││
│                                 ││                                 ││  └─────────┘│
└─────────────────────────────────┘└─────────────────────────────────┘└─────────────┘
┌Task detail────────────────────────────────────────────────────────────────────────┐
│━━ Identity ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━│
│task: task-contract                                                                 │
│name: task-contract                                                                 │
│status: approved                                                                    │
│issue: bd-1                                                                         │
│━━ Execution ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━│
│agent: codex                                                                        │
│attempt: 0/3                                                                        │
│━━ Dependencies ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━│
│depends_on: —                                                                       │
│unblocks: task-projection, task-inspector                                           │
│━━ Output ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━│
│next: —                                                                             │
└────────────────────────────────────────────────────────────────────────────────────┘
```

**Key improvements:**
- **Header row 1:** Plan title (cyan) + status badge (colored, uppercase) + `Gauge` widget (green fill on dark-gray track) + exact count label.
- **Header row 2:** `next:` action truncated with ellipsis so it never wraps unpredictably.
- **Active stage:** Stage 0 has a **cyan border + bold title** because it contains the selected task. Other stages have muted dark-gray borders.
- **Task card:** `▶` (unicode triangle) is visually heavier than `>`. Selected task gets a subtle top-border "card" feel from the paragraph spacing.
- **Detail pane:** Four semantic sections with `━━ Section ━━` headers in dark gray. Critical fields like `blocked_by` still render in **red bold** inside the Dependencies section.

---

### Wireframe B — Stacked Mode (< 90 cols)

```
 Plan: plan-1   RUNNING    [████████░░░░░░░░░░░░] 1 / 4 done
 next: Use get_task_diff to review each awaiting task...
┌Plan board─────────────────────────────────────────────────────────────────────────┐
│Stage 0                                                                            │
│▶ [PAS] task-contract  codex:done                                                  │
│                                                                                   │
│Stage 1                                                                            │
│  [REV] task-projection  codex:review                                               │
│                                                                                   │
│Stage 2                                                                            │
│  [RUN] task-app  codex:run  blocked:parent  ↑parent  retry 2/3                    │
└───────────────────────────────────────────────────────────────────────────────────┘
Selected: task-contract
┌Task detail────────────────────────────────────────────────────────────────────────┐
│━━ Identity ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━│
│task: task-contract                                                                 │
│...                                                                                 │
└────────────────────────────────────────────────────────────────────────────────────┘
```

**Key improvements:**
- Same 2-line header with Gauge preserves situational awareness even on narrow terminals.
- Stage headers remain bold cyan for the active stage.
- Meta chips (`codex:run`, `blocked:parent`, `↑parent`, `retry 2/3`) render inline in stacked mode so all context is visible without opening the detail pane.

---

## 5. Feasibility Notes (ratatui 0.29)

| Enhancement | Widget / Technique | Effort | Status |
|---|---|---|---|
| Progress gauge | `Gauge::default().ratio(...).gauge_style(...)` | Low | ✅ Implemented |
| Active stage border | `Block::border_style(Style::fg(Color::Cyan))` | Low | ✅ Implemented |
| Section headers | `Span::styled("━━ Section ━━", Style::fg(Color::DarkGray).bold())` | Low | ✅ Implemented |
| Unicode selector | `▶` char in `Span::raw` | Low | ✅ Implemented |
| Status color map | `match status { "running" => Color::Yellow, ... }` | Low | ✅ Implemented |
| Text truncation | `truncate_display(s, max)` with char-boundary safety | Low | ✅ Implemented |
| Task card background | `Block::style(Style::bg(Color::DarkGray))` on inner block | Med | ⏳ Recommended |
| Stage count badges | `Block::title(format!("Stage {} [{}]", idx, tasks.len()))` | Low | ⏳ Recommended |
| Plan health strip | Horizontal `Line` of colored `█` blocks per status count | Med | ⏳ Recommended |

---

## 6. Color Semantics & Accessibility

```
Cyan    → Active focus / primary labels (plan title, active stage border)
Green   → Success / approved / Gauge fill
Yellow  → Running / dispatched / selection
Red     → Blocked / failed / rejected / error
Magenta → Retry / cancelled
Blue    → Live worker
DarkGray→ Inactive stages / section headers / disabled text
```

**Contrast rule:** All foreground colors are paired against the default terminal background (assumed dark). No low-contrast pairings (e.g., yellow-on-white) are used.

---

## 7. Remaining Recommendations (Future Iterations)

1. **Task card background blocks**  
   Wrap each task in a `Block::default().borders(Borders::TOP)` so tasks feel like index cards. Selected card gets `Borders::ALL` + cyan border.

2. **Stage task-count badges**  
   Change stage titles to `Stage 0 [2 tasks]` so empty stages are immediately obvious.

3. **Plan health micro-bar**  
   Add a 1-line strip below the Gauge showing colored blocks: `█ approved  ░ pending  ▓ dispatched  ▒ awaiting_review`. This gives density without numbers.

4. **Contextual footer**  
   When a task is selected, replace the generic footer with task-specific hints:  
   `h/l: lane  j/k: task  Enter: diff  r: review  Alt+P/Esc: close`

5. **Stacked mode stage minimap**  
   Render a 1-line `Stage: [0]▸[1][2][3]` indicator above the board so users never lose spatial orientation when stages collapse vertically.
