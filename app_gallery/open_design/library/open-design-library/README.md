# Open Design — Asset Library

Vendored from Open Design (`resources/open-design/`, gitignored upstream). M3.5 ships
the **design-systems** layer only; mode skills + themes arrive in M4.

## Layout
- `design-systems/<id>/DESIGN.md` — 148 branded design systems (palette + type + posture).
- `index.json` — generated discovery/selection metadata (`id`, `title`, `category`, `summary`, `swatches`).

## Regenerate
`python3 build_index.py`  · verify in CI/review with `python3 build_index.py --check`.

`index.json` is committed; `--check` fails if it drifts from the `DESIGN.md` set.
Runtime install location and access surface (Read vs MCP) are finalized in M4.
