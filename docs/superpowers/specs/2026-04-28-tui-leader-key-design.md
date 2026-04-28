# TUI Leader-Key Architecture — design

**Status:** design draft, pending dual review (codex + gemini)
**Date:** 2026-04-28
**Owner:** Kevin Truong (kevin.truong.ds@gmail.com)
**Predecessors:**
- `docs/rca/2026-04-28-spur-tui-keybindings-mapping.md` — full reference map
- `docs/rca/2026-04-28-spur-tui-keybindings-ergonomic-review.md` — §12.2 (gemini + codex both elevated leader-key from "deferred" to "near-term ADR")
- `2026-04-28-tui-keybinding-quick-fixes-design.md` — short-term fixes (parallel)

**Background:** the keybinding ergonomic review identified `Alt+*` namespace exhaustion as a P1 architectural concern. SessionDetail alone has 8 Alt-keys (`Alt+I/M/S/W/D/P/R/V`); macOS Terminal.app default treats Alt as Esc-prefix Meta and these don't register without user reconfiguration; tmux/Zellij intercept several `Ctrl+*` bypasses; legacy terminals can't reliably encode `Ctrl+digit`. Helix's `Space` leader → contextual-menu pattern sidesteps modifier exhaustion entirely, and Spacemacs/Doom popularized it among power users.

---

## 1. Goal

Introduce `Space` as the global leader key in Navigate / read-only contexts, opening a discoverable, contextual menu that progressively reveals available actions. After this work:

- Every action currently bound to `Alt+*` from Navigate has a non-modifier equivalent reachable via `Space {letter}`.
- Discoverability shifts from "memorize 8 Alt-keys per view" to "press Space, see what's available".
- New actions land on the leader namespace by default; raw modifier shortcuts become a power-user opt-in.
- Macs work out of the box without "Use Option as Meta" reconfiguration.

## 2. Non-goals

- **Don't remove `Alt+*` shortcuts.** They remain as power-user aliases. Users with established muscle memory keep their workflow.
- **Don't introduce a leader in Compose mode.** `Space` inside the input bar inserts a literal space — non-negotiable.
- **Don't introduce vim-style `<leader>` substitution syntax.** SPUR isn't a remappable editor; the leader is hard-coded `Space` until proven otherwise.
- **No nested leader chords beyond two keystrokes.** `Space {letter}` only. No `Space g w` (would-be "go workers"). Two-keystroke ceiling for now.
- **No timeout-based ambiguity.** If the user presses `Space` and the modal is open, the next keypress is consumed by the modal — there's no "hold for dwell" UX.

## 3. Background — `Alt+*` density today

Leader candidates from existing Navigate-mode bindings:

### Dashboard (`dashboard.rs`)

| Existing | Leader proposal |
|---|---|
| `Alt+I` toggle vim mode (`:1416`) | `Space i` |
| `?` help (`:1207`) | unchanged (already discoverable) |
| `s` request sessions (`:1208`) | unchanged (no Alt; just standardize) |
| (new) jump to issue browser | `Space b` (browser) — additional path, NOT a replacement for `2`. The dashboard `2` keybinding stays in place; relocating it is tracked separately as a number-key cleanup follow-up. |
| (new) jump to plan inspector (current session) | `Space p` |
| (new) toggle workers panel | `Space w` |

### SessionDetail (`session_detail.rs`)

| Existing | Leader proposal |
|---|---|
| `Alt+I` toggle vim mode (`:1211`) | `Space i` |
| `Alt+M` toggle plan mode (`:1185`) | `Space m` |
| `Alt+S` request sessions (`:1194`) | `Space s` |
| `Alt+W` inspect workers (`:1200`) | `Space w` |
| `Alt+D` toggle workers panel (`:1205`) | `Space d` |
| `Alt+P` plan inspector (`:1538`) | `Space p` |
| `Alt+R` open history picker (`:1461`) | `Space r` |
| `Alt+V` mermaid overlay (`:1432`) | `Space v` |

Same letter for the same operation across Dashboard and SessionDetail = consistent.

## 4. Design

### 4.1 Activation rule (Phase 1: Dashboard-only)

**Phase 1 scope** (this spec): leader is activated **only on Dashboard in Navigate mode**. SessionDetail is deferred to Phase 2 (§7) — it has no clean Navigate/Compose mode boundary today (the input bar is always-active when focused), and codex's spec review flagged the §4.1↔§5 contradiction the original spec carried. Resolving that contradiction safely requires a dedicated read-only/navigation state in SessionDetail, which is its own design decision.

**Phase 1 activation rule** — `Space` opens the leader menu when ALL of:
1. Active view is Dashboard.
2. Dashboard mode is `Navigate` (NOT Compose).
3. No modal overlay is open (quit-confirm, collision, upgrade, help, palette).
4. No completion / mention / history picker is active.
5. The `input_bar` does NOT have visual focus.

If any condition fails, `Space` falls through to its prior meaning (insert space in input bar / consumed by picker / etc.). Strict gating (gemini's spec review A1) — Space is a printable character; we MUST NOT swallow it during text composition under any circumstance.

**Phase 2** (separate follow-up): introduce a `SessionDetailMode { Navigate, Compose }` state, mirroring Dashboard's two-mode model, and gate Space-leader on `SessionDetailMode::Navigate`. Until that lands, SessionDetail users keep using `Alt+*` aliases.

### 4.2 Leader-mode UI

A small overlay anchored to the bottom of the active view, ~10 rows tall, shows:

```
┌─ Leader (press a key, Esc to cancel) ─────────────────┐
│  i  toggle vim mode             m  plan mode toggle  │
│  s  open sessions picker        w  inspect workers   │
│  p  plan inspector              d  toggle workers    │
│  r  history picker              v  mermaid overlay   │
│  b  issue browser                                    │
└──────────────────────────────────────────────────────┘
```

Per-view filtering — only entries valid in the current view show. SessionDetail without an active plan hides `p`. Dashboard hides `m`/`r`/`v` (those are SessionDetail-only).

### 4.3 Leader-mode key handling

After `Space`:
- A registered letter → resolve via `LeaderCommand::resolve(&mut App)` (see §4.5), fire the resulting `Action` if any, close menu.
- An unregistered letter → close menu silently. Key is consumed by the menu (NOT re-dispatched to the underlying view), so a stray letter doesn't accidentally trigger a view binding.
- `Esc` → close menu.
- Any modifier-bearing key (Ctrl/Alt/Shift+letter) → close menu; key is consumed (not re-dispatched). If the user wanted that modifier shortcut, they can press it again now that the menu is closed.
- `Space` again → close menu (toggle).

The "consume on close" rule (codex's spec review concrete amendment) avoids a class of double-dispatch bugs where closing the menu plus falling through could trigger two semantically related actions.

### 4.4 Implementation sketch

New module: `crates/spur-tui/src/components/leader.rs`.

```rust
pub struct LeaderMenu {
    is_open: bool,
    bindings: Vec<&'static LeaderBinding>,  // filtered per-view at open time
}

pub struct LeaderBinding {
    pub key: char,
    pub label: &'static str,
    pub command: LeaderCommand,
    pub visible_in: ViewScope,  // Dashboard | SessionDetail | Both
    pub gate: Option<fn(&AppContext) -> bool>,  // e.g. plan-exists check
}

impl LeaderMenu {
    pub fn open(&mut self, ctx: &AppContext) { /* filter bindings, set is_open */ }
    pub fn close(&mut self) { self.is_open = false; }
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> Option<Action> {
        // Look up letter; if registered, run command.resolve(app).
    }
    pub fn render(&self, frame: &mut Frame, area: Rect) { /* overlay */ }
}
```

Codex's spec review pointed out that several proposed bindings can't be expressed as a flat `Action` value: `Space p` (plan inspector) needs the current `SessionId`; `Space d` toggles `workers_panel_collapsed` which has no current `Action` variant; `Space v` (mermaid) needs the active render-picker selection. Use a resolver pattern instead of a concrete `Action`:

```rust
pub enum LeaderCommand {
    /// Resolves to a static Action. Used for context-free commands (e.g. ToggleVimMode).
    Static(Action),
    /// Resolves dynamically against the current app state. Used for commands that
    /// need session_id, current selection, etc.
    Resolved(fn(&mut App) -> Option<Action>),
    /// View-local mutation that doesn't emit an Action (e.g. toggle a panel flag).
    Mutate(fn(&mut App)),
}

impl LeaderCommand {
    pub fn dispatch(&self, app: &mut App) -> Option<Action> {
        match self {
            LeaderCommand::Static(a) => Some(a.clone()),
            LeaderCommand::Resolved(f) => f(app),
            LeaderCommand::Mutate(f) => { f(app); None }
        }
    }
}
```

This lets the registry encode all three classes uniformly:

```rust
const LEADER_BINDINGS: &[LeaderBinding] = &[
    LeaderBinding {
        key: 'i',
        label: "toggle vim mode",
        command: LeaderCommand::Static(Action::ToggleVimMode),
        visible_in: ViewScope::Both,
        gate: None,
    },
    LeaderBinding {
        key: 'p',
        label: "plan inspector",
        command: LeaderCommand::Resolved(|app| {
            app.current_session_id().map(|sid| Action::NavigateTo(ViewId::PlanInspector(sid)))
        }),
        visible_in: ViewScope::SessionDetail,
        gate: Some(|ctx| ctx.has_plan()),
    },
    LeaderBinding {
        key: 'd',
        label: "toggle workers panel",
        command: LeaderCommand::Mutate(|app| {
            app.session_detail.workers_panel_collapsed ^= true;
        }),
        visible_in: ViewScope::SessionDetail,
        gate: None,
    },
    // ...
];
```

Wire into `app.rs` key dispatch:

```rust
// Before the existing ViewId dispatch (around app.rs:996)
if leader_menu.is_open() {
    if let Some(action) = leader_menu.handle_key(key) {
        return Some(action);
    }
    return None; // menu consumed the key (e.g. Esc / unrecognized letter)
}

if key.code == KeyCode::Char(' ') && self.can_open_leader() {
    leader_menu.open(&self.context_for_leader());
    return None;
}
```

`self.can_open_leader()` enforces the §4.1 activation rule.

### 4.5 Binding registry

The registry is declared inline alongside the resolver patterns shown in §4.4. Tab-ordering in the popup is alphabetical within scope.

This sets the precedent for the broader `is_view_action_char` registry refactor (separate ADR `2026-04-28-tui-keybinding-registry-design.md`). When that ADR ships, the `LeaderBinding` table can be folded into the unified `ViewBinding` registry with `via_leader: true` flagging — single source of truth.

### 4.6 Help overlay integration

The help overlay (`?`) appends a "Leader (Space)" section listing all bindings. Single source of truth: `LEADER_BINDINGS`.

### 4.7 Contextual popup with 250ms reveal delay (gemini A3)

A leader key architecture fails if users cannot discover the follow-up keys. Helix solves this via a transient menu — pressing `<leader>` opens a contextual hint that reveals available keystrokes after a brief delay.

**Behavior:**
- T0: User presses Space. Leader is "armed".
- T0+0ms: If a registered letter is pressed BEFORE 250ms elapses, dispatch immediately (zero-flicker for power users who know the binding).
- T0+250ms: If no letter has been pressed, render the contextual popup (the box at §4.2). User now sees options.
- After popup: any registered letter dispatches; Esc/Space closes; unregistered letter consumes-and-closes per §4.3.

The 250ms threshold is calibrated for "intentional pause to look at options" — fast enough that confident users never see the menu, slow enough that searching users do.

### 4.8 `Alt+*` deprecation horizon (gemini A2 — compromise)

Permanent dual-track is debt; instant removal breaks existing user muscle memory. Compromise:

- **Release N (this spec ships)**: leader-key + Alt+* both work. Help overlay primary-paths leader. No deprecation toast yet.
- **Release N+1 (one minor release later)**: pressing any `Alt+*` leader-mapped shortcut shows a one-shot deprecation toast: `"Alt+S deprecated. Use Space, then s."` Action still dispatches.
- **Release N+2 (two minor releases out)**: `Alt+*` removed entirely from leader-mapped bindings. Removal is data-driven if the registry refactor (Spec 4) lands first; otherwise hard-code the removal.

This compromise (between codex's "permanent" and gemini's "1 release") gives existing users a full development cycle to retrain while keeping the cleanup horizon firm.

**Out-of-scope for sunset**: `Alt+I` (toggle vim mode) is too useful as a globally-accessible shortcut and stays as a permanent alias even post-sunset for the leader-mapped subset. The registry can mark bindings `keep_modifier_alias: true` to opt-out.

## 5. Activation in input-bar context — strict gating (gemini A1)

In Dashboard Compose mode, in SessionDetail with input-bar focus, in any picker, in any history-shell — `Space` is always a literal space character or consumed by the active component. The cost of breaking text input outweighs the convenience of leader-from-typing.

This gating is enforced **structurally, not advisorily**: the activation rule (§4.1) returns false in any of these states, and the dispatch order in `app.rs` checks `can_open_leader()` BEFORE attempting to capture Space.

If a power user wants leader from Compose, they can press `Esc` first (returning to Navigate), then `Space`.

## 6. Test plan

| Test name | Scenario | Asserts |
|---|---|---|
| `leader_opens_in_navigate_mode` | Dashboard Navigate + Space | menu opens; `is_open == true` |
| `leader_does_not_open_in_compose` | Dashboard Compose + Space | space inserted into input bar |
| `leader_does_not_open_with_overlay` | Help overlay + Space | menu does not open; help still visible |
| `leader_closes_on_esc` | Open menu + Esc | menu closes; no action |
| `leader_dispatches_action` | Open menu + `s` | `Action::RequestSessions` emitted; menu closes |
| `leader_unrecognized_letter_closes` | Open menu + `z` (no binding) | menu closes silently; `z` does NOT fall through |
| `leader_filters_per_view` | SessionDetail without plan + open menu | `p` (plan inspector) absent from list |
| `leader_help_overlay_lists_all_bindings` | open `?` | leader section enumerates the registry |
| `alt_aliases_still_work` | Press `Alt+M` in SessionDetail | same effect as `Space m` |

## 7. Rollout & cross-spec coordination

### 7.1 Spec ordering

This spec ships in **Release N+1** — AFTER `2026-04-28-tui-keybinding-quick-fixes-design.md` + `2026-04-28-tui-destructive-undo-design.md` ship together in Release N. The leader popup needs:
- Panic-Esc (quick-fixes T2.9) and Esc-cancel-stream-hint (quick-fixes T1.10) hooks to clear the leader-menu cleanly under all interrupt conditions.
- Tombstone hint-slot priority (destructive-undo §4) to coexist with leader popup positioning without overlap.

### 7.2 Phase 1 (this ADR)

Leader scaffolding + 9 Dashboard-applicable bindings. Tab order in menu: alphabetical within scope. Help overlay updated. All Alt+* aliases remain functional. **SessionDetail is NOT scoped here** — see §7.4.

### 7.3 Sunset path for `Alt+*` aliases

Per §4.8: deprecation toast in Release N+2; removal in Release N+3. Subset-scoped — `Alt+I` (vim toggle) keeps its alias permanently.

### 7.4 Phase 2 — SessionDetail leader

Conditional on introducing a `SessionDetailMode { Navigate, Compose }` enum (mirroring Dashboard). Until that lands, SessionDetail users keep using Alt+* aliases (which won't be deprecated for SessionDetail-only bindings until Phase 2 ships).

### 7.5 Phase 3 — user-configurable keymap

A `[tui.leader]` config section in `.spur/config.toml` allowing user-defined bindings. Out of scope for this ADR. (Note: `[tui.*]` namespace per SPUR convention, NOT `[ui.*]` — codex's spec review caught this).

### 7.6 Hint-slot priority (cross-spec)

Per quick-fixes §6.3, hint-rendering precedence (highest first):
1. Panic-Esc reset confirmation
2. Tombstone toast
3. Leader-menu inline preview ← **this spec**
4. Esc-cancel-stream hint
5. General status

The leader popup itself uses a dedicated overlay area (the box at §4.2), NOT the single-line hint slot. Slot-row entries above are for the inline keystroke-discovery hint that may appear when the popup is closed (e.g. "Press Space for actions" footer).

## 8. Open questions

1. **Leader key conflict with text editing**: Compose mode passes Space through. What about a "type-to-search" UI where Space is a navigation shortcut (e.g. PgDown in less)? SPUR doesn't have those today. Defer.
2. **Two-keystroke ceiling**: do we want `Space g w` for "go workers"? Probably not — Helix's `g` motion uses second-keystroke namespacing already; mixing would be confusing. Hard cap at two.
3. **Modifier-bearing leader keys**: `Space Shift+I` to capitalize? Reject. Leader is letter-only. Capitals can be separate bindings if needed.
4. **Visual feedback delay**: should the menu open instantly or after 100ms (less flicker for accidental Space)? Recommend instant; users learn quickly.
5. **Sticky footer hint**: when menu is closed, show a fading hint "Press Space for actions" once per session? Recommend yes — discoverability lever.

## 9. Method note

This spec is one of three architectural ADRs derived from the 2026-04-28 keybinding ergonomic review. It addresses the §12.2 disagreement where gemini and codex both pulled "leader-key" forward from "deferred" to near-term — gemini called the dense `Alt+*` namespace "mandatory architecture required to unblock all future bindings cleanly".

Helix's `Space`-leader contextual menu and Zellij's mode-bar were the design references. SPUR adopts Helix's pattern (overlay menu) rather than Zellij's (persistent mode-bar) because SPUR's status bar is already crowded with model/effort/usage data (M10 work stream).
