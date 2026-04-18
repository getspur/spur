# .spur/skills/ — User-Override Skills

This directory holds project-level overrides for SPUR's bundled skills.
Files here take precedence over the built-in defaults shipped with SPUR.

## How it works

SPUR ships bundled SKILL.md files for brain delegation guidance. To
customize behavior for your project, create a matching directory here:

```
.spur/skills/
├── brain-delegation/           # override shared delegation procedure
│   └── SKILL.md
├── brain-delegation-kiro/      # override Kiro brain role guidance
│   └── SKILL.md
└── brain-delegation-gemini/    # override Gemini brain role guidance
    └── SKILL.md
```

Only the files you create are overridden — all other skills fall back
to the bundled defaults.

## Format

Files follow the [Agent Skills](https://agentskills.io) open standard:
YAML frontmatter (`name`, `description`) plus a markdown body. SPUR
extends the frontmatter with `role`, `agent`, and `activation` fields.

## When to override

- Your project needs different delegation routing heuristics
- You want to add project-specific constraints to the brain prompt
- A new agent is configured that has no bundled default
