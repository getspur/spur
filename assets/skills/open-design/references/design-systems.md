# Open Design — Design-System Library

148 branded design systems (Linear, Stripe, Vercel, Airbnb, …) vendored under the
Open Design asset library: `assets/open-design-library/design-systems/<id>/DESIGN.md`,
with a compact `index.json` beside them.

## index.json schema
`{ version, kind: "design-systems", count, items: [ { id, title, category, summary, swatches[] } ] }`
- `swatches` are up to 6 lowercase hex codes in document order; may be fewer than 6 (never more).

## Selecting a design system (Direction step)
1. If the user names a brand or a strong visual reference, call
   `open_design_search({ query, kind: "design-systems" })` and choose the closest
   match from the ranked `items`.
2. Call `open_design_get({ kind: "design-systems", id })` for the selected item,
   use its `design_md` for the full palette, type stack, and posture, and bind it
   to the artifact's CSS `:root`.
3. **DEV FALLBACK:** in an in-repo checkout where the tools are unavailable,
   `Read assets/open-design-library/design-systems/<id>/DESIGN.md`.
4. If no brand fits, fall back to the 5 directions in `references/directions.md`.

> Runtime selection is tool-driven so packaged installs do not depend on repo-relative
> asset paths. The `Read assets/...` path is only for local development fallback.
