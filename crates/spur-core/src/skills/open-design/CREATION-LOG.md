# open-design — creation log

- **2026-05-31** — Created for the "Open Design on Jute" host-shell M1 vertical slice.
  Re-homes Open Design's prompt stack (discovery / directions / critique) as a
  notebook-driven SPUR skill. Source spec:
  `docs/superpowers/specs/2026-05-31-open-design-jute-host-shell-design.ipynb`.
  Replaces OD's Node daemon agent loop with `notebook_*` MCP tool driving.
  Distribution: brain-role, so `skills init` materializes it only under
  `.spur/skills/open-design/` (worker adapter dirs are intentionally skipped).

- **2026-06-01** — M3.5: vendored 148 design systems under
  `crates/spur-notebook/assets/open-design-library/design-systems/`, added
  `build_index.py` + committed `index.json`, and wired the Direction step to the
  design-system library via `references/design-systems.md`. Read-driven selection;
  runtime install path + MCP surface deferred to M4. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-asset-library-design.ipynb`.

- **2026-06-01** — M2a: native deck track. Added `references/deck-mode.md` (one cell per
  slide + `jute_deck` via `set_cell_metadata`, 12 layouts, present mode), routed the Artifact
  step to it for `kind: deck`, and added deck-specific critique checks. No jute-notebook
  changes (deck mode already exists). Artifact-deck track + `html-ppt-*` themes = M2c;
  `dispatchDeckCommand` reconciliation = open decision. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb`.

- **2026-06-01** — M2b: theme port. Ported the 5 Open Design directions
  (`editorial-monocle`, `modern-minimal`, `warm-soft`, `tech-utility`, `brutalist`)
  into Jute's native deck `THEMES` as CSS-token themes (OKLch palettes + font stacks
  injected by `SlideFrame`). Native decks now offer 8 themes; the 3 class-only built-ins
  are unchanged. Spec: `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb`.
