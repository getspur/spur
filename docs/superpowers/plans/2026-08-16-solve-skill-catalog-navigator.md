# Solve Skill Catalog Navigator Plan

**Goal:** Refactor the foundation `solve` skill into a concise catalog-first
router without adding MCP tools or editing generated projections.

## TDD Sequence

1. Add bundled-skill contract tests requiring catalog-first ordering,
   progressive selectors/details, mode-specific outcomes, and removal of the
   old embedded pattern catalog.
2. Run the focused tests and observe failures against the original skill.
3. Replace the canonical skill body with routing, navigation, status, generic
   fallback, persistence, and proof-discipline guidance.
4. Add a cross-skill contract requiring TDD solve preflight to use catalog
   discovery before generic encoding; observe RED.
5. Update the two stale TDD passages and return the focused suite to GREEN.
6. Validate skill metadata, run broader skill/catalog tests, format, and inspect
   the final diff.

## Files

- Modify: `assets/skills/solve/SKILL.md`
- Modify: `assets/skills/test-driven-development/SKILL.md`
- Modify: `crates/spur-core/src/skills/mod.rs`
- Add: `docs/superpowers/specs/2026-08-16-solve-skill-catalog-navigator.md`
- Add: `docs/superpowers/plans/2026-08-16-solve-skill-catalog-navigator.md`

## Verification

```bash
scripts/spur-cargo test -p spur-core --lib skills::tests::solve_
scripts/spur-cargo test -p spur-core --lib tdd_skill_uses_catalog_first_solve_routing
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo fmt --all -- --check
```

Also run the skill-creator validator against `assets/skills/solve` and use
`git diff --check` before commit.
