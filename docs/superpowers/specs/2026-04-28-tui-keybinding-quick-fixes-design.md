# TUI Keybinding Quick Fixes — design

**Status:** design draft, pending dual review (codex + gemini)
**Date:** 2026-04-28
**Owner:** Kevin Truong (kevin.truong.ds@gmail.com)
**Predecessors:**
- `docs/rca/2026-04-28-spur-tui-keybindings-mapping.md` — full reference map
- `docs/rca/2026-04-28-spur-tui-keybindings-ergonomic-review.md` — first-principles + industry review with cross-checks
- Commit `bd5c4b5c` — F8 fix for vim-Normal `o` on focused node

**Background:** the ergonomic review identified 7 ship-now items + 5 small new findings (T1.9, T1.10, T2.7–T2.11). All are surgical, all touch existing key routing, none require architectural change. This spec bundles them into one wave to amortize review/test/rollout cost. Cross-cutting ADRs (leader-key, destructive-undo, registry refactor) are separate specs.

---

## 1. Goal

Close the cross-view consistency gaps and dead-code keybindings that surfaced from the ergonomic review, without touching the routing architecture. After this work:

- Every documented hint corresponds to a working handler.
- No global bypass in `key_owner` lands on a no-op (no silent ignores).
- `g/G` semantics never depend on terminal width.
- `Tab` / `Shift+Tab` cycle reliably across views with explicit composer/picker guards.
- `:` opens the command palette as a vim-aligned alias.
- Keyboard help / status hints surface terminal-emulator caveats once.

## 2. Non-goals

- **No `q` rebind.** `q` policy stays as it is today: subviews use `q`/Esc to close (or absent), Dashboard `q` quits. The `q`-via-quit-confirm refinement is folded into a separate dashboard-quit-confirm followup. Codex's review (`§12.2` of the ergonomic review) showed dashboard-style TUIs (gh-dash, Yazi, nnn) consistently use contextual `q`; a universal rebind would be an overshoot.
- **No leader-key (`Space`) introduction.** Tracked in `2026-04-28-tui-leader-key-design.md`.
- **No `is_view_action_char` registry refactor.** Tracked in `2026-04-28-tui-keybinding-registry-design.md`.
- **No destructive-action undo / confirm dialogs.** Tracked in `2026-04-28-tui-destructive-undo-design.md`.
- **No universal `/` filter rollout.** Filter state shape across PlanInspector / IssueBrowser / activity_log is a follow-up.
- **No vim feature parity work** (counted motions, ex-mode, find-char, etc.). Out of scope.

## 3. Background — concrete ground

| ID | Finding | File / line | Type |
|---|---|---|---|
| Q1 | Dashboard `Ctrl+O` claimed in `key_owner` global bypass at `:898-904`, but `handle_view_key` `o` arm at `:1038-1050` excludes Ctrl/Alt → silent no-op. | `crates/spur-tui/src/views/dashboard.rs:898, 1038` | dead bypass |
| Q2 | SessionPicker error-state footer hint `r retry · Esc back` at `:29` displays without handler. `Loading \| Error` arm at `:1483-1485` only handles `Esc`. | `crates/spur-tui/src/views/session_picker.rs:29, 1483` | hint mismatch |
| Q3 | SessionPicker `d` → `ToggleSessionArchive` at `:1447-1449` violates universal `d`-means-delete. | `crates/spur-tui/src/views/session_picker.rs:1447` | semantic clash |
| Q4 | PlanInspector `g/G` at `:133-134` jump first/last in CURRENT LANE in wide mode, GLOBALLY in stacked mode. Width-conditional. | `crates/spur-tui/src/views/plan_inspector.rs:133` | semantic drift |
| Q5 | SessionDetail has no Tab/Shift+Tab panel cycle. Dashboard does (`:1366-1390`). Inconsistent. | `crates/spur-tui/src/views/session_detail.rs` | missing binding |
| Q6 | No `:` alias for `Ctrl+K` command palette. Vim users must memorize Ctrl+K. | `crates/spur-tui/src/app.rs:980` | missing alias |
| Q7 | `Alt+*` shortcuts (8 in session_detail) require macOS Terminal.app "Use Option as Meta" + may collide with Ctrl+digit / Ctrl+Q-S flow control / kitty CSI-u. Help overlay says "Alt+M" with no platform translation. | `crates/spur-tui/src/views/session_detail.rs:1185-1211`, `crates/spur-tui/src/app.rs` (help overlay) | docs gap |
| T1.9 | Dashboard `Tab`: empty input → cycle example prompts; non-empty → cycle Agents/Log. Surprise. | `crates/spur-tui/src/views/dashboard.rs:1366-1390` | overload |
| T1.10 | SessionDetail `Esc` cancels in-flight stream BEFORE NavigateBack at `:1167`. User pressing Esc as "back" loses an active turn. | `crates/spur-tui/src/views/session_detail.rs:1167` | hidden destructive |
| T2.7 | IssueBrowser `d` → status=closed at `:165` reuses the same `d`-means-delete antipattern as Q3. Cross-view. | `crates/spur-tui/src/views/issue_browser.rs:165` | semantic clash |
| T2.9 | No "panic Esc" hatch — triple-Esc should reliably return to root. Currently context-dependent. | app-wide | missing escape |
| T2.10 | Hint copy says "Alt+M" / "Ctrl+P" with no platform translation (`⌥`/`⌃` on Mac). | hint rendering | docs/cosmetic |

T2.8 (`Ctrl+1..5` legacy-terminal encoding) and T2.11 (mouse ergonomics) are deferred — they're scope-broader than this wave.

## 4. Design

### 4.1 Q1 — Dashboard `Ctrl+O`: wire as observe-toggle alias

Two options considered:
- **A. Wire**: when `focused_node.is_some()`, treat `Ctrl+O` identically to plain `o` on Stream tab (toggle observe-collapsed).
- **B. Strip**: remove `Ctrl+O` from the `key_owner` bypass list at `:898-904` so it falls through to no-op (no router claim).

**Choose A.** Symmetric with SessionDetail's `Ctrl+O` at `:1404` (which already toggles Observe-entries collapse). Cross-view consistency win. SessionDetail's `Ctrl+O` is preserved untouched.

**Implementation**: in `handle_view_key`, add an arm before the existing `'o'` arm:

```rust
KeyCode::Char('o')
    if self.focused_node.is_some()
        && key.modifiers == KeyModifiers::CONTROL =>
{
    // Alias for plain `o` on focused node — symmetric with
    // SessionDetail's Ctrl+O. Allows toggling observe from any
    // detail tab without the Stream-tab predicate.
    if let Some(ref id) = self.focused_node.clone() {
        if let Some(trace) = worker_streams.get_mut(&id.0) {
            trace.toggle_observe_collapsed();
        }
    }
    None
}
```

**Test**: extend `vim_normal_focused_node_o_toggles_observe_not_compose` (added in F8 commit `bd5c4b5c`) with a sibling test asserting `Ctrl+O` produces the same effect.

### 4.2 Q2 — SessionPicker error-state retry handler

Add `r` and `Enter` handlers to the `Error` arm. `PickerState::Error` is a struct variant in current code, so the match needs to bind it correctly (codex's spec review):

```rust
PickerState::Loading => {
    if key.code == KeyCode::Esc {
        return Some(Action::NavigateTo(ViewId::Dashboard));
    }
    None
}
PickerState::Error { .. } => {
    match key.code {
        KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
        KeyCode::Char('r') | KeyCode::Enter => Some(Action::RefreshSessions),
        _ => None,
    }
}
```

**Test**: new `tests/session_picker_error_retry.rs` integration test.

### 4.3 Q3 + T2.7 — `d` ↔ `x` migration with cross-view policy

Both SessionPicker (`:1447`) and IssueBrowser (`:165`) violate the universal `d`-means-delete. Codex's review (§12.2) recommended a transitional alias rather than hard ship.

**Migration plan (single PR per surface):**
1. Add `x` as new primary binding.
   - SessionPicker `x` → `ToggleSessionArchive`.
   - IssueBrowser `x` → status=closed.
2. Keep `d` for one release as a deprecation alias. Emit a one-shot toast on first press: `"d → archive renamed to x; d will be removed in N releases"`.
3. Update footer / hint copy to say `x archive` (SessionPicker) and `x close` (IssueBrowser).
4. After two releases, drop `d` entirely OR remap `d` to "delete-permanently" (with confirm dialog — depends on destructive-undo ADR outcome).

The destructive-undo spec (`2026-04-28-tui-destructive-undo-design.md`) covers the post-deprecation `d`-as-delete decision.

**Test**: each surface's test file gets `x` parity assertions; existing `d` tests become deprecation-toast assertions.

### 4.4 Q4 — PlanInspector `g/G` regression test (corrected scope)

**Codex's spec review (2026-04-28) corrected this finding.** The mapping RCA at §6 and the actual code at `plan_inspector.rs:131-134` agree: `g`/`G` ALWAYS call `jump_lane_start/end` — lane-local, not width-conditional. The width condition lives in `move_task` at `:88-92`, which is `j`/`k`'s code path. The original ergonomic-review claim that `g/G` drift on width was wrong; what actually drifts is `j/k`.

**Fix scope for this spec:**
1. **Add a regression test pinning current `g/G` lane-local behavior across both render widths.** The test shouldn't have to change behavior; it just locks the contract so a future refactor doesn't accidentally introduce drift.
2. **Open a SEPARATE follow-up item** for `j/k` width-conditional movement. That deserves its own decision: keep stacked-mode global navigation (current behavior), or make stacked-mode also lane-local for symmetry with `g/G`. Not resolved here.

**Test**: `tests/plan_inspector_g_consistent_across_widths.rs` exercises render at width 50 and width 120; presses `g`, asserts cursor lands at the first task of the current stage in both. Then `G` → asserts last task of current stage. The test should pass on current code unchanged.

**No source code changes in this spec.** The earlier "drop the `area.width < 90` branch" instruction was based on a stale finding propagated from the ergonomic review — applying it would actually regress `j/k`. Removed.

### 4.5 Q5 — SessionDetail Tab/Shift+Tab panel cycle

Add Tab/Shift+Tab cycling to SessionDetail panels (matching Dashboard's behavior at `dashboard.rs:1366-1390`).

**Critical guards** (codex's TWEAK condition, §12.2; SessionDetail does NOT have explicit Navigate/Compose modes like Dashboard, so the guards are stated in terms of input-bar state, not mode):
1. **Picker ownership**: if `completion.is_active()`, picker gets Tab unconditionally.
2. **History-shell ownership**: if a history picker is open (`Ctrl+R` / `Alt+R` engaged), history-shell gets Tab.
3. **Composer ownership**: if input bar is non-empty (i.e. user is composing), Tab/BackTab MUST be sent to `input_bar` (e.g. for completion accept). Panel cycling only applies when input is empty.
4. Only when all three above release Tab → SessionDetail consumes it for panel cycle.

**Implementation**: extend `session_detail.rs` `handle_key` so the picker/composer guards run BEFORE the panel-cycle handler. This is identical to Dashboard's existing precedence; the bug is just that the panel-cycle handler doesn't exist yet.

**Test**: integration test asserting:
- Tab in SessionDetail with input empty + no picker → cycles panel focus.
- Tab in SessionDetail with input non-empty → goes to composer (input_bar handles it).
- Tab in SessionDetail with picker open → picker accepts.

### 4.6 Q6 — `:` alias for command palette

Vim and k9s use `:` for command mode. SPUR uses `Ctrl+K` (VS Code). Adding `:` as alias is zero-cost where the input bar isn't active.

**Critical guard**: do NOT steal literal `:` from the input bar. If the user is in Compose mode (input bar active), `:` MUST be inserted as a character.

**Implementation**: in `app.rs` global hotkey dispatch — add `:` alongside `Ctrl+K` only when `dashboard.mode() == DashboardMode::Navigate` AND no overlay is open. SessionDetail input bar is always active when focused, so `:` falls through to input_bar in that view (which is correct — typing `:` in a chat is normal). This means `:` opens palette ONLY from Dashboard Navigate mode, not session_detail. Acceptable: Dashboard is where most palette usage happens.

**Alternative considered**: also support `:` from SessionDetail when input bar is empty. Rejected — empty-buffer detection is a brittle heuristic (the F8 work already showed how that gets hairy). Keep the rule simple: Navigate mode + no overlay = `:` opens palette. Otherwise, `:` is a regular character.

**Test**: `tests/palette_colon_alias.rs` covering Navigate-mode open vs Compose-mode passthrough.

### 4.7 Q7 + T2.10 — Help overlay terminal-caveat copy + OS-aware hint rendering

Two layers:

#### 4.7.1 Help overlay — single docs section

Add to the help overlay (rendered when `?` is pressed) a "Keyboard environment" section listing:

- macOS Terminal.app: enable "View → Use Option as Meta key" or `Alt+*` shortcuts will not register.
- iTerm2: Profiles → Keys → set Left Option to "Esc+".
- Windows Terminal: native — works.
- tmux passthrough: `Ctrl+P/N/O` may be intercepted by your tmux prefix; consider rebinding tmux prefix to `Ctrl+A`.
- Legacy terminals: `Ctrl+digit` (e.g. `Ctrl+1` for tab jump) may not encode reliably; use `1..5` plain when no input bar is active (post-leader-key ADR).
- Flow control: terminals running `stty ixon` will eat `Ctrl+S`/`Ctrl+Q`. Set `stty -ixon` or use a different shortcut.

Source: `docs/superpowers/specs/2026-04-28-tui-keybinding-quick-fixes-design.md` §4.7.1 (this section).

#### 4.7.2 OS-aware modifier glyphs

Currently hint copy literals: "Alt+M", "Ctrl+P", "Shift+Tab". On macOS, users see those keys labeled `⌥`, `⌃`, `⇧`.

**Implementation**: introduce a helper in `crates/spur-tui/src/components/keyhint.rs` (new file). Codex's spec review noted that the previous `fmt_modifier` returning a single `&'static str` cannot represent ordered modifier combos like `Ctrl+Shift+P` or `Shift+Tab`. The helper takes a full `KeyEvent`-like input and returns an owned `String`:

```rust
use crossterm::event::{KeyCode, KeyModifiers};

pub fn format_key_hint(code: KeyCode, mods: KeyModifiers) -> String {
    let mut out = String::new();
    let (ctrl, alt, shift) = platform_modifier_glyphs();

    // Order: Ctrl, Alt, Shift, key — matches macOS (⌃⌥⇧) and Windows/Linux conventions.
    if mods.contains(KeyModifiers::CONTROL) { out.push_str(ctrl); out.push('+'); }
    if mods.contains(KeyModifiers::ALT)     { out.push_str(alt);  out.push('+'); }
    if mods.contains(KeyModifiers::SHIFT)   { out.push_str(shift); out.push('+'); }
    out.push_str(&format_keycode(code));
    out
}

#[cfg(target_os = "macos")]
fn platform_modifier_glyphs() -> (&'static str, &'static str, &'static str) {
    ("⌃", "⌥", "⇧")
}
#[cfg(not(target_os = "macos"))]
fn platform_modifier_glyphs() -> (&'static str, &'static str, &'static str) {
    ("Ctrl", "Alt", "Shift")
}

fn format_keycode(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "Tab".into(),  // Shift+Tab carries Shift modifier separately
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        // ... PageUp/Down, Home/End, F-keys
        other => format!("{:?}", other),
    }
}
```

Migrate hint-rendering call sites incrementally (low priority; help-overlay caveat copy comes first).

**Test**: snapshot tests for hint render on macOS vs Linux targets (cfg-gated).

### 4.8 T1.9 — Dashboard Tab overload

Currently `dashboard.rs:1366-1390`:
- Tab + input empty → `cycle_example()` (cycle example prompts in input bar)
- Tab + input non-empty → cycle Agents↔Log

This is the canonical "modifier-free key with two unrelated meanings" pattern that breaks muscle memory.

**Fix**: separate the two semantics.
- Tab → ALWAYS cycle Agents↔Log panel focus (the "navigation" meaning, consistent with Dashboard's panel model).
- Move example-prompt cycling to a dedicated key. Proposal: `Ctrl+E` (Example) when input bar is empty. Avoids vim/emacs collisions (`Ctrl+E` in vim Normal is "scroll viewport down 1 row" — unused in Dashboard Navigate).

**Migration**: deprecation toast on Tab-with-empty-input for one release: `"Tab now cycles panels; press Ctrl+E to cycle examples"`.

**Test**: `tests/dashboard_tab_panel_cycle_unconditional.rs`.

### 4.9 T1.10 — SessionDetail Esc-cancels-stream visibility

The behavior at `session_detail.rs:1167` is correct — Esc SHOULD cancel an in-flight stream. The bug is users don't see this happening.

**Fix**: when `cancel_in_flight = true` (just before `Action::CancelStream` is emitted), set a one-shot status hint: `"Esc cancelled the active turn. Press Esc again to go back."`

This makes the destructive aspect of Esc visible. Second Esc proceeds with NavigateBack (existing path).

**No code change to the cancellation logic itself.** Just hint surfacing.

**Test**: integration test asserts hint text appears post-cancel and clears on next render after second Esc.

### 4.10 T2.9 — Panic Esc hatch (MANDATORY)

**Elevated to mandatory implementation** (gemini's spec review A1). Because the brain settled on contextual `q` per codex (rather than universal back-rebind), this hatch is the user's only guaranteed escape from any nested state. Without it, a user buried in a SessionDetail Compose-mode picker inside a quit-confirm modal has no single key that returns them to a clean root. Panic Esc closes that gap.

Triple-Esc within 1000ms unconditionally returns to Dashboard root, dismissing all overlays / pickers / focus / compose mode AND clearing any pending destructive-action tombstone (per cross-spec coordination, §6 below).

**Implementation**: in `app.rs`, track `esc_chain: Vec<Instant>` (capped at 3). On `Esc` keystroke, push current `Instant`, prune entries older than 1000ms. If `esc_chain.len() == 3` → emit a special action `Action::PanicReset` that:

1. Clears every modal flag (`quit_confirm_visible`, `collision_modal`, `upgrade_modal`, `help_visible`, `palette_visible`).
2. Closes any open completion / mention / history pickers.
3. Cancels any in-flight tombstone client-queue **without dispatching**. (Coordinates with destructive-undo spec §4.)
4. Forces `ViewId::Dashboard`, `focused_node = None`, `focused_panel = Agents`, `mode = Navigate`.
5. Clears `esc_chain`.

The 1000ms window (gemini's recommendation) is more forgiving than 500ms — accommodates users who pause briefly between Esc presses without losing the chain.

**Test**: `tests/app_panic_esc_resets_to_root.rs` simulating layered state (overlay + picker + compose + tombstone) + triple Esc → asserts root state and that no destructive dispatch happened.

## 5. Cross-cutting test plan

| Test name | Scenario | Asserts |
|---|---|---|
| `dashboard_ctrl_o_toggles_observe` | Q1 | `Ctrl+O` on focused node = same effect as plain `o` |
| `session_picker_error_retry_handles_r_and_enter` | Q2 | `r` and `Enter` in Error state emit `RefreshSessions` |
| `session_picker_x_archives` | Q3 | `x` triggers archive; `d` triggers archive + emits deprecation toast |
| `issue_browser_x_closes` | T2.7 | parallel to above |
| `plan_inspector_g_consistent_across_widths` | Q4 | g/G land at first/last of current stage at width 50 AND width 120 (test passes on current code; pins behavior) |
| `session_detail_tab_panel_cycle_with_guards` | Q5 | Tab cycles panels iff composer/picker/history-shell don't claim |
| `palette_colon_alias_navigate_only` | Q6 | `:` opens palette in Dashboard Navigate; passes through in Compose |
| `dashboard_tab_unconditional_panel_cycle` | T1.9 | Tab cycles panels even with empty buffer; `Ctrl+E` cycles examples |
| `session_detail_esc_shows_cancel_hint` | T1.10 | Hint appears after first-Esc cancel |
| `app_panic_esc_resets_to_root` | T2.9 | Triple-Esc within 1000ms returns to Dashboard root, cancels any pending tombstone |
| `os_aware_modifier_glyph` (cfg-gated) | T2.10 | macOS render uses `⌥`; Linux uses `Alt` |

## 6. Rollout & cross-spec coordination

### 6.1 Commit boundaries (within this spec)

1. `fix(tui): wire Ctrl+O on dashboard as observe-toggle alias`
2. `fix(tui): SessionPicker error-state r/Enter retry handler`
3. `feat(tui): x as primary archive key; d deprecation toast (SessionPicker + IssueBrowser)`
4. `test(tui): PlanInspector g/G regression test pinning lane-local behavior`
5. `feat(tui): SessionDetail Tab/Shift+Tab panel cycle with composer/picker guards`
6. `feat(tui): : as command-palette alias from Dashboard Navigate`
7. `docs(tui): help overlay keyboard-environment caveats`
8. `feat(tui): format_key_hint helper (Mac → ⌥/⌃/⇧, ordered modifier combos)`
9. `feat(tui): Dashboard Tab unconditional panel cycle; Ctrl+E for examples`
10. `feat(tui): SessionDetail Esc cancel-hint`
11. `feat(tui): triple-Esc panic reset to Dashboard root`

Each commit has its own regression test. Migration toasts (3, 9) auto-expire after one minor release.

### 6.2 Cross-spec rollout order (gemini's spec review)

This spec ships **in the same release** as `2026-04-28-tui-destructive-undo-design.md` (the tombstone-undo spec). Reasoning: commit 3 (`d`→`x` migration) changes the user's muscle memory for archiving. The undo safety net (`u`) MUST arrive simultaneously, so users learn the new key with the recovery path active. Shipping this spec without the undo spec would create a regression window where users have new keys but no safety net.

**Spec ordering for the release branch**:
1. This spec's commits 1–11 land first (small surgical fixes).
2. Destructive-undo spec lands as one feature commit on top.
3. The two ship as a single release.

The leader-key spec (`2026-04-28-tui-leader-key-design.md`) ships in the **next** release on top of this stabilized routing layer (its overlay needs the panic-Esc and Esc-cancel-hint hooks to clear cleanly).

### 6.3 Hint-rendering slot priority

Multiple subsystems render to the bottom-of-view single-line hint slot. Priority (highest first):

1. **Panic-Esc reset confirmation** — "Returned to Dashboard root" (1s flash; T2.9).
2. **Tombstone toast** — "Archived 'foo'. Press u to undo (60s)" (destructive-undo spec).
3. **Leader-menu inline preview** — "Space: i toggle vim · m plan mode · …" (leader-key spec; lower-priority because the menu has its own overlay area).
4. **Esc-cancel-stream hint** — "Esc cancelled the active turn" (§4.9, T1.10).
5. **General status / completion-picker hint** — current default content.

Higher-priority items overwrite lower; lower items render only when slot is otherwise idle.

### 6.4 Cross-spec carve-outs

- **IssueBrowser status-key suite (`o`/`w`/`b`/`d`/`W`)** shadows vim Normal in `mapping.md §2.6` (gemini's review). This spec only deprecates `d`→`x`; the broader relocation of the status keys behind a leader sequence is deferred to the leader-key spec (§3 candidates list).
- **Number-key cleanup (`Ctrl+1..5` → `1..5` when no input bar; relocate IssueBrowser off `2`)** is NOT in scope here. Tracked in the registry-refactor / leader-key follow-ups.

## 7. Open questions

1. **Help overlay maintenance**: should the keyboard-environment caveats be hard-coded in the overlay, or sourced from `docs/superpowers/specs/...`? Recommend hard-coded with a link — overlay must work offline.
2. **Panic Esc window**: 500ms feels right; do we want it configurable? Recommend hard-code; if power users complain, expose later.
3. **`x` as cut conflict**: Yazi/nnn use `x` for cut. SessionPicker has no cut/yank semantics, so no live conflict. Note in docs.
4. **`d` deprecation timeline**: is "one release" enough for users to migrate? The mapping doc + this spec become the migration reference. Recommend two minor releases as a safety margin.

## 8. Method note

This spec consumes:
- Findings T1.1–T1.10 + T2.1–T2.11 from the ergonomic review (with re-prioritization from §12.7 of that doc).
- Codex + gemini cross-check positions reconciled into the brain's final SHIP-NOW list.
- F8 (commit `bd5c4b5c`) as proven prior-art for the "ship-now-style" fix pattern.

Three companion specs hold the deferred architectural work:
- `2026-04-28-tui-leader-key-design.md` — Space-leader namespace.
- `2026-04-28-tui-destructive-undo-design.md` — undo + confirm dialogs for state mutations.
- `2026-04-28-tui-keybinding-registry-design.md` — `is_view_action_char` data-driven registry.
