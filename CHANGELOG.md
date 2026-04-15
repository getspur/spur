## Unreleased

### Added
- **Brain delegation framework.** Per-agent descriptors in `[agents.entries.delegation]` (routing signals), structured `delegation_plan` MCP tool parameter on `delegate_to_worker`/`delegate_parallel` (reasoning trace), rewritten brain prompt behind `[brain.delegation] framework` flag. Dev builds default to `"v1"`; release builds default to `"legacy"` at v1 ship. See `docs/superpowers/specs/2026-04-15-brain-delegation-framework-design.md`.
- Built-in delegation descriptors for `claude-code-acp`, `kiro`, `codex`, `gemini` in `crates/spur-acp/src/agents/defaults.toml`.
- `ReviewPayload` gains `delegation_plan` + `chosen_matches_dispatched` for reviewer visibility.
- `DelegationRequested` event carries `delegation_plan` for TUI timeline.
- `list_available_workers` MCP tool returns enriched descriptors (tier, description, good_for, avoid_for, output_shape, cost_tier).
- Config lint warnings for oversized `good_for` entries, worker-capable agents without descriptors, capability/descriptor mismatches.

### Changed
- `delegate_to_worker` / `delegate_parallel` / `list_available_workers` MCP tool descriptions expanded with framework guidance.
- `WorkerInfo` struct extended with routing fields (additive; backward-compatible).
