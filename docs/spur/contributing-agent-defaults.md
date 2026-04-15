# Contributing Agent Defaults

Built-in delegation descriptors for known agents live in `crates/spur-acp/src/agents/defaults.toml`. Edit carefully — every change ships to all users on the next release.

## Guidelines

1. **Coarse and stable.** Descriptors should age over 6-12 months without breaking routing. Avoid version numbers, benchmark scores, or workflow-specific details.
2. **Imperative, short.** `good_for` entries are task patterns, not sentences. Target under 60 chars each. The lint fires at 80 but prefer tighter.
3. **Negative space matters.** `avoid_for` is as important as `good_for` for routing — it's often the tiebreaker when multiple agents match.
4. **Output shape is the brain's signal for task-prompt shaping.** Be specific about what the worker actually produces (diff? spec artifact? narrative?).

## Process for adding a new agent

1. Add a section to `defaults.toml`.
2. Add the agent name to `known_agents()` in `defaults.rs`.
3. Add the test case in `every_known_agent_resolves_to_a_descriptor`.
4. Update `docs/spur/agent-config.md` if this agent needs any special config notes.
5. Run `cargo test -p spur-acp --lib agents::`.

## Process for tuning an existing descriptor

1. Open an issue describing the observed misrouting.
2. Edit `defaults.toml`.
3. Add a regression test if the misrouting is reproducible with a fixture config.
4. Tag the release notes: this changes brain behavior.

## Maintenance item: capability keyword table

In `defaults.rs`, `CAPABILITY_KEYWORDS` maps capability tokens (`plan_mode`, `usage`, etc.) to trigger keywords the lint scans for in `good_for` strings. When a new token is added to `AgentConfig::capabilities`, add its trigger keywords to this table so the lint keeps working.
