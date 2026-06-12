# Open Design App

Open Design is a gallery app for notebook-driven visual design. It packages the
SPUR `open-design` brain skill together with the vendored Open Design runtime
libraries:

- `library/open-design-library/` for design systems
- `library/open-design-deck-library/` for deck themes and the deck skeleton
- `library/skill-catalog/` for upstream Open Design skill definitions

The current host still exposes `open_design_search` and `open_design_get` as
foundation MCP tools. Those tools already support `SPUR_OPEN_DESIGN_LIBRARY`;
point that variable at this app's `library/` directory to resolve assets from
the app instead of the crate assets.

The skill catalog intentionally includes only `SKILL.md` definitions from the
ignored upstream `resources/open-design/skills/` tree. Heavy examples, generated
assets, and vendored helper code stay out of the app until a gallery runtime
needs them explicitly.

The app manifest keeps the skill at `skill/SKILL.md`, matching the generic
`spur-app.json` app-mode contract used by other gallery apps.
