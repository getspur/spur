# SPUR TUI Keybindings — Ergonomic & Industry-Pattern Review

**Companion to**: `docs/rca/2026-04-28-spur-tui-keybindings-mapping.md`
**Date**: 2026-04-28
**Method**: 8-round sequential first-principles thinking + industry research (lazygit, k9s, helix, vim, emacs, less, fzf) + cross-validation pending (gemini + codex).

---

## 0. Evaluation criteria

Four axes drawn from Helix's stated keymap principles ([helix discussion #2563](https://github.com/helix-editor/helix/discussions/2563)) and the established TUI conventions of lazygit / k9s / vim:

| # | Axis | Source |
|---|---|---|
| 1 | **Keyboard-first** | All actions reachable without mouse. Trivially true for SPUR; no findings. |
| 2 | **Ergonomic** | Frequent actions on/near home row (HJKL); no awkward modifier stacks; capitals only when semantically needed. |
| 3 | **Muscle-memory cost** | Each key carries ≤2 distinct meanings across views; same operation reuses the same key. |
| 4 | **Industry-pattern** | `j/k`, `g/G`, `?`, `/`, `q`, `Esc`, `Enter`, vim h/l/w/b/e, emacs C-a/e/k/y, fzf Ctrl+P/N. |

Helix's three-principle frame:
- **Build upon previous knowledge** — `Alt+h` should relate to `h` semantically.
- **Ergonomics** — common actions = short finger travel.
- **Spatial semantics** — keys above/below = up/down semantics.

---

## 1. Where SPUR conforms (the good parts)

| Pattern | SPUR follows | Evidence |
|---|---|---|
| `j/k` row navigation | ✅ | Dashboard, SessionDetail, IssueBrowser, PlanInspector, picker, input_bar |
| `g/G` top/bottom | ✅ | All listy views |
| `h/l` left/right (tab cycle for SPUR) | ✅ | Dashboard detail tabs, PlanInspector lanes |
| `?` opens help | ✅ | App overlay; Dashboard / IssueBrowser emit `ShowHelp` |
| `Esc` cancels/backs out | ✅ (with one wart, see §2.1) | Layered correctly across modals |
| `Enter` accepts/submits | ✅ | Pickers, input_bar |
| `Tab/Shift+Tab` cycle | ✅ | Dashboard panels, picker accept |
| Vim Normal h/l/j/k/w/b/e/0/^/$/gg/G | ✅ | input_bar:482–541 |
| Emacs C-a/e/b/f/n/p/d/k/y/u/w | ✅ (via tui-textarea fallback) | input_bar:411 |
| fzf-style picker (Up/Down/Tab/Enter/Esc) | ✅ | picker_shell.rs:103 |
| Inverse-pair on same row (`g/G`, `n/N`, `h/l`, `[/]`) | ✅ | Pervasive |
| `Ctrl+K` command palette (VS Code convention) | ✅ | app.rs:980 |
| `Ctrl+P/N` for history (fzf/emacs convention) | ✅ | dashboard cross-mode + emacs path |

**Verdict**: SPUR's foundation is industry-aligned. The findings below are surface-level inconsistencies, not architectural flaws.

---

## 2. Tier 1 — fix soon (real UX bugs / cross-view inconsistencies)

### 2.1 `q` semantics inconsistency

| View | `q` does |
|---|---|
| Dashboard | `Action::Quit` (immediate quit) |
| IssueBrowser | `Action::Quit` (immediate quit) |
| MermaidViewer | `NavigateBack` (close view) |
| App-level | First press → opens quit-confirm dialog (only via `Ctrl+C`/`Ctrl+Q`) |
| SessionPicker, SessionDetail, PlanInspector | no binding |

**Industry**: lazygit, k9s, less, vim, htop — `q` always = "back/close current view". Quit-app is a chord (`Ctrl+C`/`Ctrl+Q`).

**Recommendation**: rebind `q` everywhere → `NavigateBack` (or close-view equivalent). Keep `Ctrl+C`/`Ctrl+Q` chord for quit-confirm. Migration: announce in changelog. Optionally, first press of `q` shows a one-time toast "q now closes the view; press Ctrl+Q to quit".

**Risk**: existing users have muscle-memory for `q`=quit. Acceptable migration cost given alignment payoff.

### 2.2 `Ctrl+O` is dead on dashboard, live on session_detail

- **Dashboard** (`dashboard.rs:901`): `Ctrl+O` is forced to `KeyOwner::View`, but `handle_view_key`'s `o` arm at `:1038` excludes Ctrl/Alt → **silent no-op**.
- **SessionDetail** (`session_detail.rs:1404`): `Ctrl+O` toggles Observe-entries collapse. Wired.

**Recommendation**: wire dashboard's `Ctrl+O` as alias for observe-toggle (so vim users without a focused-detail-pane can still hit it from anywhere). Or strip from the global bypass list and document its absence. Pick one — don't leave dead code.

### 2.3 SessionPicker error-state `r retry` hint without handler

`session_picker.rs:29` displays footer hint `r retry · Esc back` for `PickerState::Error`, but the `Loading | Error` arm at `:1483` has no `KeyCode::Char('r')` branch. **Aspirational hint.**

**Recommendation**: add the handler (emit `Action::RefreshSessions` even from Error state) — the cost is one branch, the user clearly expects this to work.

### 2.4 `/` for search/filter is ONLY in SessionPicker

Industry universal: `/` opens a filter/search input.

| View | Has `/` | Missing |
|---|---|---|
| SessionPicker | ✅ filter | — |
| Dashboard activity log | ❌ | Filter log entries by text |
| IssueBrowser | ❌ | Filter issue titles |
| PlanInspector | ❌ | Filter task descriptions (DAG-aware) |

**Recommendation**: add `/` filter to all listy views. PlanInspector is the highest-value target (a 100-task plan is unnavigable without filter). Multi-PR scope but conceptually identical to SessionPicker's existing implementation.

---

## 3. Tier 2 — surprise vectors (reconsider)

### 3.1 `d` means "archive" in SessionPicker — violates "d=delete" universal

`session_picker.rs:1447` binds `d` → `ToggleSessionArchive`. Every other TUI uses `d` for delete. User pressing `d` half-expects destructive removal; gets archive (mild) — positive surprise but still violates muscle memory.

**Recommendation**: rebind to `x` (vim convention "x is mild deletion"). Frees `d` for future actual-delete if needed. Migration cost low.

### 3.2 `N`/`P` for jump-to-review/prev-review collide with vim search-repeat

`dashboard.rs:1160-61` binds capital `N`/`P` to `JumpToReview` / `JumpToPreviousReview`. Vim users type `N` to repeat search backward. SPUR doesn't implement vim search, so the conflict is theoretical, but the symbol is wrong.

**Recommendation**: leave as-is for now (no live conflict), but if vim search ever lands, rebind to `]r` / `[r` (vim convention "next/prev of category R"). Add as alias today: `]r`/`[r` cooperates with future search.

### 3.3 `Alt+*` namespace is dense and Mac-hostile

SessionDetail has 8 Alt-keys: `Alt+I/M/S/W/D/P/R/V`. macOS Terminal.app default treats Alt as "send Esc+letter" (Meta), not a modified char. **Without "Use Option as Meta key" enabled, none of these work.**

**Recommendations**:
1. Document the macOS Terminal.app requirement at the top of any keyboard-shortcuts help screen.
2. Provide non-Alt aliases for the most common shortcuts. Helix uses `Space` as leader; SPUR could adopt `Space` as leader (e.g. `Space w` = workers, `Space s` = sessions, `Space p` = plan inspector). This is a separate ADR but worth opening.
3. Don't add MORE Alt-keys without a leader-key migration plan.

### 3.4 `r` has 4 meanings across views with confusable cross-view semantics

| Context | `r` does |
|---|---|
| SessionPicker (browse) | `RefreshSessions` |
| Dashboard (no focused node OR not Review tab) | `JumpToReview` |
| SessionDetail (`Ctrl+R`/`Alt+R`) | open history picker |
| (aspirational) SessionPicker error state | retry |

User pressing `r` in dashboard expecting "refresh" (from picker) gets "JumpToReview". Real surprise.

**Recommendation**: standardize. Pick ONE meaning per context family and use it. E.g.:
- `r` everywhere = "refresh/reload current view's data" (sessions, issues, log).
- `]r` / `[r` = jump to next/prev review.
- `Ctrl+R` / `Alt+R` = open history picker (kept for session_detail).

### 3.5 PlanInspector `g`/`G` semantics drift on layout

`plan_inspector.rs:133-134`: `g/G` go to first/last task IN CURRENT LANE in wide mode but GLOBALLY in stacked mode. **Same key, different scope based on terminal width.**

**Recommendation**: pick one (in-lane) and keep it stable. Width-conditional behavior is exactly the kind of surprise that erodes trust in keybindings.

### 3.6 Tab cycle exists on dashboard, missing on session_detail

Dashboard cycles Agents↔Log via Tab/Shift+Tab. SessionDetail has no equivalent for its panels.

**Recommendation**: add Tab/Shift+Tab cycle to session_detail panels. Cheap consistency win.

---

## 4. Tier 3 — convention-alignment opportunities

### 4.1 Add `:` as command-palette alias

`Ctrl+K` is VS Code convention. `:` is vim/k9s convention. Add `:` as alias — zero conflict (no existing `:` binding), gives vim users their muscle memory.

### 4.2 Number-key namespace is bifurcated

| Context | What `1` does |
|---|---|
| Dashboard, no focused node | focus Agents panel |
| Dashboard, focused node | (no binding; `Ctrl+1` jumps to Stream tab) |
| Dashboard, no focused node, `2` | navigate to IssueBrowser (ANOTHER VIEW) |

**Two issues**:
- `2` jumps to a different VIEW while `1` and `3` switch panels WITHIN dashboard. Inconsistent.
- `Ctrl+1..5` for tabs is OK-ish but the modifier requirement is ergonomic friction. lazygit uses `1..5` directly for tab jumps when no input is active.

**Recommendation**:
- Keep `1`/`3` for panel focus.
- Move IssueBrowser navigation to `Alt+B` (Browser) or to `Space b` (leader-style) — frees `2` for the still-empty middle slot.
- When a node is focused, drop the `Ctrl` requirement: `1..5` directly switch detail tabs (since input bar is governed by Compose mode anyway).

### 4.3 Capital `O` in vim-entry whitelist with no view-action target

`dashboard.rs:935` lists `O` as a vim-entry char (compose newline-above-and-insert). Lowercase `o` is a view action (toggle observe) when `focused_node`. Asymmetric. If a future feature wants capital `O` as a view action, the precedent is clear (just add it to `is_view_action_char`).

**Recommendation**: leave as-is, but document the asymmetry so future contributors know the rule.

---

## 5. Tier 4 — vim feature parity (deferrable)

The SPUR input_bar implements ~80% of vim Normal mode but is missing:

| Vim feature | Severity | Defer? |
|---|---|---|
| Counted motions (`5j`, `3w`, `10G`) | High — heavy vim users notice immediately | No — fix soon |
| Undo/redo (`u`/`Ctrl+r`) | Critical — no editor should ship without undo | **Verify if tui-textarea exposes; expose if so** |
| Replace char (`r`) | Medium | Defer |
| Find char (`f`/`F`/`t`/`T` + `;`/`,`) | Medium | Defer |
| Search (`/`/`?` + `n`/`N`) | High (also blocks `/` filter cross-cutting fix) | Phase 1: just `/` (filter). Phase 2: vim search later. |
| Ex command (`:`) | Low (palette covers it via `Ctrl+K`) | Defer |
| Paste-before (`P`) | Low (asymmetry only) | Defer |
| Text objects (`ci"`, `da(`, `vit`) | Medium — power-user feature | Defer |
| Visual block (`Ctrl+v`) | Low (`Ctrl+v` taken for scroll) | Skip permanently |

---

## 6. Tier 5 — discoverability

### 6.1 Audit `?` help overlay

App-level `?` opens help overlay (`app.rs:944`). Verify the overlay shows **per-view + per-mode** keys, not a static global cheatsheet. Helix's "describe-bindings" command ([helix #11708](https://github.com/helix-editor/helix/discussions/11708)) is gold-standard.

### 6.2 Add a "describe-key" debug command

Press `?k` then any key → "this key currently does X in this context". Self-documenting system. Helix has this. Medium effort; high payoff.

### 6.3 Mid-modal hint rendering

The hint row above the input bar overdraws the bottom border (separate bug, see prior discussion). Once fixed, it should display the **5 most-relevant keys** for the current panel/mode. Low-noise, high-discovery.

---

## 7. Architectural smells (not fixes, but flags)

### 7.1 `is_view_action_char` is a brittle registry

`dashboard.rs:954-968` hard-codes the view-action map. Any new view binding must be added here OR it becomes unreachable in vim mode (the `o` bug). Future-proofing: encode bindings as data (a registry that maps `(KeyCode, Modifiers, ViewContext)` → `Action`) and derive `is_view_action_char` from it.

### 7.2 Mode-mixing risk: input_bar Vim Visual + dashboard Navigate

If reachable, `is_vim_normal()` returns false and the vim arm guard fails. Currently unreachable in practice (Esc handling drains Visual/Operator before exiting Compose). Add `debug_assert` in `set_mode(Navigate)` to enforce.

### 7.3 Picker + history-picker stacking

What happens if `Ctrl+R` is pressed while a `@mention` picker is open in session_detail? Two pickers active. Probably state-conflict. Test.

---

## 8. Industry references (citations)

Practices that informed this review:

- **lazygit** ([keybindings reference](https://lazygit.dev/keybindings/), [GitHub docs](https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md)): `?` for help, lowercase=action / uppercase=stronger, `[`/`]` for adjacent navigation, `/` for filter.
- **k9s** ([hotkeys docs](https://k9scli.io/topics/hotkeys/)): vim h/j/k/l, `?` help, `:` command mode, escape exits filter, `q` for back.
- **helix** ([keymap docs](https://docs.helix-editor.com/keymap.html), [keymap consistency discussion #2563](https://github.com/helix-editor/helix/discussions/2563), [describe-bindings #11708](https://github.com/helix-editor/helix/discussions/11708)): three principles — build upon previous knowledge, ergonomics, spatial semantics. Space as leader key.
- **vim** (built-in `:help`): h/j/k/l motion, modal model, operator-pending, `n/N` search-repeat, `;`/`,` find-repeat, `:` ex command.
- **emacs**: C-x prefix for extended, M-x for command-by-name, C-a/e/b/f/n/p/d/k/y for nav+kill.
- **fzf**: Ctrl+P/N for picker nav, Tab for select, Enter for accept, Esc to cancel.
- **less / man**: j/k space PgDn, q quit, / search, n/N repeat, g/G top/bottom.
- **Helix-mode for Emacs** ([github](https://github.com/mgmarlow/helix-mode)): demonstrates that Helix's selection-then-action model is consistent enough to port.
- **Ergonomic keyboard layouts** ([eureka-ergonomic blog](https://eurekaergonomic.com/blogs/eureka-ergonomic-blog/keyboard-tray-vs-desktop-vim-users-ergonomic-guide), [muscle-memory friendly home-row mods](https://blog.getreu.net/20250826-muscle-memory-friendly-home-row-mods/)): HJKL home-row dominance is non-negotiable for vim power users.

---

## 9. Prioritized action list

**Ship now (low risk, high payoff):**
1. Wire dashboard `Ctrl+O` to observe-toggle alias (or strip dead bypass).
2. SessionPicker error-state `r retry` handler.
3. SessionPicker `d` → `x` for archive.
4. PlanInspector `g`/`G` always in-lane (drop layout dependency).
5. Add Tab/Shift+Tab cycle to session_detail panels.
6. Add `:` as `Ctrl+K` alias.
7. Document `Alt+*` namespace + macOS Terminal.app "Use Option as Meta" requirement in help overlay.

**Plan as separate ADRs:**
- A. `q` rebind: `q` = NavigateBack everywhere; `Ctrl+Q` for quit. Migration toast.
- B. `/` filter universal: dashboard log, IssueBrowser, PlanInspector.
- C. Counted vim motions (`5j`).
- D. Vim undo/redo (verify tui-textarea support first).
- E. Number-key namespace cleanup: drop `Ctrl+` requirement on tab jumps when node focused; relocate `2` → IssueBrowser to `Alt+B`.
- F. Leader-key architecture (`Space` as leader, helix-style) — paves the way for non-Alt shortcuts.
- G. `is_view_action_char` registry refactor — derive from binding metadata.
- H. Per-view + per-mode `?` help overlay; `?k` describe-key.

**Defer:**
- Vim feature parity (`r`, `f/F/t/T`, search, ex, text objects, `P` paste-before, visual block).

---

## 10. Open questions

1. **`q` rebind**: ship the breaking change, or stay non-aligned?
2. **Leader key adoption**: pursue `Space` as leader to migrate Alt+* workload?
3. **macOS Alt-key strategy**: invest in non-Alt aliases, or document the requirement and move on?
4. **Vim feature parity prioritization**: is undo/redo a P0 (most users will hit it within minutes) or do we accept it as a known gap?

---

## 11. Method note

This review was produced by:
- 8 rounds of sequential first-principles thinking (mcp `sequentialthinking`).
- Targeted web research validating training-knowledge claims about lazygit, k9s, helix, vim, ergonomic keyboard layouts.
- Cross-checking against the companion mapping document (`docs/rca/2026-04-28-spur-tui-keybindings-mapping.md`).

Two parallel cross-checks (gemini + codex) are queued to validate this analysis. Their feedback will be appended in §12 if it materially changes conclusions.

---

## 12. External cross-check — gemini + codex (parallel)

Both reviewers were given the mapping doc + this review doc and asked to validate, contest, and pressure-test the prioritization independently.

### 12.1 What both reviewers confirmed

- All T1 / T2 findings are real. Brain framing on `q`, `d`, `r`, `Alt+*` density, dead `Ctrl+O`, missing `r retry` handler, layout-dependent `g/G`, and brittle `is_view_action_char` are correct.
- Leader-key (`Space`) should be elevated from "deferred" to **start ADR now** — both agree.
- Vim undo > counted motions. Both elevate undo over count register.
- `is_view_action_char` is the deepest architectural smell — already produced unreachable bindings (the `o` bug we just fixed).

### 12.2 Where they disagreed sharply

| Question | Gemini | Codex | Resolution |
|---|---|---|---|
| `q` rebind | P0, universal NavigateBack | Contextual: subview=close, Dashboard=quit-confirm | **Codex** — gh-dash, Yazi, nnn all use contextual `q`. Gemini overshoots. |
| PlanInspector `g/G` | BLOCK ship-now; use `[`/`]` for lane, `g/G` absolute | SHIP in-lane; width-dependent is the bug | **Codex** — vim's `g/G` IS contextual to current buffer; kanban "current stage" maps better. |
| Document Alt+* | BLOCK — "cop-out, fix defaults via leader" | SHIP with broader caveats | **Codex** — document is right; expand caveats; leader-key is parallel ADR. |
| `d`→`x` migration | Hard ship | Transitional `d` alias + warning + hint update | **Codex** — softer migration. |
| Leader-key timing | P1 elevation | "Start ADR before adding more Alt" | **Convergent** — both pull it forward. |

### 12.3 New findings from gemini

- **No "panic Escape hatch"**: pressing Esc 3 times should reliably return to root. Currently context-dependent.
- **No OS-aware hint rendering**: text says "Alt+M" but Mac keyboards have "⌥". TUI makes no platform detection.
- **Mouse ergonomics blind spot**: Yazi/k9s blend KB+mouse seamlessly. SPUR is keyboard-only.

### 12.4 New findings from codex

- **`Ctrl+1..5` portability**: legacy terminals can't reliably encode modified printable keys ([WezTerm key encoding](https://wezterm.org/config/key-encoding.html)). Either test or add plain `1..5` aliases when input bar is inactive.
- **Dashboard `Tab` overload**: empty input → cycle example prompts; non-empty → cycle Agents/Log. Surprising. Move example cycle to a dedicated key.
- **IssueBrowser `d`-closes-issue** = same archive-not-delete problem as SessionPicker. Need cross-view **destructive/status-action policy**, not one-off patch.
- **SessionDetail `Esc` cancels in-flight stream** before navigating back. High user cost; must surface in hint while streaming.
- **`Alt+P` vs `Alt+p` case-sensitivity** is hard to communicate, degrades through Esc-prefix Meta.

### 12.5 Industry references added by codex (more grounded than gemini's editor refs)

- [gh-dash global keys](https://www.gh-dash.dev/getting-started/keybindings/global/) — `q` quit, `/` search, `r/R` refresh.
- [Yazi keymap docs](https://yazi-rs.github.io/docs/configuration/keymap/) + [default keymap](https://raw.githubusercontent.com/sxyazi/yazi/main/yazi-config/preset/keymap-default.toml) — `q` quit, `d` remove, `x` cut.
- [nnn usage](https://github.com/jarun/nnn/wiki/Usage) — `/` filter, `?` help, `q` quits context.
- [WezTerm keyboard encoding](https://wezterm.org/config/key-encoding.html) — Ctrl+digit / CSI-u caveats.
- [Zellij keybinding presets](https://zellij.dev/documentation/keybinding-presets) — explicit mode-bar approach.

### 12.6 Industry references added by gemini

- [Helix #2563 keymap consistency](https://github.com/helix-editor/helix/discussions/2563) — three principles (build upon, ergonomics, spatial).
- Helix `Space` leader → contextual menu (sidesteps modifier exhaustion entirely).
- Zellij mode-bar that explicitly locks modifier maps (eliminates `is_view_action_char` guesswork at the architecture level).

### 12.7 Reconciled final position

#### SHIP NOW (7 items, with cross-check tweaks folded in)

1. **Dashboard `Ctrl+O`**: wire to observe-toggle when `focused_node.is_some()`, OR strip from `:898-904` bypass list. Don't leave dead. Verify no tmux/Zellij prefix collision.
2. **SessionPicker error-state `r retry` handler**: emit `Action::RefreshSessions` from `PickerState::Error`.
3. **SessionPicker `d`→`x` for archive** (transitional): add `x` as new binding; keep `d` for one release with deprecation toast/hint; update footer copy.
4. **PlanInspector `g/G` always in-lane**: drop layout-conditional behavior. Stage-board scope is correct mental model.
5. **Tab/Shift+Tab cycle on session_detail panels**: SHIP with explicit guards — composer ownership, picker ownership, history-shell ownership all pre-empt Tab.
6. **`:` alias for `Ctrl+K`**: SHIP only in Navigate / non-text contexts. Do NOT steal literal `:` from the input bar.
7. **Document `Alt+*` + broader terminal caveats**: include macOS Option-as-Meta, AltGr, Esc-Meta ambiguity, `Ctrl+digit` legacy encoding, `Ctrl+Q`/`Ctrl+S` flow control, CSI-u/kitty modifications.

#### ELEVATE from "deferred" → "near-term ADR"

- **Leader-key (`Space`) architecture**: start ADR NOW, before adding more `Alt+*` shortcuts. Both reviewers converged on this.
- **Vim destructive-action undo** (split from generic vim undo): the `o`/`w`/`b`/`d`/`W` issue-status changes and `d`→archive in picker need either undo or confirm dialogs. Text-input undo can stay deferred.
- **`is_view_action_char` registry refactor**: encode as data, derive view-action chars from binding metadata. Closes the `o`-bug failure mode permanently.

#### NEW T1/T2 findings folded in (from cross-check)

| ID | Finding | Tier | Source |
|---|---|---|---|
| T1.9 | Dashboard `Tab` overloaded — empty buffer cycles examples, non-empty cycles panels | T1 | codex |
| T1.10 | SessionDetail `Esc` silently cancels in-flight streams before NavigateBack | T1 | codex |
| T2.7 | IssueBrowser `d`-closes-issue same archive-not-delete problem; need cross-view policy | T2 | codex |
| T2.8 | `Ctrl+1..5` portability via legacy-terminal encoding | T2 | codex |
| T2.9 | No "panic Escape hatch" (triple-Esc to root) | T2 | gemini |
| T2.10 | No OS-aware hint rendering (Mac shows "Alt+M" not "⌥ M") | T2 | gemini |
| T2.11 | Mouse-ergonomic blind spot (Yazi/k9s blend KB+mouse) | T2 | gemini |

#### What we're explicitly NOT doing

- **Universal `q` rebind**: rejected. Contextual `q` (subview close, dashboard quit-confirm) is the right model.
- **PlanInspector `g/G` to absolute plan jump**: rejected (codex). Stage-scope is correct.
- **Don't-document Alt+* / fix-defaults-instead**: rejected. Document AND start leader-key ADR in parallel.

### 12.8 Method note (cross-check)

- Both delegations were given identical prompts and the same two source documents.
- Gemini: `delegation_id=01c5a2d4-a68f-4aef-9e19-4964cd446661`. Strong on architectural pushback ("cop-out", "P0"), weaker on industry-reference grounding.
- Codex: `delegation_id=dec54406-69e7-4d6c-b499-a475340cee92`. Stronger on industry references and code-grounded nuance; resolves three of the gemini-vs-brain disagreements.
- Net effect: cross-check **did not invalidate the brain's framing**, but it tightened ship-now items 3, 5, 6 to TWEAKs, added 5 new findings (codex) + 3 (gemini), and elevated leader-key + vim destructive undo + registry refactor from "deferred" to "near-term ADR".
