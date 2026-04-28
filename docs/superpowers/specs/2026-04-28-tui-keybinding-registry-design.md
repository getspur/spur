# TUI Keybinding Registry — design

**Status:** design draft, pending dual review (codex + gemini)
**Date:** 2026-04-28
**Owner:** Kevin Truong (kevin.truong.ds@gmail.com)
**Predecessors:**
- `docs/rca/2026-04-28-spur-tui-keybindings-mapping.md` — full reference map
- `docs/rca/2026-04-28-spur-tui-keybindings-ergonomic-review.md` — §11.1 (T2.5 brittle registry)
- Commit `bd5c4b5c` — F8 fix that proved the failure mode (the `o` bug)
- `2026-04-28-tui-leader-key-design.md` — already introduces `LEADER_BINDINGS` static table; this ADR generalizes that pattern

**Background:** `dashboard.rs:954-968` hard-codes `is_view_action_char`, a per-character whitelist used by `key_owner` to decide whether a key is a "view action" or should fall through to the composer. The recently-fixed F8 bug (vim-Normal `o` on focused node never reached the observe-toggle binding) was caused by ordering inside `key_owner`, but the deeper failure mode is the registry shape: every new view binding must remember to update `is_view_action_char` or it becomes unreachable in vim mode.

Codex: *"already produced unreachable bindings"*. Gemini: *"trying to patch a brittle, modeless heuristic rather than adopting a robust state machine"*.

---

## 1. Goal

Replace the hard-coded `is_view_action_char` predicate with a data-driven binding registry that is the **single source of truth** for what keys do in what contexts. After this work:

- Adding a new view binding is one-line: append a `ViewBinding { key, ctx, action }` to a static table.
- `is_view_action_char` is derived (`fn(ch) -> bool` becomes a query against the registry).
- Help-overlay copy, leader-menu entries, and `key_owner` all read from the same source.
- Future ports (per-view help, describe-key debug command, custom user keymaps) build on the registry rather than duplicating it.

## 2. Non-goals

- **No user-configurable keymap.** A `[ui.keymap]` config section is a future extension. The static registry stays compile-time.
- **No new key behavior.** Existing bindings keep working identically. This is a refactor.
- **No removal of vim/emacs sub-mode handling inside the input bar.** That's an editor-mode concern, separate from view-action routing.
- **No cross-platform key normalization.** The Mac Alt-key issue is documented in quick-fixes §4.7; the registry doesn't try to abstract platform.
- **No async / dynamic registry.** Bindings are `&'static [ViewBinding]`; views can request a per-view filtered slice but cannot mutate.

## 3. Background — what's wrong with `is_view_action_char` today

`dashboard.rs:954-968`:

```rust
fn is_view_action_char(&self, ch: char) -> bool {
    if self.focused_node.is_some() {
        if matches!(ch, 'h' | 'l' | 'o') { return true; }
        if self.detail_pane.current_tab == DetailTab::Review
            && matches!(ch, 'A' | 'D' | 'M' | 'R') { return true; }
    }
    matches!(ch, 'j' | 'k' | 'g' | 'G' | 'r' | 'v' | '?' | 's' | 'q' | 'z' | 'N' | 'P')
        || (ch == 'c' && self.focused_panel == Panel::Agents)
}
```

Failure modes:
1. **Detached from `handle_view_key`**: the actual handlers live in `handle_view_key` (`dashboard.rs:976-1395`). Adding a handler there does NOT add the char to `is_view_action_char`. Forgetting = unreachable in vim mode.
2. **Repeated `if focused_node` / `current_tab` predicates** inside the function: each predicate is a hand-written `match` on `ch`. Easy to mistype.
3. **No machine-readable structure**: the help overlay can't enumerate which keys are "view actions in vim mode + Stream tab"; it has to be hand-written separately.
4. **Cross-view duplication**: SessionDetail has its own routing logic; adding a binding there needs separate code paths.

## 4. Design

### 4.1 Core data structure

```rust
pub struct ViewBinding {
    pub key: BindingKey,
    pub action: Action,
    pub ctx: BindingContext,
    pub label: &'static str,           // for help / leader menu
    pub vim_normal_routing: VimRouting, // see §4.2
}

pub enum BindingKey {
    Char(char),
    CharWithMods(char, KeyModifiers),
    Special(KeyCode),                  // Tab, Esc, F-keys, etc.
}

pub struct BindingContext {
    pub view: ViewScope,                // Dashboard | SessionDetail | …
    pub panel: Option<PanelScope>,      // Agents | Detail | Log | None
    pub tab: Option<DetailTabScope>,    // Stream | Review | …
    pub requires_focused_node: bool,
    pub requires_input_bar_empty: bool,
    pub gate: Option<fn(&AppContext) -> bool>,
}

pub enum VimRouting {
    /// In vim Normal mode, this key wins over the compose-entry whitelist.
    /// (Today's `is_view_action_char == true` semantics.)
    ViewWins,
    /// In vim Normal mode, defer to compose-entry whitelist (i/a/A/I/o/O).
    /// View-action only triggers in non-vim contexts.
    ComposeWinsInVim,
    /// Always view-action regardless of vim state. Use sparingly.
    AlwaysView,
}
```

### 4.2 Static registry

```rust
const VIEW_BINDINGS: &[ViewBinding] = &[
    ViewBinding {
        key: BindingKey::Char('j'),
        action: Action::ScrollDown,
        ctx: BindingContext::dashboard_any_panel(),
        label: "scroll down",
        vim_normal_routing: VimRouting::ViewWins,
    },
    ViewBinding {
        key: BindingKey::Char('o'),
        action: Action::ToggleObserveCollapsed,
        ctx: BindingContext::dashboard_focused_node().with_tab(DetailTabScope::Stream),
        label: "toggle observe",
        vim_normal_routing: VimRouting::ViewWins,
    },
    // … all current bindings …
];
```

### 4.3 Derived `is_view_action_char`

```rust
impl ViewBinding {
    pub fn matches_ctx(&self, app: &AppContext) -> bool { /* … */ }
}

pub fn lookup_view_binding(
    key: KeyEvent,
    ctx: &AppContext,
) -> Option<&'static ViewBinding> {
    VIEW_BINDINGS
        .iter()
        .find(|b| b.matches_key(key) && b.matches_ctx(ctx)
                  && b.gate.map_or(true, |g| g(ctx)))
}

pub fn is_view_action_char(ch: char, app: &AppContext) -> bool {
    lookup_view_binding(
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        app,
    )
    .map(|b| matches!(b.vim_normal_routing, VimRouting::ViewWins | VimRouting::AlwaysView))
    .unwrap_or(false)
}
```

`is_view_action_char` is now a one-liner derived from the registry. Adding a new binding flips the predicate automatically.

### 4.4 `key_owner` simplification

Today `dashboard.rs:923-948` is a nested match with explicit vim/emacs branches. Post-refactor:

```rust
DashboardMode::Navigate => {
    if let Some(binding) = lookup_view_binding(key, &ctx) {
        match binding.vim_normal_routing {
            VimRouting::ViewWins => return KeyOwner::View,
            VimRouting::AlwaysView => return KeyOwner::View,
            VimRouting::ComposeWinsInVim if !is_vim_normal => return KeyOwner::View,
            _ => {} // fall through
        }
    }
    if is_vim_normal && is_vim_compose_entry_char(key.code) {
        return KeyOwner::Composer;
    }
    if !is_vim_normal && key.code.is_printable_char() {
        return KeyOwner::Composer;
    }
    KeyOwner::View
}
```

The `o`-bug class is structurally impossible: any new binding in the registry is automatically considered by `key_owner`.

### 4.5 Action dispatch in `handle_view_key`

```rust
fn handle_view_key(&mut self, key: KeyEvent, ...) -> Option<Action> {
    let ctx = self.context();
    if let Some(binding) = lookup_view_binding(key, &ctx) {
        return self.dispatch_view_binding(binding);
    }
    None
}

fn dispatch_view_binding(&mut self, b: &ViewBinding) -> Option<Action> {
    match b.action {
        Action::ScrollDown => { self.detail_pane.scroll_down(); Some(Action::ScrollDown) }
        Action::ToggleObserveCollapsed => { /* … */ None }
        // … one arm per action variant …
    }
}
```

The hand-written if-else cascade in today's `handle_view_key` (`:976-1395`, ~400 lines) shrinks to a single match on `Action` variant.

### 4.6 Cross-view sharing

SessionDetail, IssueBrowser, etc. each get their own `VIEW_BINDINGS` constant. The `BindingContext` carries a `view: ViewScope` so the registry naturally segments. App-level dispatch finds the right registry by view.

For truly global bindings (e.g. `Ctrl+K` palette, `?` help), a top-level `GLOBAL_BINDINGS` registry is consulted before per-view.

### 4.7 Help overlay + leader menu reuse

The help overlay (`?`) renders the active view's `VIEW_BINDINGS` filtered by `BindingContext.matches_ctx(current_app_state)`. Each binding's `label` is the help text — no separate help-string maintenance.

The leader-key menu (separate ADR) consumes the same registry filtered by `LEADER_BINDINGS` (a sub-registry that's specifically marked `via_leader: true`).

## 5. Migration plan

This is a structural refactor. Approach: **incremental per-view**, not a big-bang rewrite.

### Phase 1: introduce types + registry, keep old paths

1. Define `ViewBinding` / `BindingKey` / `BindingContext` / `VimRouting` (one new module: `crates/spur-tui/src/keybindings/registry.rs`).
2. Populate `VIEW_BINDINGS` for Dashboard (the highest-leverage view) — every existing binding, mirroring current behavior.
3. Add `lookup_view_binding` and `dispatch_view_binding` but **don't wire into `key_owner` yet**. The old paths still run.
4. Snapshot test: for every (key, context) in a generated test matrix, assert `lookup_view_binding(key, ctx).is_some()` iff `is_view_action_char_old(ch)` agrees with the routing decision.

### Phase 2: cut over Dashboard

5. Replace Dashboard's `is_view_action_char` with the registry-derived version.
6. Replace `handle_view_key`'s match cascade with `dispatch_view_binding`.
7. Run full test suite. The F8 regression test (`vim_normal_focused_node_o_toggles_observe_not_compose`) and the contract suite (19 tests) must pass unchanged.
8. Delete the old hand-written paths.

### Phase 3: cut over SessionDetail

9. Repeat phases 1-2 for SessionDetail.

### Phase 4: cut over remaining views

10. IssueBrowser, SessionPicker, PlanInspector, MermaidViewer.

Each view ships independently. Merging out of order is safe — registries don't interact.

## 6. Test plan

| Test name | Scenario | Asserts |
|---|---|---|
| `registry_covers_existing_bindings` | scan every key the old `is_view_action_char` returned true for | every key has a corresponding `ViewBinding` |
| `registry_o_bug_unreachable_under_new_routing` | vim Normal + focused_node + `o` with REGISTRY routing | `lookup_view_binding` returns ToggleObserveCollapsed; never enters Composer |
| `registry_review_tab_a_d_m_r_via_registry` | vim Normal + focused_node + Review tab + `A`/`D`/`M`/`R` | each routes to View; previous explicit Review-tab branch removed cleanly |
| `registry_no_unreachable_bindings_audit` | static analysis: every binding's vim_normal_routing != ComposeWinsInVim where the char ALSO appears in vim compose-entry whitelist | passes — no new structural ambiguity |
| `parity_with_pre_refactor_routing` | run a fuzz suite of (mode, key, ctx) triples; compare old `key_owner` vs new | identical KeyOwner decisions |

## 7. Rollout

Phase 1 + Phase 2 together in one PR (Dashboard cutover). Phase 3 + Phase 4 each their own PR.

Test gate: F8 contract suite (19 tests) + new parity-fuzz test must stay green throughout.

## 8. Open questions

1. **Const fn limitations**: `BindingContext::dashboard_focused_node()` builders need to be `const fn`. Rust 1.85+ supports most of what we need but trait-object closures (the `gate: Option<fn(...)>`) are fine; complex predicates may need manual struct literal init.
2. **Action variant explosion**: `Action` enum already has ~50 variants. The registry pattern will surface every new action as a registry entry, which is fine, but consider grouping (`Action::Scroll(ScrollDir)` instead of `ScrollUp`/`ScrollDown` separately).
3. **Modifier-bearing keys**: `BindingKey::CharWithMods` works, but does the registry want to enumerate ALL modifier combos (Ctrl+Alt+letter)? Recommend: explicit entries for each combo we actually use; reject unknowns.
4. **User-overridable keymap**: this ADR doesn't open that door. But if/when we want it, the static registry becomes a default that a user file overlays. Worth designing the API so this is forward-compatible.
5. **Per-binding doc strings**: should `ViewBinding.help: &'static str` be required (not just `label`)? Recommend yes — drives help overlay quality.
6. **Test coverage of the registry itself**: should we generate tests from the registry (e.g. each binding has a "this key in this context emits this action" smoke test)? Recommend yes — proc-macro or build.rs.

## 9. Method note

This is the lowest-priority of the three ADRs in terms of user-visible UX impact, but the highest-priority in terms of preventing future class bugs. The F8 regression test is concrete proof: a missing entry in `is_view_action_char` (vs the actual handler in `handle_view_key`) creates an unreachable binding that's invisible until a user tries it.

The registry pattern is borrowed from Helix's `keymap.rs` (which uses similar static binding tables) and from k9s's command registry. SPUR's variant is simpler — no nested key sequences (those live in the leader-key spec) — but follows the same "single source of truth" principle.
