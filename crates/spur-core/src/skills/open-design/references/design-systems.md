# Open Design — Design-System Library

148 branded design systems (Linear, Stripe, Vercel, Airbnb, …) vendored under the
Open Design asset library: `assets/open-design-library/design-systems/<id>/DESIGN.md`,
with a compact `index.json` beside them.

## index.json schema
`{ version, kind: "design-systems", count, items: [ { id, title, category, summary, swatches[] } ] }`
- `swatches` are up to 6 lowercase hex codes in document order; may be fewer than 6 (never more).

## Selecting a design system (Direction step)
1. If the user names a brand or a strong visual reference, scan `index.json` `items`
   by `id` / `title` / `category` / `summary` for the closest match.
2. `Read` that system's `design-systems/<id>/DESIGN.md` for the full palette, type
   stack, and posture, and bind it to the artifact's CSS `:root`.
3. If no brand fits, fall back to the 5 directions in `references/directions.md`.

> Runtime install location and any search tool / MCP Resource surface are finalized
> in M4. For now, selection is `Read`-driven against the committed library + index.
