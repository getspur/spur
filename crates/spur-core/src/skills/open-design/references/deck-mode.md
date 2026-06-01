# Open Design — Native Deck Mode

When the brief is a deck (`kind: deck`), do NOT emit a single HTML blob. Build a
**native Jute deck**: the notebook IS the deck, one cell per slide. Jute renders it
via `cellToSlide` + layout components, with present mode, speaker notes, and bullet
reveal already built.

## Set up the deck (notebook-level)
Set `metadata.jute_deck` on the notebook: `{ theme: "minimal-light", aspect: "16:9",
title: "<deck title>", author?: "<name>" }`. (More themes arrive in M2b; the 3
built-ins are `minimal-light`, `minimal-dark`, `spur-brand`.)

## One cell per slide
- `notebook_insert_cell(kind="markdown", source="...")` for prose slides (the common case);
  `kind="code"` only when the slide shows live code/output.
- Then `set_cell_metadata(id, patch={ ... }, expected_version)` to set the slide's
  `jute_deck` facet. The patch merges into `cell.metadata.jute_deck`.

## Per-slide `jute_deck` fields
- `layout`: one of `title · section · content · bullets · code · output · code-output ·
  two-col · image · blank` (omit or `auto` to infer).
- `speaker_notes`: markdown shown only via the `S` overlay in present mode.
- `fragments`: `true` to reveal markdown bullets one at a time.
- `background`: per-slide color or image URL.
- `theme_override`: a theme id for this slide only.
- `hidden`: `true` to keep a cell in the notebook but skip it in the deck.

## Layout inference (so you can often skip `layout`)
- `# H1` (one line) → `title`
- `## H2` (one line) → `section`
- lines starting `- ` / `* ` → `bullets`
- code cell → `code` (or `code-output` when it has output)
- otherwise → `content`; use `two-col` / `image` explicitly when relevant.

## Flow
1. Set notebook `jute_deck` (theme, aspect, title).
2. For each slide: `notebook_insert_cell` → `set_cell_metadata(jute_deck:{layout, speaker_notes?, fragments?})`.
3. Keep one idea per slide; let inference pick the layout unless you need an explicit one.
4. Critique with the **Deck-specific checks** in `references/critique.md`, then revise the slide cells.

> The polished/branded "artifact deck" track (OD's magazine/launch HTML themes) lands in
> M2c. For now, native deck mode is the path for every `kind: deck`.
