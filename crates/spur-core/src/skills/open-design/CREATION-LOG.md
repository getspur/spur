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

- **2026-06-01** — M2c: artifact-deck track + theme library. Brain-vendored 51
  self-contained Open Design deck themes (`guizang-ppt`, `replit-deck`, 48 `html-ppt-*`)
  + `deck-skeleton.html` (the 1920×1080 fixed-canvas framework, verbatim) into
  `assets/open-design-deck-library/`, with a tolerant `build_index.py` + committed
  `index.json` (`--check`-guarded). Wired the skill's artifact-deck escalation
  (`references/deck-artifact.md` + SKILL.md step-4) and deck-artifact critique checks.
  Native deck mode (M2a) stays the default; the artifact track is the polish/brand
  escalation. Excludes `simple-deck`/`weekly-update` (need a design system) and
  `html-ppt-retro-quarterly-review` (video/template mode); those + `dispatchDeckCommand`
  reconciliation remain open. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb`.

- **2026-06-01** — M4: runtime access surface. Added a `crates/spur-notebook/src/open_design/` loader
  (resolution order: `$SPUR_OPEN_DESIGN_LIBRARY` → `~/.spur/open-design/` overlay → Tauri `resource_dir()`
  → repo `assets/`; read-in-place, no copy) and two MCP tools — `open_design_search` (deterministic
  field-weighted ranking over both libraries) and `open_design_get` (path-independent package fetch;
  `example_html` optional, present in 49/51 deck themes). Bundled both asset dirs as Tauri resources and
  rewired the skill's Direction / artifact-deck steps to call the tools (`Read` kept as dev fallback).
  `open_design_list` + MCP Resources deferred to M4+. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m4-runtime-access-design.ipynb`.

- **2026-06-01** — taste-skill cross-pollination. Folded two gates from the external
  `taste-skill` v2 framework (`github.com/leonxlnx/taste-skill`) into the references so they
  are permanent rather than per-session: (1) a **zero-tolerance em-dash ban** scoped to
  artifact-visible text in `references/critique.md` (new checklist bullet + a hard subsection
  with a mechanical `text/html` scan for `—` / `–` / `&mdash;` / `&ndash;`), which also
  corrected the old guidance that suggested `—` as a placeholder glyph; and (2) the **three-dial
  framework** (`DESIGN_VARIANCE` / `MOTION_INTENSITY` / `VISUAL_DENSITY`) in
  `references/directions.md`, with per-direction defaults and surface overrides, making the
  "embody the specialist" step deterministic. SKILL.md (SPUR-MANAGED) untouched; both edited
  files are read by the existing loop steps. Edits applied to the authoritative crate source;
  `.spur/.claude` copies regenerate on `skills init`.

- **2026-06-01** — artifact tracks A/B + Direction B default. Validated a second artifact
  track end-to-end in a live Deno-kernel notebook: components (Preact) + SSR baseline +
  Tailwind compiled in-kernel + esbuild inline-bundled client island, emitted as one
  self-contained `text/html` cell (zero external URLs). Bake-off vs the hand-written
  single-file track showed the hand-written DAG view fails the new scripts-off gate (it
  builds all DOM in script, so it renders blank with active content off), while the SSR
  track passes by construction. Decision: **Track B (componentized + SSR) is the default**;
  Track A (single-file HTML) is for simple / static artifacts and must pre-render its
  initial DOM. Added `references/artifact-tracks.md` (decision rule, hard constraints,
  Track B recipe skeleton, three gotchas: two-preact-instances, jsr-blocked http plugin,
  active-content-default-off) and rewrote SKILL.md step 4 to pick a track and allow a Deno
  build step (the prior "single-entry HTML, no build step" M1 line is superseded; the
  rendered output is still one inlined self-contained cell).
