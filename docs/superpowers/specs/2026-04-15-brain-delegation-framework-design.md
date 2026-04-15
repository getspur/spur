# Brain Delegation Framework — Design

**Status:** design
**Date:** 2026-04-15
**Reference specs:**
- `docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md` (hard dependency, Change 1)
- `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md` (sub-table conventions)
- `docs/superpowers/specs/2026-04-14-agent-config-foundation-design.md` (Spec 1, shares lint API surface)

**Area:** `spur-acp` config + defaults · `spur-core` orchestrator brain prompt · `spur-mcp` tool surface · `spur-acp` domain types

## Problem

Today, the brain-agent side of delegation is under-specified across five axes:

1. **Trigger decisions** — when to delegate vs. do-it-yourself. Today's brain prompt in `orchestrator.rs:1835-1846` is five lines of prose; there's no structured dispatch procedure.
2. **Routing decisions** — which worker agent fits which task. `list_available_workers` returns only agent names. No capability data, no specialties, no anti-patterns.
3. **Task-prompt quality** — workers receive under-specified tasks because there's no enforced shape (scope / constraints / expected output).
4. **Decomposition** — the brain doesn't parallelize; calls `delegate_to_worker` sequentially when `delegate_parallel` would work.
5. **Planning discipline** — the brain fires on its first idea; no "enumerate candidates → score → commit" loop.

The parallel spec `brain-worker-refinement-design.md` enriches the feedback loop *from* workers *to* the brain (richer `DelegationResult`, retry history, `brain_session_id` threading). It does not touch the dispatch side. This spec closes the other half of the loop.

### Landscape snapshot

- `crates/spur-mcp/src/tools.rs` exposes 8 MCP tools, including `delegate_to_worker`, `delegate_parallel`, `list_available_workers`.
- `crates/spur-core/src/orchestrator.rs:1831-1869` assembles the brain prompt. Three spawn sites (`:292, :1183, :1336`) populate `WorkerInfo` with name-only entries.
- `crates/spur-acp/src/config/` parses `[[agents.entries]]` blocks. Known sub-tables: `commands`, `mentions`, `permissions`, `capabilities`, `display`.
- The roadmap's cultural rule — "config declares CHOICES between built-in code hooks; config does not describe TRANSFORMATIONS" — is preserved here: descriptors are data, not transformations.

## Goals

1. **Structured per-agent capability descriptors** readable by both prompt assembly and the `list_available_workers` tool, with built-in defaults for known agents and per-field user override.
2. **A rewritten brain prompt** that lists available workers, states a dispatch decision procedure, requires (soft) a structured `delegation_plan` on every delegation call, and prescribes a CONTEXT/GOAL/CONSTRAINTS/EXPECTED_OUTPUT shape for worker task prompts.
3. **A `delegation_plan` structured MCP tool parameter** on `delegate_to_worker` / `delegate_parallel` — race-free structured capture, reviewer-visible, never-blocking.
4. **A config validator** emitting warnings (not errors, in v1) for oversized `good_for` entries, missing descriptions on worker-capable agents, and capability/descriptor contradictions.
5. **Safe rollout via feature flag** (`[brain.delegation] framework`) with build-aware default: dev-builds default `v1`, release-builds default `legacy` at ship, flipping across 3 releases.

## Non-goals

- **Hard procedural enforcement.** The brain is advised, not constrained. Schema validation on `delegation_plan` stays permissive. (User chose C+D posture during brainstorm.)
- **Learned routing.** Tracking per-(task-pattern, agent) success rates and feeding them back into prompts. Phase 2 — needs embeddings/similarity + review-gate success signal + persistence.
- **Cost-budgeted dispatch.** Orchestrator injecting running session cost into every routing decision. Orthogonal spec.
- **`expected_output` as first-class MCP tool field.** `delegation_plan.rationale` + `output_shape` inlined into the task string cover this. Flagged in `brain-worker-refinement` non-goals too.
- **Nested-delegation lineage.** Brain → worker → sub-worker. Pre-existing concern; no new surface here.
- **Model-specific prompt flavors.** Single shared prompt across claude / kiro / codex / gemini for v1. Measure first, specialize later.
- **Blocking-semantics changes.** `delegate_to_worker` remains blocking; no streaming partial results.
- **Async / non-blocking delegation.** Already a `brain-worker-refinement` Phase 2 item.
- **Metrics emission.** No Prometheus counters; observability via structured logs + events.
- **TUI UI for editing descriptors.** Config-only.
- **Runtime config hot-reload.** Matches roadmap non-goal.

## Design

Five parts: (A) data model + defaults, (B) brain prompt, (C) MCP tool surface, (D) validator, (E) rollout. Four code changes plus documentation. No existing fields removed; no method signatures re-ordered.

---

### Part A — Data model and built-in defaults

#### A.1 New sub-table: `[agents.entries.delegation]`

Eight optional fields; all omittable. Missing fields fall back to a built-in default when the agent name matches; otherwise the field stays empty.

```toml
[agents.entries.delegation]
description = "Generalist coding agent; strong at greenfield + refactors."
tier        = "generalist"          # "specialist" | "generalist"
good_for    = [
  "multi-file refactors",
  "writing new modules from spec",
  "test authoring",
]
avoid_for   = [
  "kiro vendor-ext command invocation",
]
strengths           = ["long-context reasoning", "diff-shaped output"]
limitations         = ["no network access beyond allowlisted tools"]
input_expectations  = "Provide acceptance criteria + file allowlist when scope > 3 files."
output_shape        = "Unified diff + summary paragraph + test plan bullets."
inherit_defaults    = true          # default true; set false to suppress built-in merge
```

**Field roles (critical distinction):**

- **Routing-relevant** (injected into the per-session system prompt as a one-liner per agent): `description`, `tier`, `cost_tier` (pre-existing).
- **Routing-relevant via `list_available_workers`** (brain fetches on demand): `good_for`, `avoid_for`, `output_shape`.
- **Task-shaping only** (injected into the per-dispatch worker task prompt, never into routing): `strengths`, `limitations`, `input_expectations`.

This split bounds the per-session system prompt at ≤2 KB even with 20 agents configured.

#### A.2 Rust types

Location: `crates/spur-acp/src/config/` (wherever `AgentConfig` lives).

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DelegationDescriptor {
    pub description:        Option<String>,
    pub tier:               Option<Tier>,
    pub good_for:           Vec<String>,
    pub avoid_for:          Vec<String>,
    pub strengths:          Vec<String>,
    pub limitations:        Vec<String>,
    pub input_expectations: Option<String>,
    pub output_shape:       Option<String>,
    #[serde(default = "inherit_defaults_default")]
    pub inherit_defaults:   bool,
}

fn inherit_defaults_default() -> bool { true }

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier { Specialist, Generalist }

// AgentConfig gains:
//     pub delegation: DelegationDescriptor,
// with #[serde(default)] so pre-existing configs keep parsing.
```

#### A.3 Built-in defaults — bundled TOML

Defaults live in **TOML**, not Rust literals. Contributors tune descriptors via diff-friendly edits.

**File:** `crates/spur-acp/src/agents/defaults.toml` (new)

```toml
[claude-code-acp]
description = "Generalist coding agent; strong at greenfield + refactors."
tier        = "generalist"
good_for    = ["multi-file refactors", "writing new modules from spec",
               "test authoring", "code review with rationale"]
avoid_for   = ["kiro vendor-ext command invocation"]
strengths   = ["long-context reasoning", "diff-shaped output"]
limitations = ["no network access beyond allowlisted tools"]
input_expectations = "Provide acceptance criteria + file allowlist when scope > 3 files."
output_shape       = "Unified diff + summary paragraph + test plan bullets."

[claude-code]
# Same descriptor as claude-code-acp; stream-json variant, deprecated-preferred.
# (inherits via alias map below)

[kiro]
description = "Specialist agent for Kiro spec-driven workflows and vendor commands."
tier        = "specialist"
good_for    = ["/spec-init, /spec-plan, /spec-execute tasks",
               "work requiring Kiro's internal spec schema"]
avoid_for   = ["tasks outside Kiro's spec/command workflow",
               "large refactors with no spec artifact"]
strengths   = ["structured spec output", "vendor-ext command integration"]
output_shape = "Spec artifact + next-step suggestions."

[codex]
description = "Low-cost generalist; strong at narrowly-scoped edits."
tier        = "generalist"
good_for    = ["single-file edits", "syntactic refactors",
               "translating between language idioms"]
avoid_for   = ["multi-file coordination", "architectural decisions"]
output_shape = "Unified diff + one-sentence rationale."

[gemini]
description = "Generalist agent with strong multi-modal support."
tier        = "generalist"
good_for    = ["tasks involving images or diagrams",
               "exploratory analysis where context is ambiguous"]
output_shape = "Narrative analysis + action items."
```

**Loader** (`crates/spur-acp/src/agents/defaults.rs`):

```rust
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULTS_TOML: &str = include_str!("defaults.toml");

static DEFAULTS: OnceLock<HashMap<String, DelegationDescriptor>> = OnceLock::new();

pub fn builtin_descriptor(agent_name: &str) -> Option<DelegationDescriptor> {
    let map = DEFAULTS.get_or_init(|| {
        toml::from_str::<HashMap<String, DelegationDescriptor>>(DEFAULTS_TOML)
            .expect("defaults.toml must parse")
    });
    // Alias: claude-code → claude-code-acp descriptor.
    let key = match agent_name {
        "claude-code" => "claude-code-acp",
        other         => other,
    };
    map.get(key).cloned()
}

pub fn known_agents() -> Vec<&'static str> {
    vec!["claude-code-acp", "claude-code", "kiro", "codex", "gemini"]
}
```

Build-time test asserts the TOML parses and each known agent resolves with `description` + `tier` + non-empty `good_for` + `output_shape`.

#### A.4 Merge logic

```rust
pub fn apply_builtin_defaults(cfg: &mut AgentConfig) {
    if !cfg.delegation.inherit_defaults { return; }
    let Some(default) = builtin_descriptor(&cfg.name) else { return };
    let user = &mut cfg.delegation;
    user.description        = user.description.take().or(default.description);
    user.tier               = user.tier.or(default.tier);
    if user.good_for.is_empty()    { user.good_for    = default.good_for; }
    if user.avoid_for.is_empty()   { user.avoid_for   = default.avoid_for; }
    if user.strengths.is_empty()   { user.strengths   = default.strengths; }
    if user.limitations.is_empty() { user.limitations = default.limitations; }
    user.input_expectations = user.input_expectations.take().or(default.input_expectations);
    user.output_shape       = user.output_shape.take().or(default.output_shape);
    tracing::info!(agent = %cfg.name, "applied built-in delegation descriptor");
}
```

Semantics:
- **Per-field override** — users replace any subset without restating others.
- **Empty vec means inherit** (when `inherit_defaults = true`). Rare "truly empty vec" case uses `inherit_defaults = false`.
- **Idempotent** — calling twice is a no-op.

#### A.5 Thin synthesis for unknown agents

When `builtin_descriptor` returns `None` AND user config leaves the block fully empty:

```rust
cfg.delegation.description = Some(format!("{} agent (no descriptor configured)", cfg.name));
cfg.delegation.tier = Some(Tier::Generalist);
// good_for / avoid_for stay empty
```

Such agents are **excluded from the brain prompt's workers block** (see B.2) but remain callable via `list_available_workers` — the synthesized description is visible there. Prevents dead-weight in every session's system prompt.

---

### Part B — Brain prompt rewrite

Replaces today's 5-line prose at `orchestrator.rs:1835-1846`. Prompt assembly is deterministic from config — same inputs produce byte-identical output.

#### B.1 Prompt assembly pipeline

```
1. Header
2. Available workers block (one-liner per agent)
3. Dispatch decision procedure
4. Delegation plan requirement (scaled content by triggers)
5. Task prompt structure template
6. Canonical example (ONE, positive)
7. Issue context        — unchanged from today
8. Project context      — unchanged from today (config.brain.prompt.append)
9. Task                 — unchanged from today
```

Blocks 1-6 are deterministic from config. `build_brain_prompt` decomposes into named helpers (`render_header`, `render_workers_block`, `render_dispatch_procedure`, `render_plan_requirement`, `render_task_structure`, `render_canonical_example`), each ≤50 LoC. No function body exceeds 80 LoC.

The full assembled prompt is written once per session to `.spur/logs/brain-prompts/{session_id}.md`. `.spur/logs/` is already in `.gitignore`; LRU eviction keeps the directory ≤50 MB.

#### B.2 Workers block (one-liner per agent)

Iterates `agents.worker_capable()`. Excludes agents with **empty `good_for`** (covers both thin-synthesized unknowns from A.5 and user-declared-empty-with-`inherit_defaults = false`). Renders:

```
## Available worker agents

### claude-code-acp  (generalist, cost: medium)
Generalist coding agent; strong at greenfield + refactors.

### kiro  (specialist, cost: medium)
Specialist agent for Kiro spec-driven workflows and vendor commands.

### codex  (generalist, cost: low)
Low-cost generalist; strong at narrowly-scoped edits.
```

Cap: ~100 chars/agent. 20 agents → ~2 KB. Full `good_for`, `avoid_for`, `output_shape` are NOT in this block — brain fetches them via `list_available_workers` when routing is ambiguous.

#### B.3 Dispatch decision procedure

```
## When to delegate vs. do it yourself

Do it yourself when:
  - The task is <15min of work.
  - You need tight iterative control (probe, edit, probe).
  - The task requires your accumulated session context.
  - No worker's good_for meaningfully matches.

Delegate when:
  - Subtasks are independent and parallelizable (use delegate_parallel).
  - A worker's good_for directly matches the task shape.
  - Scope (LoC, files, or duration) exceeds what you want to spend your
    context window on.
  - You need fresh context isolation.

Routing rule: prefer specialist tier when good_for matches exactly;
fall back to generalist tier otherwise. avoid_for is a SOFT signal —
you MAY override it with a stated rationale when no better agent exists.

Your <delegation_plan> replaces, does not supplement, other planning
artifacts you would emit FOR DELEGATION DECISIONS. Native planning
tools (Todo, plan mode, etc.) remain for intra-task work.
```

#### B.4 Delegation plan requirement

```
## Required: delegation_plan parameter

Every delegate_to_worker and delegate_parallel call MUST include a
`delegation_plan` argument. Content scales with complexity:

For ≥2 subtasks OR >3 files touched — pass the full shape:
  {
    "candidates":    [{"agent": "...", "rationale": "..."}, ...],
    "decomposition": [{"subtask": "...", "parallelizable_with": ["..."]}],
    "chosen":        "agent-name-or-self-or-parallel",
    "rationale":     "Why this choice beats the alternatives. If
                     violating any agent's avoid_for, state why."
  }

For trivial single-step delegations — minimum shape:
  { "chosen": "agent-name", "rationale": "short justification" }

All fields are advisory; the orchestrator accepts the tool call
even with minimal or missing content. Your rationale is surfaced
to the review gate so human / automated reviewers can see what you
decided and why.

If you have access to a sequential-thinking MCP tool, use it to
generate the candidates and decomposition before committing to the
delegate_* call.
```

#### B.5 Task prompt structure template

```
## Task prompt structure (what to send workers)

Structure the `task` field of delegate_to_worker as:

  CONTEXT: {scope, constraints from this session, relevant file paths}
  GOAL:    {one-sentence success criterion}
  CONSTRAINTS: {what the worker must NOT do}
  EXPECTED OUTPUT: {populated from the chosen agent's output_shape
                   when declared}

For agents with declared output_shape, EXPECTED OUTPUT MUST restate
it. For agents with declared input_expectations, CONTEXT MUST satisfy
those expectations before dispatch. The orchestrator injects the
chosen agent's input_expectations and output_shape into this message
block at the point of dispatch — you see them then.
```

#### B.6 Canonical example (ONE positive)

A single worked example showing a multi-file refactor: narrative reasoning, full `delegation_plan`, matching `delegate_to_worker` call with CONTEXT/GOAL/CONSTRAINTS/EXPECTED_OUTPUT. ~500 tokens. Anchors the pattern without over-constraining.

#### B.7 Token accounting

| Block | Tokens |
|---|---|
| Header | ~80 |
| Workers block (15 agents × 100 chars) | ~400 |
| Dispatch procedure | ~250 |
| Delegation plan requirement | ~300 |
| Task prompt structure | ~150 |
| Canonical example | ~500 |
| **System-prompt overhead (once per session)** | **~1,700 tokens** |

Per-dispatch: `delegation_plan` tool-param ~200-400 tokens. Per-session with 10 delegations: ~5 KB total framework overhead. Negligible on 128 K-context models.

---

### Part C — MCP tool surface changes

#### C.1 Enriched `WorkerInfo` returned by `list_available_workers`

Location: `crates/spur-mcp/src/server.rs:86`.

```rust
pub struct WorkerInfo {
    pub name:         String,
    pub tier:         Option<String>,        // "specialist" | "generalist"
    pub description:  Option<String>,
    pub good_for:     Vec<String>,
    pub avoid_for:    Vec<String>,
    pub output_shape: Option<String>,
    pub cost_tier:    Option<String>,
}
```

Populated at the three orchestrator spawn sites (`:292, :1183, :1336`) via a shared helper in `spur-acp`:

```rust
pub fn build_worker_info(cfg: &AgentConfig) -> WorkerInfo { ... }
```

DRY; prevents triplicate drift.

Tool response JSON is purely additive — old consumers ignore unknown fields.

#### C.2 Tool description upgrades (`crates/spur-mcp/src/tools.rs`)

Descriptions refresh into the brain's context every turn — load-bearing defense against system-prompt attention decay across long sessions.

**`delegate_to_worker`:**

> "Delegate a task to a worker agent. Blocks until the worker completes. Pass a `delegation_plan` parameter with at minimum `{chosen, rationale}` (more for multi-step work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT. Use `list_available_workers` when routing is ambiguous."

**`delegate_parallel`:**

> "Delegate multiple tasks in parallel. Blocks until all complete. The `delegation_plan.decomposition` field MUST demonstrate that subtasks are independent — no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker` serially."

**`list_available_workers`:**

> "Returns tier, description, good_for, avoid_for, output_shape, and cost_tier for each worker. Call when the system-prompt one-liner is insufficient."

#### C.3 `delegation_plan` structured tool parameter

Added to both `delegate_to_worker` and `delegate_parallel` input schemas. Permissive — all nested fields optional. No schema enforcement of content length or structure; brain may pass minimal `{chosen, rationale}` for trivial dispatches.

```json
"delegation_plan": {
  "type": "object",
  "properties": {
    "candidates": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "agent":     {"type": "string"},
          "rationale": {"type": "string"}
        }
      }
    },
    "decomposition": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "subtask":             {"type": "string"},
          "parallelizable_with": {"type": "array", "items": {"type": "string"}}
        }
      }
    },
    "chosen":    {"type": "string"},
    "rationale": {"type": "string"}
  }
}
```

#### C.4 `DelegationPlan` struct

Location: `crates/spur-acp/src/domain/delegation.rs` (co-located with `DelegationRequest`).

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DelegationPlan {
    pub candidates:    Vec<PlanCandidate>,
    pub decomposition: Vec<PlanSubtask>,
    pub chosen:        Option<String>,
    pub rationale:     Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanCandidate {
    pub agent:     Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanSubtask {
    pub subtask:             Option<String>,
    pub parallelizable_with: Vec<String>,
}
```

`DelegationRequest` gains `pub delegation_plan: Option<DelegationPlan>`. Stamped by the 8 handlers in `spur-mcp/src/server.rs` (alongside `brain_session_id` per `brain-worker-refinement` Change 1).

#### C.5 ReviewPayload extension

Location: `crates/spur-acp/src/domain/review.rs` (or current ReviewPayload home).

```rust
pub struct ReviewPayload {
    // pre-existing fields including brain-worker-refinement's diff_summary
    pub delegation_plan:           Option<DelegationPlan>,
    pub chosen_matches_dispatched: Option<bool>,
}
```

Populated at the review-gate call site in `execute_delegation`.

#### C.6 Mismatch detection

Before the review gate fires, the orchestrator compares `plan.chosen` against the dispatched agent:

```rust
let normalized_chosen = plan.as_ref()
    .and_then(|p| p.chosen.as_ref())
    .map(|c| normalize_agent_name(c));
let normalized_dispatched = normalize_agent_name(&request.agent);
let chosen_matches_dispatched = normalized_chosen
    .as_deref()
    .map(|c| c == normalized_dispatched);

if chosen_matches_dispatched == Some(false) {
    tracing::warn!(
        session = %request.brain_session_id,
        chosen = %plan.as_ref().and_then(|p| p.chosen.as_deref()).unwrap_or(""),
        dispatched = %request.agent,
        "delegation_plan.chosen does not match dispatched agent"
    );
}
```

Never blocks. Observable via log + `ReviewPayload.chosen_matches_dispatched`.

`normalize_agent_name`: lowercase, trim, strip `-acp` / `_acp` / `-cli` / `_cli` suffixes. Unit-tested with a (input, expected) table.

#### C.7 Event schema extension

`DelegationRequested` event body gains `pub delegation_plan: Option<DelegationPlan>` for TUI timeline visibility. Additive; funnel and broadcast contracts unchanged.

---

### Part D — Validator and config hygiene

#### D.1 Public API

Location: `crates/spur-acp/src/agents/defaults.rs`.

```rust
pub enum LintLevel { Warn, Error }

pub struct LintMessage {
    pub level:   LintLevel,
    pub agent:   String,
    pub message: String,
}

pub fn validate_delegation_config(cfgs: &[AgentConfig]) -> Vec<LintMessage> { ... }
```

Called once during config load, after `apply_builtin_defaults`.

#### D.2 Lint checks (four, all Warn in v1)

| # | Check | Message |
|---|---|---|
| 1 | Length-lint: any `good_for[i]` or `avoid_for[i]` with `.chars().count() > 80` | `"agent '{name}': good_for[{i}] exceeds 80 chars; use a short task pattern, not a sentence"` |
| 2 | Worker-capable AND `description.is_none()` AND no built-in default applied | `"agent '{name}': worker-capable but has no delegation.description — routing will be weak"` |
| 3 | Worker-capable AND `good_for.is_empty()` AND no built-in default applied | `"agent '{name}': worker-capable but no delegation.good_for entries — brain has no positive routing signal"` |
| 4 | `good_for` string contains a capability keyword (`plan_mode`, `planning`, `usage`, `load_session`, `list_sessions`, `session_resume`) but `capabilities` does not declare the corresponding token | `"agent '{name}': delegation.good_for references {keyword} but capabilities does not declare {token}"` |

Check 4 uses a keyword→capability-token table in `defaults.rs`. **Maintenance note**: when `AgentConfig.capabilities` gains a new recognized token, update this table. Documented as explicit contributor maintenance item.

Dropped from an earlier draft as low-value / high false-positive: tier/good_for length coherence and duplicate detection.

#### D.3 Integration points

- **Startup surfacing**: after config load, iterate `validate_delegation_config(&cfgs)` and emit each message via `tracing::warn!`. Not `eprintln!` — flows through existing log sinks and avoids corrupting the TUI.
- **`spur config check` subcommand**: shipped separately by `agent-config-foundation-design.md` (Spec 1). This spec exports the API; Spec 1 consumes it. No hard ordering dependency.

#### D.4 Tests (in `defaults.rs`)

```
fn builtin_defaults_parses_and_covers_known_agents()
fn apply_builtin_defaults_per_field_override()
fn apply_builtin_defaults_empty_vec_inherits()
fn apply_builtin_defaults_inherit_false_keeps_empty()
fn apply_builtin_defaults_unknown_agent_thin_synthesis()
fn lint_flags_oversized_good_for()
fn lint_flags_worker_without_description()
fn lint_flags_capability_mismatch()
fn lint_clean_config_produces_no_warnings()
```

---

### Part E — Rollout

#### E.1 Three-phase deploy

1. **Phase 1 — A + D.** Ship data model, defaults, validator, merge logic. No user-visible brain behavior change; pre-existing configs continue to parse and run identically.

2. **Phase 2 — C.** Ship `WorkerInfo` enrichment, tool-description upgrades, `delegation_plan` tool-param schema, `ReviewPayload` extensions, mismatch detection, event schema extension. Tool descriptions change the brain's per-turn context; behavior shifts are *additive* (brain that ignores new guidance behaves as before). Phase 2 is safely revertless — no flag needed.

3. **Phase 3 — B.** Ship the rewritten brain prompt behind the feature flag. The prompt is a *replacement*, not additive — old behavior is preserved only by staying on the flag.

Each phase is independently shippable, reviewable, and revertable.

#### E.2 Feature flag

```toml
[brain.delegation]
framework = "v1"      # or "legacy"
```

**Build-aware default:**

```rust
#[cfg(debug_assertions)]
const DEFAULT_FRAMEWORK: &str = "v1";
#[cfg(not(debug_assertions))]
const DEFAULT_FRAMEWORK: &str = "legacy"; // at v1-release-ship
```

**Lifecycle (3 releases):**

| Release | Dev default | Release-build default | State |
|---|---|---|---|
| v1 ship | `v1` | `legacy` | Maintainers dogfood `v1`; users opt-in |
| v2 ship | `v1` | `v1` | Production default flips |
| v3 ship | — | — | Flag removed; `legacy` path deleted |

`config.toml.example` documents both values with commentary. Conservative blast radius; internal telemetry via dogfooding feeds the v2 flip decision.

#### E.3 Dependency: land order

This spec's Phase 1 merges **after** `brain-worker-refinement-design.md` Change 1 (the `brain_session_id` threading) has merged. Rationale: both specs modify `DelegationRequest` in `spur-mcp/src/tools.rs` and the 8 handlers in `spur-mcp/src/server.rs`. Landing `brain_session_id` first means this spec's diff is purely additive (`delegation_plan: Option<DelegationPlan>`), no rebase thrash.

Further: `brain_session_id` is a conceptual precondition — `delegation_plan` needs to be attributable to the originating brain, which requires the session ID already threaded through.

#### E.4 Documentation

| Doc | Change |
|---|---|
| `docs/spur/agent-config.md` | New `[delegation]` sub-table section documenting all 9 fields + `inherit_defaults` + worked examples |
| `docs/spur/brain-worker-architecture.md` | Update the brain-side dispatch section to describe the framework |
| `.spur/config.toml.example` | Add commented `[delegation]` block to each agent entry |
| `docs/spur/contributing-agent-defaults.md` | New; how to tune `defaults.toml`, when to cut a release |
| Changelog | Entry for each phase |

## Testing

### Unit tests

| Test | Location |
|---|---|
| `builtin_defaults_parses_and_covers_known_agents` | `spur-acp/src/agents/defaults.rs` |
| `apply_builtin_defaults_*` (5 variants) | same |
| `lint_*` (4 variants + clean-config control) | same |
| `build_worker_info_merges_config_and_defaults` | `spur-acp/src/agents/` |
| `normalize_agent_name_*` table-driven | `spur-core/src/orchestrator.rs` |
| `delegation_plan_deserializes_from_partial_json` | `spur-acp/src/domain/delegation.rs` |

### Integration tests (use MockBrain, not real subprocess)

| Test | What it asserts |
|---|---|
| `delegation_plan_param_threaded_to_review_payload` | Brain passes structured plan → `ReviewPayload.delegation_plan = Some(...)` |
| `missing_delegation_plan_logs_warn_not_blocks` | Brain omits param → `None` + warn emitted, dispatch succeeds |
| `chosen_mismatch_detected_not_blocked` | Plan's `chosen` ≠ dispatched agent → `chosen_matches_dispatched = Some(false)`, warn, no block |
| `list_available_workers_returns_enriched_descriptor` | Response contains `tier`, `description`, `good_for`, `avoid_for`, `output_shape`, `cost_tier` |
| `brain_prompt_snapshot_deterministic` | Fixed fixture config + fixed task → byte-identical assembled prompt. No timestamps, no SessionIds in prompt body |
| `empty_descriptor_agent_excluded_from_block_only` | Fully-empty agent → absent from prompt's workers block, present in `list_available_workers` |

Harness: `MockBrain` is a test fixture that speaks MCP directly to the callback server. Deterministic and CI-friendly.

### Nightly smoke (real brain)

1-2 scripted real-brain flows against `claude-code-acp`, asserting that `ReviewPayload.delegation_plan` is populated. Run nightly; not gating on PR.

### Routing evaluation corpus

`test/fixtures/delegation-routing/*.yaml`:

```yaml
task: "Refactor auth module to use new session format across 4 files"
config: fixtures/configs/standard.toml
expected_agents: ["claude-code-acp", "codex"]   # either acceptable
```

Mini harness invokes MockBrain with assembled prompt, captures the first `delegate_to_worker` call's `agent` argument, checks membership. Target: ≥7/10 exact-agent matches.

### Manual TUI smoke

1. Start spur with default config. Verify `.spur/logs/brain-prompts/{session_id}.md` appears.
2. Ask a kiro-fitting task (`/spec-plan ...`). Observe `delegation_plan.chosen == "kiro"` in the DelegationRequested event.
3. Craft a deliberate avoid_for violation. Observe routing behavior + stated rationale.
4. Malform a `good_for` entry (100-char sentence). Start spur. Observe `tracing::warn!` in log output.

## Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Brain shortcut — passes `{chosen, rationale: "ok"}` with no substance | Medium | Review-gate sees `delegation_plan`; collect data; harden in Phase 2 if observed frequently |
| Built-in default drifts from actual agent behavior | Low–Medium | Coarse defaults age better; contributor process in `contributing-agent-defaults.md`; version bump checklist |
| Prompt too long, degrades weaker-model performance | Low | 2 KB cap on workers block; scaled `delegation_plan` content; feature flag allows fallback to legacy |
| User configures 20+ worker agents → bloated `list_available_workers` response | Low | Tool response is on-demand, not injected every turn |
| `DelegationPlan` schema evolution breaks older brains | Low | All fields optional; schema additive; no breakage |
| Capability keyword table in validator drifts from actual `capabilities` tokens | Low | Maintenance item called out explicitly in `defaults.rs` module header |
| Nightly real-brain smoke flaky when upstream SDKs update | Low | Nightly, not gating; accept flake as CI reality |

## Success criteria

After all three phases ship:

1. **Structural quality.** `build_brain_prompt` composed of named helper fns, each ≤50 LoC, no single function body >80 LoC. Prompt blocks are pure functions of config.
2. **Prompt determinism.** Snapshot test against a fixture config + fixture task produces byte-identical output. No timestamp, no SessionId, no cost figures in the assembled prompt body.
3. **Routing accuracy.** ≥7/10 exact-agent matches on the `test/fixtures/delegation-routing/` corpus via MockBrain harness.
4. **Observable compliance.** In local dogfooding over one month, ≥80% of DelegationRequested events carry a non-None `delegation_plan`. Health signal, not release gate.
5. **No regression.** All `brain-worker-refinement` tests remain green. CI-enforced.

## File touch summary

| File | Section | Change |
|---|---|---|
| `crates/spur-acp/src/agents/defaults.toml` | D | **New** — bundled defaults |
| `crates/spur-acp/src/agents/defaults.rs` | D | **New** — loader, merge, lint, API |
| `crates/spur-acp/src/agents/mod.rs` | D | **New** — re-exports |
| `crates/spur-acp/src/lib.rs` | D | `pub mod agents;` |
| `crates/spur-acp/src/config/` (parser) | A | `DelegationDescriptor` struct; `AgentConfig.delegation` field; call `apply_builtin_defaults` + `validate_delegation_config` post-parse |
| `crates/spur-acp/src/domain/delegation.rs` | C | `DelegationPlan`, `PlanCandidate`, `PlanSubtask` structs |
| `crates/spur-acp/src/domain/review.rs` | C | `ReviewPayload.delegation_plan` + `chosen_matches_dispatched` |
| `crates/spur-acp/src/domain/events.rs` | C | `DelegationRequested.delegation_plan` |
| `crates/spur-mcp/src/server.rs` | C | Extend `WorkerInfo`; stamp `delegation_plan` onto `DelegationRequest` in 8 handlers |
| `crates/spur-mcp/src/tools.rs` | C | 3 tool descriptions updated; `delegation_plan` input schema; `DelegationRequest.delegation_plan` field |
| `crates/spur-core/src/orchestrator.rs` | B + C | Prompt rewrite via helper fns + logging; `build_worker_info` at 3 spawn sites; mismatch detection; review-payload population; `normalize_agent_name`; feature-flag gate |
| `crates/spur-cli/src/main.rs` | D | (optional, coordinates with Spec 1) surface lint warnings in `config check` |
| `docs/spur/agent-config.md` | E | `[delegation]` section |
| `docs/spur/brain-worker-architecture.md` | E | Dispatch-side update |
| `.spur/config.toml.example` | E | Commented `[delegation]` per agent |
| `docs/spur/contributing-agent-defaults.md` | E | **New** |
| Changelog | E | Phase entries |

**Estimated LoC**: ~600 code + ~200 tests + ~50 flag plumbing + ~10 event schema = **~860 total** across 4 crates. Docs: ~300 prose lines.

## Interactions with adjacent specs

| Spec | Relationship |
|---|---|
| `brain-worker-refinement-design.md` | **Hard dependency** — Change 1 must merge first. Additional integration: retry augmented-task template (their Change 3) gains an `Expected output shape:` line sourced from the chosen agent's `output_shape` |
| `agent-onboarding-roadmap.md` | Extends sub-table registry; roadmap's "config declares CHOICES" rule preserved (descriptors are data); adds a `delegation` row to the roadmap's sub-table table |
| `agent-config-foundation-design.md` (Spec 1) | Consumes `validate_delegation_config` API via `spur config check`; no hard ordering dependency |
| `spec2-agent-command-surface.md` | Independent; different sub-table (`commands`); merges cleanly |
| `spurevent-stream-backbone-design.md` | `DelegationRequested` event gains `delegation_plan` field; additive, same funnel/broadcast |

## What this does NOT change

- Four-channel architecture (MCP bridge, delegation pipeline, event bus, review gate).
- `DelegationStatus` variants or any worker-side code path.
- ACP / MCP wire protocol surface — no new MCP tools (only schema extension on existing ones).
- Blocking semantics of `delegate_to_worker`.
- Brain prompt contracts other than the system prompt (append, task, issue context blocks remain).
- Build-time dependencies (no new crates required beyond `toml` which spur-acp already uses).
