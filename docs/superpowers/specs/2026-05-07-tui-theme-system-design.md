# TUI Theme System — Design

- **Status:** Draft (brainstorm output)
- **Date:** 2026-05-07
- **Owner:** TBD
- **Implements:** user-switchable themes for spur-tui
- **Out of scope:** branding/white-label, fonts, prompt symbols, banners

## Motivation

Three driving needs (in user words: A + B + C):

1. **Aesthetic preference.** Users want Dracula / Solarized / Catppuccin / personal palettes without patching source.
2. **Accessibility.** Current contrast is hard to read in some terminals and for some users; a light theme and a high-contrast theme need to ship out of the box.
3. **Terminal compatibility.** Spur should remain legible in 16-color terminals (e.g. some serial consoles, restricted CI shells) where truecolor is unavailable.

Branding/white-label (option D from brainstorm) is explicitly **out of scope**. No `branding.*` or `font.*` sections in theme files. Spur is a TUI; fonts are the terminal's job.

## Current state

- **Renderer:** ratatui + crossterm. Entry: `crates/spur-tui/src/tui.rs`, `crates/spur-tui/src/app.rs`.
- **Theme abstraction today:** none. 565 hardcoded `Color::` calls across ~40 files in `crates/spur-tui/`.
- **Existing semantic-ish enums** (these are precedents, not theme infrastructure):
  - `LicenseBadgeTone { Neutral, Success, Warning, Danger }` — `crates/spur-tui/src/components/status_bar.rs:36`.
  - `BadgeColor { Amber, Green, Red, Neutral }` — `crates/spur-acp/src/adapter/mod.rs:110` — emitted by adapters, consumed by TUI.
- **Config:** TOML, loaded by `load_config_for_repo` in `crates/spur-cli/src/main.rs:1150`. Lookup order: `.spur/config.toml` (project) → `~/.spur/config.toml` (user). Top-level struct: `SpurConfig` at `crates/spur-acp/src/config/mod.rs:382`. `[tui]` subsection: `TuiConfig` at line 452 (currently `edit_mode`, `disable_paste_burst`).
- **Color heat map** (top sites):
  - `components/trace_format.rs` — 57 calls
  - `components/react_trace/builder.rs` — 45 calls
  - `views/session_picker.rs` — 38 calls
  - `components/status_bar.rs` — 36 calls
- **Non-TUI surfaces:** `BadgeColor` flows from spur-acp into the TUI. No ANSI coloring libraries in any other crate; logs and ACP output are uncolored.

## Design

### Two-layer model: palette + semantic tokens

Themes have two layers:

1. **Palette** — a fixed set of named base colors. Every theme must define each entry (or inherit via `extends:`).
2. **Tokens** — UI roles (e.g. `border.focused`, `tool.family.thinking`) that resolve to palette entries. Tokens have built-in defaults; theme files override only where they care.

**Tokens reference palette keys only — no literal hex in tokens.** This is the key invariant: literal hex in tokens fragments themes (overrides don't inherit through `extends:`). If a theme needs a custom color, it adds it to the palette layer.

This mirrors VS Code, Helix, and Zed. It lets a Dracula theme be ~5 lines (palette only) or ~50 lines (per-token overrides) without forcing either shape.

### Palette (24 entries)

```yaml
# Surfaces
bg              # main canvas
bg_panel        # raised panels: status bar, sidebars
bg_selection    # selected row in a list
bg_overlay      # modals / palette / completion popup

# Text
fg              # primary
fg_muted        # secondary (timestamps, hints)
fg_subtle       # tertiary (placeholder, disabled)

# Text-on-X (guaranteed contrast partners; replaces a single fg_inverse)
fg_on_accent
fg_on_success
fg_on_warning
fg_on_danger
fg_on_info
fg_on_overlay

# Borders
border          # default border / separator
border_focused  # focused panel / active element

# Accents (neutral hues for variety; no semantic charge)
accent
accent_alt

# Status (reserved for genuinely exceptional states)
success
warning
danger
info

# Highlight
highlight       # search matches / inline emphasis (distinct from accent)

# Diff (foreground-only; promote to bg variants if/when inline diff rendering lands)
diff_add
diff_del
```

**Why 24 entries:**

- The six `fg_on_*` entries replace a single `fg_inverse`. spur uses `Color::Black` on `Color::Yellow` in `plan_pulse`, `input_bar`, and `session_detail`. A single `fg_inverse` fails when a theme remaps `warning` to a darker hue — the inverse is no longer legible. Each colored background that carries text needs its own legible partner. One slot per status color (`success`, `warning`, `danger`, `info`) plus `accent` and `overlay`.
- `border` and `border_focused` get explicit slots because `Color::DarkGray` is the second-most-common color in the codebase and structural to nearly every panel.
- `highlight` is distinct from `accent` so search matches and focused borders aren't forced to share a hue. (If `accent` is hot pink, search hits shouldn't be alarming.)
- Diff is **foreground-only**: 2 entries instead of 4. Spur's TUI doesn't render inline-bg diffs today (`diff_viewer.rs` exists; current rendering is verified at implementation time). Promote to bg variants only when needed.

### Token taxonomy

Tokens are dot-namespaced strings. Defaults bind each token to a palette key. The full list is enumerated at implementation time from a sweep of all 565 `Color::` sites; the design fixes the **shape**, not the exact list.

Sample bindings (illustrative — not exhaustive):

```yaml
# Chrome
status_bar.bg:               bg_panel
status_bar.fg:               fg
border.normal:               border
border.focused:              border_focused
spinner.fg:                  accent

# Pickers / lists
picker.selected.bg:          bg_selection
picker.selected.fg:          fg
picker.match.fg:             highlight
picker.hint.fg:              fg_subtle

# Tool families (neutral hues — no semantic charge)
tool.family.thinking:        accent_alt
tool.family.edit:            accent          # NOT warning — editing is neutral
tool.family.read:            info
tool.family.bash:            success
tool.family.task:            accent_alt

# License / status badges
license_badge.neutral.bg:    bg_panel
license_badge.neutral.fg:    fg_muted
license_badge.success.bg:    success
license_badge.success.fg:    fg_on_success
license_badge.warning.bg:    warning
license_badge.warning.fg:    fg_on_warning
license_badge.danger.bg:     danger
license_badge.danger.fg:     fg_on_danger

# Plan stages
plan.stage.queued:           fg_muted
plan.stage.running:          info
plan.stage.done:             success
plan.stage.failed:           danger
plan.stage.blocked:          warning

# Diff
diff.add.fg:                 diff_add
diff.del.fg:                 diff_del
diff.context.fg:             fg_muted

# Activity log
activity.think:              fg_muted
activity.act:                accent_alt       # NOT warning — "agent action" not "alarm"
activity.observe:            success
activity.delegate:           accent
activity.complete:           success
activity.error:              danger
activity.user_message:       accent
activity.permission:         warning          # genuinely an interrupt
activity.info:               fg
```

**Notable semantic moves vs current code:**

- `tool.family.edit` and `activity.act` no longer share `Color::Yellow` — they bind to `accent` / `accent_alt`. Editing is not a warning state; the current yellow conflates "agent is doing something" with "caution". Themes that want yellow tools can override.
- `license_badge.*.fg` uses `fg_on_*` partners instead of hardcoded `Color::Black`. Current code embeds the assumption that warning = light = needs dark text; this breaks under any high-contrast or dark-warning theme.

### `BadgeColor` mapping

The cross-crate `BadgeColor` enum (`Amber | Green | Red | Neutral`, emitted by spur-acp adapters) maps through tokens at the TUI rendering site, not at the adapter:

```
BadgeColor::Amber   → token "badge.amber.bg"  (default → warning)
BadgeColor::Green   → token "badge.green.bg"  (default → success)
BadgeColor::Red     → token "badge.red.bg"    (default → danger)
BadgeColor::Neutral → token "badge.neutral.bg" (default → bg_panel)
```

The adapter stays color-blind; only the TUI knows about themes. No spur-acp changes are required.

### File format

```yaml
version: 1                  # required; hard error on mismatch
name: dracula               # required; matches filename stem
description: Dracula theme  # optional

extends: dark               # optional; single-level only

palette:
  bg: "#282a36"
  fg: "#f8f8f2"
  accent:
    rgb: "#ff79c6"
    ansi: magenta           # optional unless theme declares ANSI compat
  accent_alt: "#bd93f9"     # short form: rgb only; ansi auto-resolved via role map
  # ... remaining entries ...

tokens:                     # optional; bindings reference palette keys ONLY
  tool.family.thinking: accent_alt
  picker.match.fg: highlight
  # NO literal hex allowed here — `picker.match.fg: "#ff79c6"` is rejected at parse
```

**Format rules (enforced at load):**

1. **`version: 1` is required.** Mismatched version → hard error, falls back to default theme, logs to stderr. (Silent partial loads create patchwork UIs and were the failure mode kimi flagged for the format-evolution case.)
2. **`extends:` is single-level.** A theme may extend a built-in (`dark`, `light`, `high-contrast`) or a user theme. The extended theme may not itself have an `extends:` field — chained inheritance is rejected at load. Merge semantics: the child's `palette:` and `tokens:` shallow-merge over the parent's; entries the child omits are inherited unchanged. There is no way to *unset* an inherited entry — to drop a binding, override it.
3. **Tokens reference palette keys only.** Literal hex in the `tokens:` section is rejected at parse. (Custom colors go in the palette layer.)
4. **Palette entries that don't carry an explicit `ansi:` are auto-resolved through the role map** (see ANSI fallback below) only when the runtime is in ANSI-16 mode.
5. **Unknown palette keys and unknown tokens are warnings, not errors.** A future spur version may add tokens; older theme files should still load. (This complements the `version` rule: `version` gates breaking changes, unknown-key tolerance gates additive changes.)

### Discovery and switching

- **Built-in themes:** shipped as embedded YAML in the binary at `crates/spur-tui/themes/`:
  - `dark.yaml` — pixel-perfect reproduction of current colors
  - `light.yaml` — accessibility variant
  - `high-contrast.yaml` — accessibility variant, declares full ANSI role map
- **User themes:** discovered from:
  - `~/.spur/themes/<name>.yaml`
  - `.spur/themes/<name>.yaml` (per-project, takes precedence on name collision; warning logged on collision)
- **Configuration:** `theme: String` field added to `TuiConfig` (`crates/spur-acp/src/config/mod.rs:452`). Default: `"dark"`.
- **Slash command:** `/theme` lists available themes; `/theme <name>` switches at runtime.
- **Live switching:** swap is in-memory; rendering reads through the active `Theme` reference, no restart required. The set of discovered themes is computed once at startup; refresh is manual via `/theme reload` (filesystem watching is explicitly **not** in scope — too much surface area for a feature that doesn't justify it).

### ANSI-16 fallback (role map)

The kimi review flagged that independent per-color RGB→ANSI auto-snap can destroy contrast (e.g. `bg → Black`, `fg → DarkBlue`). The design uses a holistic **role map** instead.

When the runtime detects a non-truecolor terminal (or the user opts into 16-color mode via `tui.color_depth = "ansi16"` config), every palette entry resolves through this role map:

```
bg              → Black
bg_panel        → Black                  (intensity differentiated by reverse-video where needed)
bg_selection    → Blue                   (background)
bg_overlay      → Black

fg              → White
fg_muted        → Gray
fg_subtle       → DarkGray

fg_on_accent    → Black
fg_on_success   → Black
fg_on_warning   → Black
fg_on_danger    → White
fg_on_info      → White
fg_on_overlay   → White

border          → DarkGray
border_focused  → Cyan

accent          → Cyan
accent_alt      → Magenta

success         → Green
warning         → Yellow
danger          → Red
info            → Blue

highlight       → LightYellow

diff_add        → Green
diff_del        → Red
```

**Rules:**

- This default role map is the `dark` theme's ANSI mapping. `light` and `high-contrast` ship their own.
- A theme "declares a role map" by carrying an `ansi:` value on each palette entry. There is no separate `role_map:` block — the role map is the union of per-entry `ansi:` declarations across the palette.
- A custom theme that wants ANSI compat **must** declare `ansi:` for each palette entry it cares about. Entries without `ansi:` fall through to the parent theme's role map (via `extends:`), or to the built-in `dark` map if no parent is declared. This is documented behavior, not a heuristic.
- The runtime never auto-snaps RGB → nearest ANSI for a single color. The choice is always: explicit role map, explicit per-entry `ansi:`, or fall through to a parent's role map.

### Backward compatibility

- The default theme (`dark`) reproduces the current TUI exactly. Users who never set `theme:` see no visual change at any point.
- During migration, surfaces that haven't been ported to read from theme keep their hardcoded `Color::` literals. They render the same regardless of active theme. This is acceptable: the contract is "theme system works on themed surfaces; un-themed surfaces are unaffected".
- The phased rollout (below) covers the high-traffic surfaces first so themes feel substantive on day one.

## Migration strategy (option C: foundation + high-traffic surfaces)

Five PRs land themes meaningfully; remaining surfaces migrate opportunistically.

| PR | Scope | Approx LoC | Visual change |
|---|---|---|---|
| **PR 1** | **Foundation.** `Theme` / `Palette` / `TokenMap` types, YAML loader, `version: 1` enforcement, `extends:` (single-level), role-map ANSI resolver. Embed `dark.yaml`, `light.yaml`, `high-contrast.yaml`. Loader unit tests. **No call-site changes.** | ~600 + 3 themes | None. |
| **PR 2** | **Threading.** Add `theme: &Theme` to render context. Wire `TuiConfig.theme` from config. Add `theme_compat::current()` shim returning literal colors that match today, used by un-migrated surfaces during the transition. | ~200 | None. |
| **PR 3** | **High-traffic surface migration, wave 1.** `status_bar.rs` (36 hits), `trace_format.rs` (57 hits), `react_trace/builder.rs` (45 hits). Replace `Color::X` with `theme.token("...")`. Default theme keeps colors identical. | ~400 | None when default theme; tinted under non-default. |
| **PR 4** | **High-traffic surface migration, wave 2.** `session_picker.rs` (38 hits), `session_detail.rs`, `plan_browser.rs`, `plan_inspector.rs`, `plan_stage_board.rs`. | ~500 | None when default theme. |
| **PR 5** | **`/theme` slash command.** List built-in + user themes. Switch at runtime. `/theme reload` rescans the filesystem. Documentation. | ~200 | New command. |

**After PR 5:** themes work on the surfaces a user looks at most (~50% of `Color::` usage). Remaining files migrate opportunistically as touched in unrelated work.

**Out of this plan (deferred):**

- Filesystem watch on `~/.spur/themes/` — manual `/theme reload` is enough.
- Theme editor / scaffold command (`/theme new <name>`).
- WCAG contrast validation. The `fg_on_*` palette entries make safe pairs *expressible*; enforcing them is a separate effort.

## Testing

- **Loader unit tests** (PR 1):
  - Valid theme parses; tokens resolve to palette entries.
  - `version: 0` or missing → hard error.
  - `extends:` chained → rejected.
  - Literal hex in `tokens:` → rejected.
  - Unknown token → warning, not error.
  - Round-trip: serialize a `Theme`, parse it back, equal.
- **Role-map resolver tests** (PR 1):
  - Every palette entry has an ANSI fallback under the default role map.
  - User palette entry without `ansi:` falls through to parent role map.
  - `light.yaml` and `high-contrast.yaml` declare full role maps.
- **Integration** (PR 5):
  - `/theme dracula` switches active theme; subsequent renders use new colors.
  - `/theme nonexistent` reports error, leaves active theme unchanged.
  - `/theme reload` picks up newly-added file in `~/.spur/themes/`.
- **Visual regression** (PR 3, PR 4): snapshot tests on representative views (status bar, picker, trace) under default theme; output must equal pre-migration baseline byte-for-byte.

## Open questions

1. **Theme schema versioning policy.** `version: 1` is the only version this design defines. The bump-policy ("when do we go to 2?") is deferred until the first breaking change is contemplated. The hard-error behavior on mismatch ensures we don't accidentally normalize loose handling.
2. **`tui.color_depth` config field.** The role-map kicks in either on terminal-capability detection or explicit user opt-in. The exact field name and detection logic are implementation decisions for PR 1.
3. **Per-project theme override semantics.** `.spur/themes/foo.yaml` shadows `~/.spur/themes/foo.yaml` on name collision. Whether this is silent, warned, or rejected is a small UX question for PR 5.

## References

- Brainstorm transcript: this document is the output of a brainstorming session on 2026-05-07.
- External design review by `kimi` (delegation `068f72fa-417d-4228-aab0-8a229dedd09d`) — the palette additions (`fg_on_*`, `border`, `highlight`), the role-map ANSI strategy, and the no-literal-hex-in-tokens rule come from that review.
- Inspirational priors: VS Code color theme spec, Helix theme TOML, Zed theme JSON.
