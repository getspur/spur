# Plan Inspector UI/UX Redesign — Final Design Proposal

> **Source issue:** `br-lyg` — "spur-tui: redesign plan inspector — DAG awareness, theme migration, operator-first UX"  
> **Scope:** `crates/spur-tui/src/views/plan_inspector.rs`, `plan_stage_board.rs`, `plan_task_detail.rs`  
> **Status:** Consolidated decision ready for implementation  

---

## 1. Problem Statement

The plan inspector is the operator's primary affordance for reasoning about an in-flight implementation plan. After collaborative review (kimi proposal + codex critique), eight concrete problems were identified:

| # | Problem | Location | Severity |
|---|---------|----------|----------|
| 1 | **Zero edge visualization** — `depends_on` / `blocked_by` / `unblocks` are invisible as a graph; only comma-joined strings in detail pane | `plan_task_detail.rs:53–67` | High |
| 2 | **Theme-bypass regression** — hardcoded `Color::Yellow / Cyan / DarkGray / Red` literals survive in board + detail components | `plan_stage_board.rs:38–80`, `plan_task_detail.rs:60–62` | High |
| 3 | **`blocked_by` and `depends_on` visually identical** — both render as same gray comma-list; operator can't distinguish structural wait from active failure block | `plan_task_detail.rs:53–67` | High |
| 4 | **Header burns 3 rows on metadata** — low-signal row 3 pushes kanban down | `plan_inspector.rs:430–522` | Medium |
| 5 | **Detail pane is a DB-row dump** — flat `━━ section ━━` headers with no narrative hierarchy | `plan_task_detail.rs` | Medium |
| 6 | **Stacked mode loses lane structure** — stage borders disappear below 90 cols | `plan_inspector.rs:556–590` | Medium |
| 7 | **Footer hint built ad-hoc** — wraps on narrow widths | `plan_inspector.rs:622` | Low |
| 8 | **Status glyph collisions** — `✗` used for both rejected and failed; `▶`/`▸`/`▷` collide | `plan_stage_board.rs` | Medium |

---

## 2. Design Principles

1. **Operator-first, not graph-first.** The brain asks: *what can I dispatch?* / *why is this stuck?* / *what did this unblock?* — optimize for answering these three questions, not for drawing the full DAG.
2. **Local over global.** Dependency information should be contextual to the *selected* task. Global rail systems fail in ratatui geometry (border cell ownership, variable task height, row jitter on live updates).
3. **Theme-complete.** Zero hardcoded colors. Every render path flows through `crate::theme::resolve_token` with ANSI16 fallback.
4. **Progressive disclosure.** Stacked mode (< 90 cols) must remain usable; detail pane sections are collapsible; blocked banner only renders when selected task is actually blocked.

---

## 3. Phase 1 — Theme Migration & Layout Hardening

*Separable PR. Closes PR3/PR4 wave-2 regression. Blocks Phase 2.*

### 3.1 Theme-token migration (`plan_stage_board.rs`, `plan_task_detail.rs`)

**Remove every `Color::*` literal** and replace with token-driven resolution.

#### `plan_stage_board.rs` changes

| Current literal | Replacement token | Notes |
|-----------------|-------------------|-------|
| `Color::Yellow` (selection arrow) | `plan_inspector.board.selection.fg` | NEW — replaces `Color::Yellow` at line 40 |
| `Color::Cyan` (active stage border/title) | `plan_inspector.board.stage.active.fg` | NEW — replaces `Color::Cyan` at lines 72, 78, 99 |
| `Color::DarkGray` (inactive border/title) | `plan_inspector.board.stage.inactive.fg` | NEW — replaces `Color::DarkGray` at lines 74, 81 |
| `Color::Blue` (dispatched status) | `plan_inspector.status.dispatched.fg` | NEW |
| `Color::Yellow` (awaiting_review) | `plan_inspector.status.awaiting_review.fg` | NEW |
| `Color::Green` (approved) | `plan_inspector.status.approved.fg` | NEW |
| `Color::Red` (rejected/failed) | Split to `plan_inspector.status.rejected.fg` / `.failed.fg` | NEW — disambiguate |
| `Color::White` + `bg(Color::Red)` (blocked_on_setup_conflict) | `plan_inspector.status.blocked.fg` + `.blocked.bg` | NEW |
| `Color::Magenta` (cancelled) | `plan_inspector.status.cancelled.fg` | NEW |
| `Color::Magenta` (retry chip) | `plan_inspector.chip.retry.fg` | NEW — mapped from existing palette role |
| `Color::Cyan` (live node chip) | `plan_inspector.chip.live.fg` | NEW |
| `Color::Red` (blocked chip) | `plan_inspector.chip.blocked.fg` | NEW |
| `Color::DarkGray` (depends_on chip) | `plan_inspector.chip.depends.fg` | NEW |

#### `plan_task_detail.rs` changes

| Current literal | Replacement token | Notes |
|-----------------|-------------------|-------|
| `Color::Red` (blocked_by key) | `plan_inspector.detail.blocked_label.fg` | NEW |
| `Color::Red` (blocked_by value) | `plan_inspector.detail.blocked_value.fg` | NEW |
| `Color::DarkGray` (section header) | `plan_inspector.detail.section.fg` | NEW |
| `Color::Cyan` (kv label) | `plan_inspector.detail.label.fg` | NEW |

#### New tokens to add to `tokens.rs`

```rust
// plan_inspector board tokens
("plan_inspector.board.selection.fg", "accent"),
("plan_inspector.board.stage.active.fg", "accent"),
("plan_inspector.board.stage.inactive.fg", "fg_subtle"),

// status tokens (disambiguated)
("plan_inspector.status.dispatched.fg", "info"),
("plan_inspector.status.awaiting_review.fg", "warning"),
("plan_inspector.status.approved.fg", "success"),
("plan_inspector.status.rejected.fg", "warning"),   // distinct from failed
("plan_inspector.status.failed.fg", "danger"),      // distinct from rejected
("plan_inspector.status.blocked.fg", "fg_on_danger"),
("plan_inspector.status.blocked.bg", "danger"),
("plan_inspector.status.cancelled.fg", "accent_alt"),
("plan_inspector.status.pending.fg", "fg_subtle"),
("plan_inspector.status.ready.fg", "fg_muted"),

// chip tokens
("plan_inspector.chip.retry.fg", "accent_alt"),
("plan_inspector.chip.live.fg", "info"),
("plan_inspector.chip.blocked.fg", "danger"),
("plan_inspector.chip.depends.fg", "fg_subtle"),

// detail pane tokens
("plan_inspector.detail.hero.fg", "fg"),
("plan_inspector.detail.hero.agent.fg", "accent"),
("plan_inspector.detail.blocked_label.fg", "danger"),
("plan_inspector.detail.blocked_value.fg", "danger"),
("plan_inspector.detail.blocked_banner.fg", "fg_on_danger"),
("plan_inspector.detail.blocked_banner.bg", "danger"),
("plan_inspector.detail.section.fg", "fg_subtle"),
("plan_inspector.detail.label.fg", "accent"),
("plan_inspector.detail.edge.structural.fg", "fg_subtle"),
("plan_inspector.detail.edge.blocked.fg", "danger"),
("plan_inspector.detail.edge.highlight.fg", "accent"),

// header tokens
("plan_inspector.header.next_action.fg", "fg"),
("plan_inspector.header.count.dispatched.fg", "info"),
("plan_inspector.header.count.awaiting_review.fg", "warning"),
("plan_inspector.header.count.ready.fg", "success"),
("plan_inspector.header.count.blocked.fg", "danger"),
("plan_inspector.header.count.failed.fg", "danger"),
```

### 3.2 Status glyph disambiguation

| Status | Old glyph | New glyph | Rationale |
|--------|-----------|-----------|-----------|
| Selection indicator | `▶` | `▶` | **Reserved exclusively** for selection cursor |
| pending | `[QUE]` | `○` | Outlined circle = queued, not started |
| ready | `[RDY]` | `◐` | Half-filled = ready to dispatch |
| dispatched | `[RUN]` | `◉` | Filled circle = actively running |
| awaiting_review | `[REV]` | `◈` | Diamond = needs attention (review) |
| approved | `[PAS]` | `✓` | Check = done |
| rejected | `[REJ]` | `⊘` | Circled slash = rejected (actionable, retry possible) |
| failed | `[ERR]` | `✕` | Cross = failed (terminal unless mutated) |
| cancelled | `[SKP]` | `⊝` | Minus circle = skipped/cancelled |
| superseded | `[SUP]` | `⇢` | Arrow = forwarded to successor |
| blocked_on_setup_conflict | `BLOCKED` | `⦿` | Bullseye = blocked, hard stop |

> **Note on `[…]` brackets:** The 5-char bracketed badges consume significant horizontal space in narrow columns. Single Unicode glyphs free ~4 chars per task for task names.

### 3.3 Header compression (3 rows → 2 rows)

**Current:**
```
Row 0:  Plan: plan-abc  RUNNING  0/3 done          [========>          ] 0/3 done
Row 1:  next: dispatch first stage
Row 2:  source: work item: bd-epic    owner: brain-1
```

**Proposed:**
```
Row 0:  Plan: plan-abc  ◉ 2  ◈ 1  ◐ 3  ⦿ 2  ✕ 1     next: dispatch S2.api
        blocked: lint ← S1.ui   setup-conflict ← S0.env
Row 1:  [========>          ] 1/3 done
```

**Implementation:** Replace the 3-row vertical split with a 2-row layout:
- Row 0: `Constraint::Length(1)` — operator summary line (counts + next action + blocker rollup)
- Row 1: `Constraint::Length(1)` — progress gauge (full width, no split)

The `source:` and `owner:` metadata move to the detail pane hero line.

#### Operator summary line builder

```rust
fn build_operator_summary(plan: &TrackedPlan, theme: &Theme, width: u16) -> Line<'static> {
    // Count chips: RUN 2  REV 1  RDY 3  BLOCKED 2  FAIL 1
    // If width < 80, collapse to numeric-only:  ◉2 ◈1 ◐3 ⦿2 ✕1
    // If width < 50, show only non-zero counts + next action truncation
}
```

### 3.4 Detail pane reorganization

Current flat `━━ section ━━` dump → narrative hierarchy with hero element.

**New structure (top to bottom):**

```
┌─ Task detail (1-12/34 35%) ──────────────────────────────┐
│ build-api                                    codex  retry 2/3 │  <- HERO
│                                                              │
│  BLOCKED  lint rejected 18m ago                              │  <- conditional banner
│                                                              │
│  Parents        Children          Blocked by                 │  <- dep strip
│  ↑ bootstrap    → test-e2e        lint (REJ)                 │
│                 → deploy                                     │
│                                                              │
│  branch: spur/plan-staging/...                               │  <- execution strip
│  delegation: del-A                                           │
│                                                              │
│  ── Output ───────────────────────────────────────────────── │  <- collapsible
│  summary: api crate builds clean                             │
│  diff: 3 files +45/-12                                       │
│                                                              │
│  ── Linked issue ─────────────────────────────────────────── │  <- collapsible
│  bd-epic.2 · build-api integration                           │
└──────────────────────────────────────────────────────────────┘
```

#### Hero line

```rust
fn render_hero_line(task: &TrackedTask, live_node: Option<&ExecutorNode>) -> Line<'static> {
    // "build-api" (bold, fg) + "  " + "codex" (accent) + "  retry 2/3" (accent_alt)
    // If live_node present: "codex:run" instead of static agent
}
```

#### Blocked banner (conditional)

Only renders when `!task.blocked_by.is_empty()` or `task.status == "blocked_on_setup_conflict"`.

```
  BLOCKED  lint rejected 18m ago
```

Style: `blocked_banner.fg` on `blocked_banner.bg` (inverse/danger).

#### Dependency strip

Three-column mini-table within the detail pane:

```
  Parents (fg_subtle)      Children (fg_subtle)     Blocked by (danger)
  ↑ bootstrap              → test-e2e               lint (REJ)
                           → deploy
```

- `↑` = structural parent (`depends_on`)
- `→` = structural child (`unblocks`)
- `(status)` = resolved status from plan.tasks lookup; blocked tasks show `(REJ)` / `(ERR)` in danger color

If a referenced task ID isn't in the current plan, show the raw ID in muted color.

#### Execution strip

Compact inline fields (no section header):
```
  branch: spur/plan-staging/bfe73...    delegation: del-A
```

#### Collapsible sections

Output and Issue detail sections become collapsible. Default state:
- Output: expanded if `summary` / `feedback` / `error` / `diff_summary` present
- Issue: collapsed unless `open_issue_id` is active

> **Scroll behavior:** Detail pane scroll offset applies to the entire pane. Collapsible sections don't need independent scroll; they contribute to total line count.

### 3.5 Footer hint builder

Replace ad-hoc `format!` with a structured hint builder that respects width:

```rust
fn build_footer_hint(
    open_issue: bool,
    has_linked_issue: bool,
    width: u16,
) -> Line<'static> {
    let mut parts = vec!["h/l: lane", "j/k: task"];
    if open_issue {
        parts.push("Enter: close");
        parts.push("j/k: scroll");
        parts.push("g/G: top/btm");
    } else if has_linked_issue {
        parts.push("Enter: issue");
    } else {
        parts.push("Enter: —");
    }
    parts.push("o: work item");
    parts.push("Alt+P/Esc: close");

    // If width < 80, drop scroll hints; if < 50, drop lane/task labels
    // Join with "  "
}
```

---

## 4. Phase 2 — Operator-First Features

*Depends on Phase 1. Includes readability spike validation.*

### 4.1 Selected-task local dependency strip

Instead of global gutter rails (rejected — see §5), the detail pane hosts a **local dependency strip** that answers all three operator questions for the *selected* task only.

**Layout within detail pane (fixed 3-column):**

```
  ┌ Parents ───────┬─ Children ───────┬─ Blocked by ─────┐
  │ ↑ bootstrap    │ → test-e2e       │ lint (REJ)       │
  │                │ → deploy         │                  │
  └────────────────┴──────────────────┴──────────────────┘
```

Each cell:
- Task name (truncated to column width minus glyph prefix)
- Status suffix in parens when available: `(RUN)`, `(REJ)`, `(ERR)`
- Color: structural parents/children use `edge.structural.fg`; blocked-by uses `edge.blocked.fg`; selected task's own edges use `edge.highlight.fg`

**Column width formula:**
```rust
let col_width = (detail_area.width.saturating_sub(4)) / 3; // 2 borders + 2 dividers
```

**Empty state:** If a column has no entries, render "—" in `fg_subtle`.

### 4.2 Age/risk cues

Replace "critical path" concept (deferred indefinitely — misleading for software work with retries/rejections).

**Risk banner in operator summary line:**
```
risk: lint rejected 18m ago · build-ui running 42m · api retry 2/3
```

Computed from:
- `blocked_by` tasks that are `rejected` or `failed` → show age since last status change
- `dispatched` tasks running longer than median → show elapsed time
- Tasks on attempt > 1 → show retry count

**Per-task risk chip** (rendered in board meta line):
```
  codex:run  ⚠ retry 2/3
```

### 4.3 "Jump to blocker" navigation

**New keybinding:** `b` (when selected task has blockers)

Cycles through `blocked_by` entries, setting selection to each blocker task in turn. If a blocker is not in the current plan (external dependency), flash hint: "Blocker not in plan: {id}".

No `H`/`L` edge navigation — ambiguous semantics with multiple parents.

---

## 5. Rejected Approaches (Documented)

| Approach | Why Rejected |
|----------|-------------|
| **Option A: Inter-stage rail gutter** | Geometry doesn't work: borders own their cells, rails can't attach; variable task heights cause stacked corners that look like border damage; row jitter on live updates makes rails unstable |
| **Option B: Inline graph lane** | Loses cross-stage edge identity; same jitter problem |
| **Option C: Mermaid raster overlay** | Diagnostic-only; image-protocol dependent; not portable |
| **Static critical path highlighting** | Misleading metric: retries/rejections/conflicts dominate, so static path length doesn't reflect actual bottleneck |
| **`H`/`L` edge navigation** | Ambiguous when task has multiple parents; "jump to blocker" (`b`) is more direct |
| **21 speculative chip tokens** | Bloat; chips map to existing semantic roles (`accent`, `info`, `danger`, etc.) |

---

## 6. Stacked Mode (< 90 cols)

**Current:** Flat list with stage labels but no borders; "Selected:" one-liner.

**Proposed:** Preserve stage group structure with lightweight separators.

```
┌─ Plan board ─────────────────────────┐
│ Stage 0                              │
│  ○ bootstrap                         │
│  ◐ gen-migration                     │
│ ─────────────────────────────────────│
│ Stage 1                              │
│▶ ◉ build-api     codex:run           │
│  ◐ build-ui                          │
│ ─────────────────────────────────────│
│ Stage 2                              │
│  ◈ test-e2e                          │
└──────────────────────────────────────┘
Selected: build-api
```

Changes:
- Stage labels bold with `plan_inspector.board.stage.active.fg` for active stage
- Horizontal rule (`─`) between stages (uses border color)
- Selection indicator `▶` remains at line start
- Meta chips shown inline when width ≥ 50, hidden below

---

## 7. Theme System Contract

All new colors must:
1. Be defined in `crates/spur-tui/src/theme/tokens.rs` with bindings to palette roles
2. Be resolvable through `resolve_token(theme, token, ColorDepth::Truecolor)` in production
3. Have an `ansi` fallback in the palette entry (guaranteed by loader for built-ins)
4. Degrade gracefully for `ColorDepth::Ansi16` and monochrome (use palette roles that differ in ANSI16)

No `Color::*` literals may remain in the three target files after Phase 1.

---

## 8. File Change Map

| File | Phase | Changes |
|------|-------|---------|
| `crates/spur-tui/src/theme/tokens.rs` | 1 | Add ~25 new token bindings |
| `crates/spur-tui/src/components/plan_stage_board.rs` | 1 | Replace all `Color::*` with `resolve_token`; new glyph set; meta chip tokens |
| `crates/spur-tui/src/components/plan_task_detail.rs` | 1 | Replace all `Color::*` with `resolve_token`; restructure layout (hero → banner → dep strip → execution → collapsible sections) |
| `crates/spur-tui/src/views/plan_inspector.rs` | 1 | Header 3→2 rows; operator summary line; footer hint builder; metadata relocation |
| `crates/spur-tui/src/views/plan_inspector.rs` | 2 | `b` keybinding for jump-to-blocker; risk banner computation |

---

## 9. Acceptance Criteria

### Phase 1

- [ ] Zero `Color::*` literals remain in `plan_stage_board.rs` and `plan_task_detail.rs`
- [ ] All new tokens resolve in `dark`, `light`, and `high-contrast` built-in themes
- [ ] Header compressed to 2 rows; operator summary line shows counts + next action
- [ ] Detail pane reorganized: hero line → conditional blocked banner → dep strip → execution strip → collapsible output/issue
- [ ] `blocked_by` styled distinct from `depends_on` in both board chips and detail pane
- [ ] Status glyphs disambiguated per §3.2; `▶` reserved for selection only
- [ ] Footer hint uses structured builder; never wraps on width ≥ 40
- [ ] Existing test `o_opens_source_work_item_from_plan_inspector` passes
- [ ] New tests: blocked-banner conditional render; theme token resolution for all new tokens; stacked mode renders stage separators

### Phase 2

- [ ] Selected-task local dep strip renders parents/children/blocked-by in 3-column layout
- [ ] Age/risk cues computed and displayed in operator summary line
- [ ] `b` keybinding cycles through blockers; flash hint for external blockers
- [ ] No global rail/track-assignment code introduced

---

## 10. Mockups

### Wide mode (≥ 120 cols)

```
┌─ Plan Inspector ─────────────────────────────────────────────────────────────┐
│ Plan: bd-1dwm  ◉2 ◈1 ◐3 ⦿2 ✕1    next: dispatch S2.api                       │
│ [████████████████████░░░░░░░░░░░░░░░░░░] 5/12 done                           │
│┌─ Stage 0 ───────┬─ Stage 1 ───────┬─ Stage 2 ───────┬─ Stage 3 ───────┐│
││▶ ◉ bootstrap     │  ◐ build-api    │  ◈ test-e2e    │  ✓ deploy       ││
││   codex:run      │   codex:run     │                │                 ││
││  ◐ gen-migration │  ◐ build-ui     │  ✕ lint        │                 ││
││                  │   retry 2/3     │   (REJ)        │                 ││
│└──────────────────┴──────────────────┴──────────────────┴──────────────────┘│
│┌─ Task detail ────────────────────────────────────────────────────────────┐│
││ build-api                                                   codex:run    ││
││                                                                          ││
││  Parents        Children          Blocked by                             ││
││  ↑ bootstrap    → test-e2e       lint (REJ)                              ││
││                 → deploy                                                 ││
││                                                                          ││
││  branch: spur/plan-staging/bfe73...    delegation: del-A                 ││
││                                                                          ││
││  ── Output ───────────────────────────────────────────────────────────── ││
││  summary: api crate builds clean                                         ││
││  diff: 3 files +45/-12                                                   ││
│└──────────────────────────────────────────────────────────────────────────┘│
│ h/l: lane  j/k: task  Enter: issue  o: work item  g/G: ends  Alt+P/Esc: close│
└──────────────────────────────────────────────────────────────────────────────┘
```

### Narrow mode (50–89 cols)

```
┌─ Plan Inspector ─────────────────────────────┐
│ Plan: bd-1dwm  ◉2◈1◐3  next: dispatch S2.api │
│ [████████░░░░░░░░░░] 5/12 done               │
│┌─ Plan board ────────────────────────────────┐
││ Stage 0                                    │
││▶ ◉ bootstrap                              │
││  ◐ gen-migration                           │
││ ───────────────────────────────────────────│
││ Stage 1                                    │
││  ◐ build-api                               │
││  ◐ build-ui                                │
││ ───────────────────────────────────────────│
││ Stage 2                                    │
││  ◈ test-e2e                                │
││  ✕ lint                                    │
│└─────────────────────────────────────────────┘
│ Selected: build-api
│┌─ Task detail ───────────────────────────────┐
││ build-api                     codex:run     │
││                                            │
││  Parents    Children    Blocked by         │
││  ↑ boot     → test      lint (REJ)         │
││             → deploy                         │
││                                            │
││  branch: spur/plan-stag...                 │
││                                            │
││  ── Output ────────────────────────────────│
││  summary: api crate builds clean           │
││  diff: 3 files +45/-12                     │
│└─────────────────────────────────────────────┘
│ h/l: lane  j/k: task  Enter: issue  Esc: close
└──────────────────────────────────────────────┘
```

### ANSI16 fallback (monochrome-safe)

- All `accent`/`info`/`success`/`warning`/`danger` roles map to distinct ANSI colors in built-in themes
- Glyphs remain readable without color (shapes are distinct: `○ ◐ ◉ ◈ ✓ ⊘ ✕ ⊝ ⇢ ⦿`)
- Bold/italic modifiers supplement color for status differentiation

---

## 11. References

- Source files:
  - `crates/spur-tui/src/views/plan_inspector.rs`
  - `crates/spur-tui/src/components/plan_stage_board.rs`
  - `crates/spur-tui/src/components/plan_task_detail.rs`
  - `crates/spur-tui/src/theme/tokens.rs`
  - `crates/spur-tui/src/theme/resolver.rs`
- Issue: `br-lyg`
- Theme migration commits: `45330b89`, `f9f7e178`, `5f801439`, `71efbbdc`
- Industry refs: [lazygit](https://github.com/jesseduffield/lazygit), [ascii-dag](https://lib.rs/crates/ascii-dag), [gh-dash](https://github.com/dlvhdr/gh-dash)
