# UI/UX Review: Dashboard Keyboard Bindings End-to-End
**Persona:** L9 UI/UX Designer — MCTS + Visual Thinking
**Scope:** `crates/spur-tui/src/views/dashboard.rs` keyboard routing
**Date:** 2026-04-22
**Premise:** *"Keyboard bindings are very hard to use"* — user report

---

## Executive Summary

The keyboard binding system is not "hard to use" because of bad key choices. It is hard to use because **the dashboard tries to be two products at once** — a monitoring dashboard AND a chat interface — without explicit modality. This forces a `key_owner()` heuristic that guesses user intent, which is inherently unreliable. The result is a state matrix with 48+ branches where a single key (`j`) can do 6 different things depending on invisible state.

**Root cause:** No explicit input modality. The input bar is always "listening," so the system must infer whether `j` means "scroll down" or "type the letter j."

**Design principle violated:** [Mode visibility](https://www.nngroup.com/articles/ui-modes/) — users cannot see which mode they're in, so they cannot predict key behavior.

---

## MCTS Review: Simulated Personas

### Round 1 — First-Time User (discovery phase)

**Scenario:** Open dashboard, see agents running, try to navigate.

| Action | Expected | Actual | Result |
|--------|----------|--------|--------|
| Press `j` | Move down in tree | Works (if input empty) | ✅ |
| Type `j` in input | Letter j appears | Works (if input not empty) | ✅ |
| Press `j` after typing a word | Navigate tree | Goes to composer (cursor moves) | ⚠️ Confusing |
| Press `Esc` to "exit input" | Return to navigation | Depends on vim mode / other state | ❌ Unpredictable |
| Press `Tab` | Cycle panels | Works | ✅ |
| Press `Enter` on agent | Focus agent | Works | ✅ |
| Press `j` after focusing agent | Scroll detail pane | Works | ✅ |
| Press `Esc` after focusing | Unfocus, return to tree | Works | ✅ |
| Press `g` | Jump to top of tree | Works (if Agents focused) | ✅ |
| Press `g` after focusing agent | Jump to top of detail | Works | ✅ |
| Press `g` in Issues panel | Jump to first issue | Works | ✅ |
| Press `G` in Log panel | Jump to bottom of log | Works | ✅ |

**Pain points discovered:**
- R1.1: After typing anything in input bar, ALL navigation keys stop working until Backspace clears the bar. No visual indicator of this state change.
- R1.2: `Esc` behavior is invisible — it closes issue detail, unfocuses node, or navigates back depending on state. User cannot know which will happen.
- R1.3: The context hint at the bottom changes dynamically but is too subtle. Users don't read it.

### Round 2 — Power Operator (heavy usage, speed matters)

**Scenario:** 50+ agents, 20+ issues, rapid navigation between panels.

| Action | Expected | Actual | Result |
|--------|----------|--------|--------|
| `jjjj` in Agents panel | Move down 4 agents | Works | ✅ |
| `kk` in Issues panel | Move up 2 issues | Works | ✅ |
| `5j` in Agents (vim-style count) | Move down 5 | Not supported | ❌ |
| `Ctrl+d` in Log | Half-page down | Not supported | ❌ |
| `Ctrl+u` in Log | Half-page up | Not supported | ❌ |
| `/` to search agents | Filter tree | Only opens command palette | ❌ |
| `n` / `N` to repeat search | Next/prev result | Not supported | ❌ |
| `Space` in detail pane | Page down | Not supported | ❌ |
| `Shift+Space` in detail pane | Page up | Not supported | ❌ |
| `H` / `L` for tab cycling | Left/right tab | Only ←/→ arrows work | ⚠️ |
| `Ctrl+w` then `j` | Move to panel below (vim split) | Not supported | ❌ |
| `q` to quit | Exit app | Not supported (Ctrl-C only) | ⚠️ |

**Pain points discovered:**
- R2.1: No page-wise scrolling (Space, Ctrl+D/U, PageUp/PageDown). Log scrolling is line-by-line only.
- R2.2: No vim count prefix (`5j`, `10G`). Power users expect this.
- R2.3: No search within panels. `/` only opens command palette, not local search.
- R2.4: Tab cycling is only via `Tab` (one direction). No `Shift+Tab` for reverse.
- R2.5: No direct panel jumping (`1` = Agents, `2` = Issues, `3` = Log).

### Round 3 — Reviewer (audit mode, checking agent outputs)

**Scenario:** Review multiple agent completions, compare diffs.

| Action | Expected | Actual | Result |
|--------|----------|--------|--------|
| Focus agent, `r` | Jump to next review | Works | ✅ |
| Review tab, `a` | Approve | Works (if input empty) | ✅ |
| Review tab, `d` | Deny | Works (if input empty) | ✅ |
| Review tab, type reason after `d` | Enter reason | Can't — `d` is swallowed | ❌ |
| Switch between Stream and Diff tabs | Quick compare | ←/→ arrows work | ✅ |
| `v` for verbose | Toggle verbose | Works | ✅ |
| `z` for zoom | Maximize log pane | Works | ✅ |

**Pain points discovered:**
- R3.1: Review decisions (`a`/`d`/`m`/`R`) cannot take typed reasons because the system has no "prompt mode." The `decision_for_key` function takes an `Option<String>` but no UI path ever collects it.
- R3.2: No diff-specific navigation (`]` next hunk, `[` prev hunk).
- R3.3: No way to quickly jump between agents with pending reviews.

### Round 4 — Debugger (investigating failures)

**Scenario:** Agent failed, need to inspect logs, artifacts, task spec.

| Action | Expected | Actual | Result |
|--------|----------|--------|--------|
| `1` | Stream tab | Not supported | ❌ |
| `2` | Artifacts tab | Not supported | ❌ |
| `3` | Attempts tab | Not supported | ❌ |
| `4` | Task tab | Not supported | ❌ |
| `5` | Review tab | Not supported | ❌ |
| `I` on focused agent | View linked issue | Works | ✅ |
| `o` on issue detail | Set open | Works | ✅ |
| `w` on issue detail | Set in_progress | Works | ✅ |
| `b` on issue detail | Set blocked | Works | ✅ |
| `d` on issue detail | Set closed | Works | ✅ |
| `W` on issue detail | Work on issue | Works | ✅ |

**Pain points discovered:**
- R4.1: No direct tab access via number keys. Must cycle with ←/→ or use mouse.
- R4.2: `I` (uppercase i) for issue detail is easy to confuse with `i` (lowercase) for vim insert mode. In vim normal mode, `i` enters insert mode, but `I` opens issue detail. This is confusing.
- R4.3: Issue status keys (`o`/`w`/`b`/`d`) shadow vim normal mode commands and are only active in issue detail overlay. But there's no visual border/mask making this explicit.

### Round 5 — Accessibility / Error Recovery

**Scenario:** User made a mistake, wants to undo or recover.

| Action | Expected | Actual | Result |
|--------|----------|--------|--------|
| `u` | Undo last action | Not supported | ❌ |
| `Ctrl+r` | Redo | Not supported | ❌ |
| `Ctrl+z` | Suspend (terminal) | Not supported | ❌ |
| `?` | Help overlay | Works | ✅ |
| `Esc` from help | Close help | Works | ✅ |
| `Ctrl+c` | Quit | Works | ✅ |
| `q` | Quit | Not supported | ❌ |
| `:` | Command line | Not supported | ❌ |

**Pain points discovered:**
- R5.1: No `q` to quit (standard in TUIs like `htop`, `less`, `vim`). Only Ctrl-C works.
- R5.2: No command-line mode (`:`) for advanced commands.
- R5.3: Help overlay (`?`) is comprehensive but doesn't explain the input bar state machine.

---

## Visual Thinking: State Machine Analysis

### Current State Machine (simplified)

```
┌─────────────────────────────────────────────────────────────────────┐
│                          DASHBOARD                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌────────────┐ │
│  │ AgentsTree  │  │IssuesPanel  │  │ ActivityLog │  │ InputBar   │ │
│  │ (navigable) │  │ (navigable) │  │ (scrollable)│  │ (composable)│ │
│  └─────────────┘  └─────────────┘  └─────────────┘  └────────────┘ │
│                            │                                        │
│                    ┌───────┴───────┐                                │
│                    │ focused_panel │                                │
│                    │  Agents/Issues│                                │
│                    │     /Log      │                                │
│                    └───────┬───────┘                                │
│                            │                                        │
│         ┌──────────────────┼──────────────────┐                     │
│         ▼                  ▼                  ▼                     │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐               │
│  │ focused_node│   │issue_focus  │   │   (none)    │               │
│  │  = Some(id) │   │  = Loaded   │   │  browse mode│               │
│  └─────────────┘   └─────────────┘   └─────────────┘               │
│         │                  │                  │                     │
│         ▼                  ▼                  ▼                     │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐               │
│  │ DetailPane  │   │IssueDetail  │   │ panel nav   │               │
│  │  (tabs+scroll)│  │  (scroll)   │   │  j/k/g/G    │               │
│  └─────────────┘   └─────────────┘   └─────────────┘               │
│                                                                     │
│  KEY OWNER ARBITRATION (heuristic):                                │
│  ├─ Input empty + Vim Normal → View gets j/k/g/G/r/c/v/z/?/s      │
│  ├─ Input empty + Insert     → View gets j/k/g/G/r/v/?/s          │
│  ├─ Input has text           → Composer gets almost everything    │
│  ├─ Special cases: review tab → a/d/m/R go to view                │
│  └─ Global bypass: Ctrl+O, Ctrl+P, Ctrl+N, Alt+i                  │
└─────────────────────────────────────────────────────────────────────┘
```

**Problem:** The input bar is a permanent, active component that competes for keys. This creates the `key_owner()` heuristic, which is the source of all unpredictability.

### Proposed State Machine (modal)

```
┌─────────────────────────────────────────────────────────────────────┐
│                     DASHBOARD — NAVIGATION MODE                     │
│                                                                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌────────────┐ │
│  │ AgentsTree  │  │IssuesPanel  │  │ ActivityLog │  │ InputBar   │ │
│  │   ◄── active│  │   ◄── active│  │   ◄── active│  │  [grayed]  │ │
│  └─────────────┘  └─────────────┘  └─────────────┘  └────────────┘ │
│                                                                     │
│  Keys: j/k/h/l/↑/↓/←/→/g/G/Tab/Enter/Esc/1-5/q/z/r/v/Space/Ctrl+d  │
│                                                                     │
│  Press `i` or click input bar →  ┌─────────────────────────────┐   │
│                                  │   COMPOSE MODE              │   │
│                                  │   InputBar: [ACTIVE CYAN]   │   │
│                                  │   All keys → composer       │   │
│                                  │   Esc → Navigation Mode     │   │
│                                  └─────────────────────────────┘   │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  OVERLAY MODES (temporary, Esc always exits to nav)        │   │
│  │  ├─ focused_node → DetailPane overlay (tabs 1-5, scroll)   │   │
│  │  ├─ issue_focus  → IssueDetail overlay (status o/w/b/d)    │   │
│  │  └─ help_visible → Help overlay (? to toggle)              │   │
│  └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

**Benefit:** In Navigation Mode, there is NO key arbitration. Every key has exactly one meaning. The input bar is visually inactive (gray border, dimmed text). In Compose Mode, there is also no arbitration — all keys go to the composer. Esc exits compose mode.

---

## Findings Summary (Prioritized)

### P0 — Critical (Breaks Core Flow)

| ID | Finding | Impact |
|----|---------|--------|
| **KB-P0.1** | **No explicit input modality** — `key_owner()` heuristic is invisible and unreliable | Users cannot predict if `j` will navigate or type. Causes accidental navigation while typing and accidental typing while navigating. |
| **KB-P0.2** | **Esc behavior is dangerously overloaded** — closes help, unfocuses node, closes issue detail, or navigates back | In 4 different states, Esc does 4 different things. Users develop anxiety about pressing Esc. |
| **KB-P0.3** | **No way to exit input bar without clearing it** — if you've typed text and want to navigate, you must Backspace everything | Power users constantly clear text to navigate, then retype. Extremely frustrating. |

### P1 — High (Friction on Common Paths)

| ID | Finding | Impact |
|----|---------|--------|
| **KB-P1.1** | **No page-wise scrolling** — Space, Ctrl+D/U, PageUp/PageDown not supported in any panel | Scrolling through long logs or issue bodies is tedious (line-by-line only). |
| **KB-P1.2** | **No reverse Tab cycle** — `Shift+Tab` doesn't cycle panels in reverse | Users must cycle forward through all panels to go back one. |
| **KB-P1.3** | **No direct panel jumping** — Can't press `1`/`2`/`3` to jump directly to Agents/Issues/Log | Power users want direct access, not cyclic Tab. |
| **KB-P1.4** | **No direct tab jumping** — Can't press `1`/`2`/`3`/`4`/`5` for Stream/Artifacts/Attempts/Task/Review | Must cycle tabs with arrows. |
| **KB-P1.5** | **Review decisions can't take typed reasons** — `d`/`m`/`R` swallow the key with no prompt | Review workflow is incomplete; users must use mouse or slash commands. |
| **KB-P1.6** | **`I` (issue detail) conflicts with vim `i` (insert mode)** | In vim normal mode, `i` enters insert mode. `I` (uppercase) opens issue detail. Easy to hit wrong key. |

### P2 — Medium (Power User Friction)

| ID | Finding | Impact |
|----|---------|--------|
| **KB-P2.1** | **No `q` to quit** — Standard TUI quit key missing | Users expect `q` from `htop`, `less`, `vim`, `ranger`, etc. |
| **KB-P2.2** | **No vim count prefix** — `5j`, `10G` not supported | Power vim users expect count prefixes for all motion. |
| **KB-P2.3** | **No search within panels** — `/` opens command palette, not local search | Can't search/filter agents or issues. |
| **KB-P2.4** | **No `Shift+Space` for page up** — Common in pagers | Muscle memory from `less` doesn't work. |
| **KB-P2.5** | **Context hint is too subtle** — Single line of dark gray text at bottom | Users don't notice it. Should be more prominent or use a key legend bar. |

### P3 — Low (Polish)

| ID | Finding | Impact |
|----|---------|--------|
| **KB-P3.1** | **No `:` command mode** — Advanced commands require slash prefix | Vim users expect `:` for commands. |
| **KB-P3.2** | **No `u` / `Ctrl+r` undo/redo** — Input bar has no undo | tui-textarea supports this but it's disabled (`set_max_histories(0)`). |
| **KB-P3.3** | **Help overlay doesn't explain the state machine** — Help shows keys but not WHEN they apply | Users read help but still get surprised by context-dependent behavior. |

---

## Design Proposal: The Modal Dashboard

### Principle 1: Explicit Modes, No Heuristics

Replace `key_owner()` with an explicit `DashboardMode` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardMode {
    /// Navigation mode: all keys control panels, trees, logs.
    /// Input bar is visually inactive.
    Navigate,
    /// Compose mode: all keys go to the input bar.
    /// Enter submits, Esc returns to Navigate.
    Compose,
}
```

**State transitions:**
- `Navigate` → `Compose`: Press `i`, `a`, `A`, `I`, `o`, `O` (vim insert keys), or click input bar, or start typing any character when input bar is focused
- `Compose` → `Navigate`: Press `Esc` (always), or `Ctrl+[` (vim escape), or submit with `Enter`

### Principle 2: Unified Esc Behavior

In **Navigate mode**, `Esc` is a single, predictable action:
- If any overlay is active (issue detail, focused node, help), close the overlay
- If no overlay, `Esc` is a no-op (or shows a subtle "press q to quit" hint)

In **Compose mode**, `Esc` always exits to Navigate mode.

**This eliminates KB-P0.2 entirely.**

### Principle 3: No Key Arbitration in Navigate Mode

In Navigate mode, the input bar is **not a key competitor**. It is visually grayed out. All keys go directly to the active panel/overlay.

```rust
// In Navigate mode — NO key_owner() needed!
match key.code {
    KeyCode::Char('j') | KeyCode::Down => self.nav_down(),
    KeyCode::Char('k') | KeyCode::Up   => self.nav_up(),
    KeyCode::Char('g')                 => self.nav_top(),
    KeyCode::Char('G')                 => self.nav_bottom(),
    KeyCode::Char('i') | KeyCode::Char('a') | ... => {
        self.mode = DashboardMode::Compose;
        self.input_bar.enter_insert_mode();
    }
    // ... every key has exactly ONE meaning
}
```

### Principle 4: Visual Mode Indicators

| Mode | Input Bar Border | Status Bar | Context Hint |
|------|------------------|------------|--------------|
| Navigate | Gray (`Color::DarkGray`) | "NAV" badge | Panel-specific nav hints |
| Compose | Cyan (`Color::Cyan`) | "INSERT" badge | Edit hints (Ctrl+Enter submit) |

### Principle 5: Overlay Inheritance

Overlays (focused node detail, issue detail) temporarily **take over** the key space. `Esc` exits the overlay. No other keys are contextually rerouted.

```
Navigate mode
    ├─ No overlay     → panel keys (j/k/g/G/Tab/Enter)
    ├─ focused_node   → detail keys (j/k/←/→/1-5/g/G/Esc)
    ├─ issue_focus    → issue keys (j/k/o/w/b/d/W/Esc)
    └─ help_visible   → help keys (?/Esc)
```

---

## Implementation Plan

### Phase 1: Add `DashboardMode` and Visual Indicators
- Add `DashboardMode` enum to `DashboardView`
- Default to `Navigate`
- Style input bar border based on mode
- Add mode badge to status bar

### Phase 2: Replace `key_owner()` with Mode-Based Routing
- In `Navigate` mode: all non-modifier chars go to view
- In `Compose` mode: all keys go to input bar
- Remove `key_owner()`, `is_view_action_char()`, and the 48+ branch heuristic

### Phase 3: Add Missing Navigation Primitives
- PageUp/PageDown / Space / Shift+Space for page-wise scroll
- `Shift+Tab` for reverse panel cycle
- `1`/`2`/`3` for direct panel jumping
- `1`-`5` for direct detail tab jumping

### Phase 4: Add `q` Quit
- `q` in Navigate mode opens quit confirm dialog
- Aligns with standard TUI conventions

### Phase 5: Update Help Overlay
- Document Navigate vs Compose modes explicitly
- Show mode-specific key tables

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Breaking existing user muscle memory for input bar behavior | Keep `Enter` submit and `Esc` cancel in Compose mode identical to current behavior. Only change is explicit mode entry/exit. |
| Users accidentally entering Compose mode with `i` | Add a one-time tooltip: "Press Esc to return to navigation" on first Compose entry. |
| Emacs users confused by `i` entering Compose | Emacs mode users can click the input bar or press any character to enter Compose. `i` is only for vim users in Normal mode. |
| Mode state bugs (getting stuck in Compose) | `Esc` is an unconditional exit. Add a debug assertion that mode is always valid. |

---

## Conclusion

The keyboard binding crisis is a **symptom of implicit modality**, not a key-mapping problem. The `key_owner()` system is a clever workaround for a fundamental design flaw: the input bar should not compete for keys when the user is navigating.

By making **Navigate vs Compose explicit**, we eliminate the entire heuristic layer, make every key predictable, and open the door for power-user features (page scroll, direct jumps, search) that are currently impossible because they'd conflict with the input bar.

**Recommended priority:** Implement Phase 1+2 (modal system) immediately. This is a foundational fix that unblocks all other keyboard improvements.
