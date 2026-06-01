# Open Design — Deck Theme Library

Vendored from Open Design (`resources/open-design/skills/`, gitignored upstream). M2c
ships the **artifact-deck** track: 51 self-contained 16:9 HTML deck themes + the shared
`deck-skeleton.html` fixed-canvas framework.

## Layout
- `deck-themes/<id>/` — a vendored deck theme (SKILL.md spec + example/template HTML;
  zhangzara themes also carry `template.json`).
- `deck-skeleton.html` — `DECK_SKELETON_HTML` verbatim (1920×1080 scale-to-fit, keyboard
  nav, print-to-PDF). Copy it verbatim; fill only the `SLOT:` markers.
- `index.json` — generated discovery metadata (`id`, `title`, `scenario`, `mode`,
  `featured`, `summary`, `source`, `swatches`).

## Regenerate
`python3 build_index.py`  ·  verify with `python3 build_index.py --check`.

Excludes `simple-deck`/`weekly-update` (need an active design system) and
`html-ppt-retro-quarterly-review` (video/template mode). Runtime access surface
(Read vs MCP) is finalized in M4.
