# Open Design — Artifact Deck Track

The **default** deck path is native Jute deck mode (`references/deck-mode.md`): editable,
reactive, present mode built in. Escalate to this **artifact track** only when the brief
wants a *polished, branded, pixel-fidelity* presentation that native layouts can't express.

## When to escalate (track-selection rule)
| Brief signal | Track |
|---|---|
| "working deck", "outline", "I'll edit slides", data/charts, reactive | **Native** (default — `deck-mode.md`) |
| "magazine", "launch", "investor pitch", "polished", a named taste (WIRED / editorial / brutalist / cyber), WebGL/hero | **Artifact** (this file) |
| unsure | **Native** — it's editable; the user can ask to "make it polished" to escalate |

## Build an artifact deck
1. **Pick a theme.** Call `open_design_search({ query, kind: "deck-themes" })` and
   choose the closest match from the ranked `index.json` `items` by `id` / `title` /
   `scenario` / `summary` (and `swatches` for palette fit).
2. **Fetch the theme and framework.** Call
   `open_design_get({ kind: "deck-themes", id, include_skeleton: true })`. Use its
   `skill_md` for the theme rules, optional `example_html` for the concrete pattern,
   and `deck_skeleton_html` as the 1920×1080 fixed canvas with scale-to-fit,
   keyboard nav, slide counter, and print-to-PDF already baked in.
   **DEV FALLBACK:** in an in-repo checkout where the tools are unavailable,
   `Read assets/open-design-deck-library/deck-themes/<id>/SKILL.md`,
   `Read assets/open-design-deck-library/deck-themes/<id>/example.html` when present,
   and `Read assets/open-design-deck-library/deck-skeleton.html`.
   Do NOT re-derive the scaling/focus JavaScript; that is the whole point of shipping
   the framework verbatim.
3. **Fill only the `SLOT:` markers** — deck title, the `:root` theme tokens (bind the
   chosen theme's palette + fonts), the per-deck `<style>` block, and the
   `<section class="slide">` bodies. Leave the framework `<style>`, the chrome, and the
   trailing `<script>` untouched.
4. **Emit one cell.** Write the finished single HTML file as a `text/html` cell output
   (the M1 substrate) — it renders in Jute's sandboxed iframe (`allow-scripts`).
5. **Critique** with the deck-artifact checks in `references/critique.md`, then revise.

> This track produces one opaque HTML deck — no cell↔slide mapping or native present mode.
> If the user wants to edit slide-by-slide, use the **native** track instead.
