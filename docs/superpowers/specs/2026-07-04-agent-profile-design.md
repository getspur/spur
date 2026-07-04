# Agent Profile — canonical Claude-markdown agent definitions, per-kind materialization & selection

**Status:** draft — design approved in principle (interactive exploration + live ACP probes), pending review and implementation plan
**Date:** 2026-07-04
**Owners:** Kevin Truong (kevin.truong.ds@gmail.com)
**Scope:** Add a per-delegation `profile` so the brain can dispatch a worker *as a named agent persona* — system prompt, description, default model/effort — defined once in Claude's standard agent-markdown format and applied to any worker kind that supports it, e.g. `delegate_to_worker(agent="claude-code", profile="code-reviewer")`. Builds directly on the shipped per-delegation model/effort override (m11, PR #48) and composes with the in-progress generic `config_overrides` (m12).

---

## 1. Goal

Today a worker's *identity* (system prompt, persona, tool posture) is fixed by whatever its binary defaults to; the only levers are the task prompt and one registry entry per variant. Meanwhile every major coding agent has grown a native, file-based agent-definition convention, and the 2026-07-04 live probes (`2026-07-04-agent-profile-acp-probe-results.md`) verified that **selection is already reachable through RPCs SPUR has wired end-to-end**:

- claude-code-acp 0.54.1 advertises an `agent` session config option whose values are the project's `.claude/agents/*.md` — `set_session_config_option("agent", <name>)` verified live.
- opencode exposes `.opencode/agent/*.md` as values of its `mode` config option — verified live.
- kiro surfaces `.kiro/agents/*.json` as ACP session modes — `session/set_mode(<name>)` verified live (and `acp --agent` at spawn).
- codex has `.codex/agents/*.toml` subagents but no ACP selection surface (verified negative, adapters 1.0.2/1.1.0).
- kimi advertises nothing.

This spec adds the smallest end-to-end path that:

1. establishes **one canonical profile format** — Claude agent markdown — stored in `.spur/profiles/<name>.md`,
2. accepts an optional `profile` field on `delegate_to_worker` / `delegate_parallel` / `submit_plan` task entries, carried exactly like m11's `model`/`effort` (structured field, never a beads label),
3. **materializes** the profile into the worker worktree in the worker kind's native format, as an ephemeral git-invisible overlay,
4. **selects** the profile on the fresh worker session via the per-kind strategy verified by the probes, fail-soft.

## 2. Non-goals

- **A new ACP RPC or `_meta` channel.** Everything rides `session/set_config_option`, `session/set_mode`, and files on disk. Probes confirmed `_meta` is not used for this by any adapter.
- **Brain-side `/profile` TUI picker.** Workers only; the brain session is untouched.
- **Semantic translation of tool allowlists across kinds in v1.** The canonical `tools:` frontmatter is carried where the target format has a slot, dropped (with a `debug!` log) where it doesn't. Persona = prompt + defaults, not a cross-tool permission model.
- **Persisting the applied profile on beads labels.** Same rationale as m11 D3: labels are the wrong vehicle. The `[[spur-audit v1]]` dispatch sentinel is the follow-up home if needed.
- **Per-delegation spawn-argv templating.** v1 deliberately selects via existing post-session RPCs so `AgentConfig.effective_args()` stays untouched. Kiro's spawn-time `--agent`/`--model`/`--effort` flags are recorded as a follow-up (they also fix m11's silent no-op on kiro).
- **Prompt-plane emulation for kinds with no surface (kimi).** v1 logs and continues at default; a config-gated prompt-preamble fallback is future work.

## 3. Background — verified substrate

### 3.1 What exists (all file:line refs verified 2026-07-04)

- **Spawn path:** `run_one_worker_attempt` builds `spawn_args = ctx.agent_config.effective_args()` (`worker_attempt.rs:470`), spawns via `build_connection_from_transport`, creates the session with **cwd = worktree path** (`worker_attempt.rs:555-560`), then applies m11 overrides via `apply_model_effort_override` (`worker_attempt.rs:236-303`, invoked at `:571`).
- **Discovery follows session cwd, not process cwd** — probe-verified on claude-agent-acp with process cwd `/tmp` and session cwd elsewhere. Files materialized into the worktree are discovered.
- **Overlay precedent:** task-diff overlays apply at `worker_attempt.rs:389-410` (`extract_overlays` → `apply_overlays`); per-delegation virtual MCP config already rides `new_session` (`worker_mcp_servers`, `:555-560`).
- **Renderer precedent:** the skills installer (`crates/spur-core/src/skills/installer.rs`, `adapters.rs`) already renders one canonical body into `.claude/`, `.kiro/`, `.gemini/`, `.codex/`, `.opencode/`, `.kimi/`, `.cursor/` trees with SPUR-MANAGED markers and sha256 idempotence.
- **Capture hazard (probe-adjacent, code-verified):** `finalize_worker_branch` treats any `git status --porcelain` output as dirty (`manager.rs:1010`) and stages with `git add -A` (`manager.rs:916-940`). A bare untracked injected file would create junk commits and leak into task diffs. `scrub_worktree` is `reset --hard` + `clean -fd` — no `-x` (`manager.rs:503-508`), so *ignored* files survive scrubs.
- **m11 fields:** `DelegationRequest.model` / `.effort` (`delegation_types.rs:116-117`); the apply helper is fail-soft and self-gating on advertised caps.

### 3.2 Why Claude agent markdown as the canonical format

1. **It is the richest and most widely-cloned shape**: YAML frontmatter (`name`, `description`, optional `model`, optional `tools`) + markdown body as system prompt. Kiro's JSON (`name`/`description`/`prompt`), opencode's markdown (`description`/`mode` + body), and codex's TOML (`name`/`description`/`developer_instructions` + optional `model`/`model_reasoning_effort`) are all strict subsets — every target field is derivable, no target needs a field the canonical lacks.
2. **Zero-cost on the strongest target.** For claude workers (and the direct-CLI `claude-code-sj` entry, whose `--agent` flag is documented on claude 2.1.197) the canonical file is written verbatim — no translation, no drift.
3. **Ecosystem gravity.** `.claude/agents/*.md` files are already produced by plugins, marketplaces, and users; adopting the format means SPUR can ingest existing agents unmodified.

### 3.3 Per-kind selection & materialization matrix (probe-verified)

| kind | materialize as | select via | model/effort over ACP |
|---|---|---|---|
| `claude-code-acp` | `.claude/agents/<name>.md` (verbatim) | `set_session_config_option("agent", name)` | ✅ both (0.54.1: `effort` has `thought_level` category) |
| `open-code` | `.opencode/agent/<name>.md` | `set_session_config_option("mode", name)` | ✅ both |
| `kiro` | `.kiro/agents/<name>.json` | `session/set_mode(name)` | ❌ none advertised (spawn flags only — follow-up) |
| `codex-acp` | `.codex/agents/<name>.toml` (subagent, best-effort) | none — skip with `debug!` | ✅ both |
| `kimi` | none | none — skip with `debug!` | ❌ none |
| `claude-stream-json` | `.claude/agents/<name>.md` | follow-up: `--agent` argv (CLI-documented) | CLI flags |

## 4. Design decisions

| # | Question | Choice | Rationale |
|---|---|---|---|
| D1 | Canonical format | **Claude agent markdown**, stored in `.spur/profiles/<name>.md`. | §3.2. Canonical source lives under `.spur/` (like `.spur/skills/`) rather than `.claude/agents/` directly, so SPUR profiles don't pollute the user's interactive claude sessions and the SPUR-managed set is unambiguous. |
| D2 | How the override enters the system | **`profile: Option<String>` on `DelegationRequest` and plan task entries**, mirroring m11 `model`/`effort` exactly. | Same D1–D3 rationale as the m11 spec: label-grammar-safe, schema-discoverable, serde-clean. |
| D3 | Selection mechanism | **Post-session RPCs only in v1** (`set_config_option` / `set_mode`), applied in the same lifecycle slot as m11. | Probe-verified on claude/opencode/kiro; requires zero argv plumbing; one code path, one fail-soft contract. |
| D4 | Materialization vs selection coupling | **Orthogonal.** If `.spur/profiles/<name>.md` exists → materialize + select. If not → select-only pass-through. | The probes showed target tools discover agents from other sources too (user-committed files, plugins). A brain may select `superpowers:code-reviewer` on claude without SPUR owning the definition. |
| D5 | Git invisibility of materialized files | **Per-worktree exclude:** `extensions.worktreeConfig` + `core.excludesFile` in the worktree's `config.worktree`, pointing at a spur-written exclude list enumerating exactly the injected paths. | Ignored files are invisible to `status --porcelain`, `add -A`, and `diff` — neutralizes every leak path in §3.1 with zero changes to `finalize_worker_branch`, and survives `clean -fd` scrubs. Shared `info/exclude` rejected: leaks patterns into the main checkout and races concurrent delegations. |
| D6 | Ordering | **Materialize after `apply_overlays`, before the connection is built.** | Overlay-conflict handling scrubs with `clean -fd`, which would delete not-yet-excluded files; the agent process must find files on disk before session create. |
| D7 | Unknown profile at submit | **Hard error at the MCP tool layer when the profile is neither in `.spur/profiles/` nor explicitly marked pass-through by existing on no side.** Concretely: tool accepts any string; dispatch logs `info!` distinguishing `materialized` vs `pass-through`. Apply-time rejection stays fail-soft. | A typo'd profile silently running the default persona is worse than a rejected dispatch, but hard-failing pass-through selection would break D4. The compromise: validate existence only for the materialization half; selection remains fail-soft with a `warn!` (same contract as m11 D6). |
| D8 | Profile frontmatter `model:` / SPUR-extension `effort:` | **Act as defaults, not overrides:** effective model = request `model` ▸ profile `model` ▸ agent default (same for effort), resolved before the existing m11 helper runs. | One precedence rule, no new apply mechanism; the m11 helper stays the single writer of model/effort. Claude ignores unknown frontmatter keys, so an `effort:` extension keeps the file claude-loadable. |
| D9 | Where the per-kind strategy lives | **`ProfileStrategy` derived from `AgentKind` with optional `[agents.entries.profile]` config override** (`select = "config_option:agent" \| "config_option:mode" \| "session_mode" \| "none"`, `materialize = "claude_md" \| "opencode_md" \| "kiro_json" \| "codex_toml" \| "none"`). | Defaults encode the probe matrix; config override absorbs upstream adapter changes (e.g. codex gaining an `agent` option) without a code release — consistent with how `AgentConfig` already declares permissions/commands wiring. |
| D10 | License/feature gate | **No.** | Mirrors m11 D8: no new RPC, no new cost surface. |

## 5. Architecture

### 5.1 Data flow

```
brain                                spur-core                                   worker agent
─────                                ─────────                                   ────────────
delegate_to_worker(
  agent="claude-code",               DelegationRequest {
  profile="code-reviewer",   ──────►   profile: Some("code-reviewer"),
  model=None, effort=None,             model/effort: None, ... }
  task=...)                          │
                                     ▼
                                     execute_delegation → WorkerAttemptCtx { profile, ... }
                                     │
                                     ▼
                                     run_one_worker_attempt
                                       create worktree → apply_overlays        (existing)
                                       ── NEW: materialize_profile(kind, profile)
                                             .spur/profiles/code-reviewer.md
                                               → <worktree>/.claude/agents/code-reviewer.md
                                             + per-worktree exclude entry
                                       spawn → initialize → new_session(cwd=worktree)
                                       ── m11 (extended): apply_session_overrides(
                                             profile, model, effort)
                                             1. select profile  (per ProfileStrategy)
                                             2. model/effort    (existing helper,
                                                D8 precedence pre-resolved)
                                       drive_prompt_notifications  ◄── prompt runs as persona
```

### 5.2 Touchpoints

1. **`crates/spur-core/src/delegation_types.rs`** — add `profile: Option<String>` to `DelegationRequest`.
2. **`crates/spur-core/src/mcp/delegation.rs` + `tool_schemas.rs`** — accept/document optional `profile` on `delegate_to_worker`, `delegate_parallel.tasks[]`, `submit_plan.tasks[]`.
3. **`crates/spur-core/src/profiles/` (NEW module)** — canonical parser (frontmatter + body, reusing `skills/frontmatter.rs` machinery) and per-kind renderers (`claude_md` verbatim, `opencode_md`, `kiro_json`, `codex_toml`), each emitting the SPUR-MANAGED marker.
4. **`crates/spur-worktree/src/manager.rs`** — `add_worktree_excludes(worktree, paths)`: enables `extensions.worktreeConfig` (idempotent, repo-level once) and writes the per-worktree exclude file.
5. **`crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`** — materialization call between overlay apply and connection build; generalize `apply_model_effort_override` → `apply_session_overrides` adding the profile-selection arm (config-option or set-mode per strategy), profile first, then model/effort.
6. **`crates/spur-acp/src/config/mod.rs`** — optional `[agents.entries.profile]` block (D9); `AgentKind → ProfileStrategy` defaults.
7. **Plan path** — `server/types.rs` task entries, `plan_builder.rs` (labels unchanged), `plan/reconciler/mod.rs` threading, `submit_plan_mutation` field rewrite: identical shape to m11's plumbing.

### 5.3 Boundary discipline

| Crate | Change |
|---|---|
| `spur-acp` | `ProfileStrategy` type + config block only. No connection/RPC changes — reuses `set_session_config_option` and `set_session_mode`. |
| `spur-core` | Owns profile parsing/rendering, request field, apply logic. |
| `spur-worktree` | Owns the exclude mechanism. |
| `spur-mcp`, `spur-tui` | No change (TUI surfacing of applied profile = follow-up with the m11 TUI follow-up). |

## 6. Failure modes & mitigations

| Failure | Mitigation |
|---|---|
| Worker rejects profile selection (unknown value) | Fail-soft `warn!`; worker runs default persona; model/effort still applied. Kiro precedent: unknown `--agent` fell back to `kiro_default` gracefully. |
| Kind has no selection surface (codex, kimi) | `ProfileStrategy::select = none` → `debug!` skip. Codex still gets the `.codex/agents/*.toml` materialized (subagent best-effort). |
| Materialized file would collide with a committed agent of the same name | Refuse to overwrite non-SPUR-MANAGED files (installer contract, `installer.rs:58`); select-only against the committed definition and `warn!`. |
| Exclude setup fails (ancient git, locked config) | Abort materialization for that delegation (`warn!`), continue select-only. Never inject non-excluded files — the §3.1 leak paths make that strictly worse than no persona. |
| Worker force-adds an excluded file (`git add -f`) | Accepted residual risk; workers have no instruction to do so. Finalize-side pathspec stripping listed as hardening follow-up. |
| Profile parse error at dispatch | Hard error before worktree creation (cheap, deterministic, brain-visible). |
| Adapter drift (e.g. claude renames `agent` option) | Selection self-gates on the advertised option id from `NewSessionResponse` (same predicate pattern as `SpurAgentCaps`); miss → fail-soft + `warn!` naming the expected id. |

## 7. Testing strategy

1. **Renderer unit tests** — golden-file per kind from one canonical fixture; idempotence via SPUR-MANAGED sha256 (mirror `skills/adapters.rs` tests).
2. **Exclude behavior** — worktree test: inject + exclude → `worktree_dirty() == false`, `finalize_worker_branch` returns `NoOp` for an idle worker, injected paths absent from `collect_diff` and from a squashed commit after real worker edits.
3. **Apply unit tests** — recording-connection tests per strategy: claude kind → one `session/set_config_option {configId:"agent"}` before any model/effort call; kiro kind → one `session/set_mode`; codex/kimi → no selection RPC; rejection → `warn!` + model/effort still attempted.
4. **Precedence (D8)** — request model beats profile model beats none.
5. **Plan persistence** — `labels::agent` untouched with `profile` present; reconciler round-trips the field across retries.
6. **Live wire probes** — keep `/tmp`-style ndjson probes as a documented manual procedure against pinned adapter versions on bump (probe doc §Raw evidence); optionally automate as ignored integration tests gated on binary availability.

## 8. Out of scope / future work

- **Kiro spawn-flag mapping** (`--agent`/`--model`/`--effort`) via a per-kind argv template — also the fix for m11's silent no-op on kiro.
- **`claude-code-sj` `--agent` argv support** for the stream-json transport.
- **Prompt-plane fallback** for kinds with no surface (kimi), config-gated.
- **Audit sentinel extension** — record applied `profile` (and m11 model/effort) on the `[[spur-audit v1]]` dispatch record.
- **TUI session-detail surfacing** of the applied persona.
- **`spur profiles` CLI** — list/validate/import (e.g. ingest an existing `.claude/agents/*.md` into `.spur/profiles/`).
- **Seed bump** — `seed_agents.toml` still pins claude-agent-acp 0.33.1 and deprecated `@zed-industries/codex-acp`; live config runs 0.54.1 / `@agentclientprotocol/codex-acp@1.0.2`.

## 9. Glossary

- **profile** — a named agent persona in canonical Claude agent-markdown, materialized and/or selected per delegation.
- **materialize** — render the canonical profile into the worker kind's native agent-definition file inside the worker worktree, git-excluded.
- **select** — make the fresh worker session run *as* the profile, via the kind's verified surface.
- **pass-through selection** — selecting a profile name SPUR does not manage (D4).
- **ProfileStrategy** — per-kind (materialize, select) pair, defaulted from `AgentKind`, overridable in config (D9).

## 10. References

- Probe evidence: `docs/superpowers/specs/2026-07-04-agent-profile-acp-probe-results.md` (this spec's per-kind matrix is lifted from it verbatim).
- Precedent spec & plumbing shape: `docs/superpowers/specs/2026-07-04-per-delegation-model-effort-override-design.md` (m11, shipped as PR #48).
- Renderer/marker precedent: `crates/spur-core/src/skills/installer.rs`, `adapters.rs`.
- Capture hazard: `crates/spur-worktree/src/manager.rs` (`finalize_worker_branch`, `collect_diff`, `scrub_worktree`).
- Codex subagents: https://developers.openai.com/codex/subagents (`.codex/agents/*.toml`; no ACP surface as of adapters 1.0.2–1.1.0).
