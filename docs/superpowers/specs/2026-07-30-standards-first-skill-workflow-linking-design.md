# Standards-First Skill Workflow Linking Design

**Decision date:** 2026-07-30
**Status:** Empirically validated; written-spec review pending
**Design epic:** `bd-1vnls`
**Plan ID:** `3139f802-06fc-4fdc-968d-1ad72a495cd4`
**Target area:** `assets/skills/`, `crates/spur-core/src/skills/`, runtime skill
projection

## Summary

SPUR will keep every canonical skill as an independently valid
[Agent Skills](https://agentskills.io/specification) package rooted at
`SKILL.md`. Workflow relationships will be authored inside the standard
`metadata` string map, rendered into an agent-readable `Workflow links`
section, and compiled by SPUR into a validated in-memory graph.

There will be no authoritative `assets/skills/skill-graph.toml`. A serialized
graph may exist under `.spur/runtime/` as a derived projection artifact, never
as a second source of truth.

The projection pipeline will map canonical logical skill IDs to the names that
each host actually exposes. For example, canonical `spur-way` may render as
`spurpower-spur-way`; every generated workflow reference must be rewritten
through the same name map. This closes the current gap where frontmatter names
are prefixed but body references remain unprefixed.

Projection makes a skill available; it does not activate the skill body.
SPUR will therefore compile the selected graph into an adapter-specific
activation agenda for the launch prompt, while the projected `Workflow links`
block gives the agent imperative instructions for later handoffs. Codex and
OpenCode use different activation mechanisms, so success is defined as loading
and following the exact target body rather than emitting one universal tool
call.

Only `requires` edges form an acyclic dependency DAG. Guards, handoffs, retries,
and role transitions have distinct semantics; handoff and retry paths may
cycle. Runtime selection will use role- and workflow-aware closure instead of
projecting the complete catalog into every agent session.

## Problem

The current asset catalog is portable in shape but not fully portable in
metadata or linking:

- canonical assets use the nonstandard top-level `role:` field;
- most workflow relationships exist only as prose, and many skills have no
  explicit links at all;
- `frontmatter::parse_source` extracts only `name`, `description`, and the
  nonstandard top-level `role`;
- `SelectionPolicy` contains only `AllActive`;
- `resolve_effective_skills` receives `RuntimeRole` but ignores it;
- adapter rendering prefixes the skill's `name` while copying the Markdown body
  unchanged;
- Codex, Claude Code, OpenCode, and the Agent Skills specification do not
  define a portable dependency-graph sidecar.

The last point is a standards boundary, not an implementation gap in those
agents. A checked-in `skill-graph.toml` could help SPUR, but native agents would
not discover or enforce it. Making that file authoritative would leave two
incompatible products: SPUR-aware workflows and standalone Agent Skills.

The complete `AllActive` projection also contributes to host skill-description
budget pressure. Codex may shorten descriptions when the discovered catalog
exceeds its skills context budget. A graph sidecar would not address that
warning because native discovery still sees every projected skill description.

## Standards Grounding

The cross-vendor common denominator is:

| Host/specification | Portable contract | Relevant extension |
|---|---|---|
| [Agent Skills](https://agentskills.io/specification) | A directory containing `SKILL.md`; required `name` and `description`; optional `license`, `compatibility`, `metadata`, and experimental `allowed-tools` | `metadata` is explicitly a string-to-string extension map |
| [Codex](https://github.com/openai/codex/blob/main/codex-rs/skills/src/assets/samples/skill-creator/SKILL.md) | Standard `SKILL.md` package and progressive disclosure | Optional `agents/openai.yaml` is OpenAI-specific interface/policy metadata, not a workflow graph |
| [Claude Code](https://code.claude.com/docs/en/skills) | Agent Skills-compatible `SKILL.md` | Additional invocation, tool, and subagent frontmatter fields |
| [OpenCode](https://opencode.ai/docs/skills) | Discovers standard packages from `.agents/skills`, `.claude/skills`, and `.opencode/skills` | Unknown top-level fields are ignored |

Therefore:

1. source packages must remain valid without SPUR;
2. SPUR extensions must use the standard `metadata` map;
3. workflow instructions needed by a native agent must appear in the Markdown
   body;
4. SPUR alone is responsible for deterministic graph validation, role
   selection, closure, and target-name rewriting.

These boundaries were confirmed against real Codex and OpenCode ACP sessions.
See the [ACP probe results](2026-07-30-skill-linking-acp-probe-results.md).

## Goals

- Keep `assets/skills/<id>/SKILL.md` valid under the Agent Skills standard.
- Make workflow relationships explicit, typed, validated, and reviewable.
- Give standalone native agents readable next-step instructions.
- Give SPUR one machine-readable source for graph construction.
- Prevent metadata/body drift.
- Resolve canonical IDs to adapter- and plugin-visible names deterministically.
- Compile selected requirements into host-specific activation instructions.
- Reduce runtime catalog size through role and workflow closure.
- Preserve the existing immutable generation, ownership, reconciliation, and
  user-file safety contracts.
- Support long-running review/retry loops without falsely rejecting them as
  dependency cycles.

## Non-goals

- Defining a new cross-vendor Agent Skills standard.
- Requiring Codex, Claude Code, OpenCode, or another host to interpret SPUR
  metadata.
- Guaranteeing dependency auto-loading when a skill is copied outside SPUR.
  The body remains understandable, but standalone hosts decide whether to
  follow a named handoff.
- Defining one cross-vendor nested skill-invocation RPC. No such ACP primitive
  was exposed consistently by the tested hosts.
- Replacing the current projection manifest, ownership journal, symlink/copy
  fallback, or generation garbage collection.
- Dynamically changing a host's discovered skills in the middle of an existing
  session. Workflow closure is computed before launch.
- Introducing an LLM-as-judge for deterministic adapter rendering.

## Chosen Architecture

The design has five layers:

```text
canonical SKILL.md
  standard frontmatter + namespaced workflow metadata
                         |
                         v
authoring validator/generator
  validates schema + renders portable Workflow links block
                         |
                         v
resolver
  builds typed graph + selects roots + computes role/workflow closure
                         |
                         v
adapter renderer
  builds canonical-ID -> host-visible-name map
  rewrites generated metadata/body references
  compiles adapter-specific activation agenda
                         |
                         v
immutable runtime generation
  standard projected skills + derived graph/name-map diagnostics
```

Canonical source, rendered body, and runtime graph have different
responsibilities:

| Layer | Authority | Editable |
|---|---|---|
| Canonical `SKILL.md` metadata | Workflow semantics and logical IDs | Yes |
| Canonical `Workflow links` body block | Generated human/agent view | Only through generator |
| Projected `SKILL.md` | Host-specific derived package | No |
| Runtime graph/name map/activation agenda | Diagnostics and launch evidence | No |

## Canonical Skill Layout

The source tree remains ordinary Agent Skills:

```text
assets/skills/
  spur-way/
    SKILL.md
    references/                 # optional
    scripts/                    # optional
    assets/                     # optional
  writing-plans/
    SKILL.md
  brain-delegation/
    SKILL.md
  ...
```

There is deliberately no canonical graph file beside these directories.

### Standard frontmatter

Only fields allowed by the Agent Skills specification appear at the top level.
Existing `role:` fields move under `metadata`.

```yaml
---
name: writing-plans
description: Use when converting an approved design into a beads-backed implementation task DAG.
compatibility: Requires SPUR project-management and planning tools.
metadata:
  "getspur.schema": "workflow/v1"
  "getspur.role": "brain"
  "getspur.requires": "spur-way,beads-lifecycle"
  "getspur.guards": "submit=plan-task-discipline"
  "getspur.handoffs": "submitted=brain-delegation;revision=brainstorming"
---
```

All metadata values remain strings, as required by the standard.

### Namespaced metadata grammar

`getspur.schema` opts a skill into SPUR workflow semantics. Unknown non-SPUR
metadata is preserved and ignored by the workflow parser.

When `getspur.schema` is present, `getspur.role` is required. The relationship
keys are optional and default to an empty set.

| Key | Value grammar | Meaning |
|---|---|---|
| `getspur.schema` | exactly `workflow/v1` | Workflow schema version |
| `getspur.role` | `brain`, `worker`, or `both` | Runtime role eligibility |
| `getspur.requires` | comma-separated canonical skill IDs | Same-session prerequisites |
| `getspur.guards` | semicolon-separated `transition=skill-id` entries | Skill that must validate a transition |
| `getspur.handoffs` | semicolon-separated `outcome=skill-id` entries | Next workflow skill or cross-role handoff |
| `getspur.overlays` | semicolon-separated `adapter=skill-id` entries | Adapter-specific companion skill |

Skill IDs already exclude comma, semicolon, and equals characters, so this
bounded grammar is unambiguous without inventing YAML nested structures.
Whitespace around separators is ignored; duplicate keys, duplicate edges, an
empty endpoint, or a malformed adapter/outcome token is invalid. Adapter tokens
must name a supported projection adapter. Transition and outcome tokens must
match `^[a-z][a-z0-9-]*$`; their meaning is owned by the source skill rather
than a global outcome vocabulary.

The first implementation supports one target per guard, handoff outcome, or
adapter entry. Fan-out belongs in a plan DAG, not in skill metadata.

### Agent-readable generated block

The authoring generator renders the metadata into a bounded block:

```markdown
<!-- SPUR-WORKFLOW:BEGIN v=1 sha256=<metadata-digest> -->
## Workflow links

- **Required skills:** `spur-way`, `beads-lifecycle`
- **Before `submit`:** Use `plan-task-discipline`.
- **On `submitted`:** Continue with `brain-delegation`.
- **On `revision`:** Return to `brainstorming`.
<!-- SPUR-WORKFLOW:END -->
```

The digest covers the normalized `getspur.*` metadata. Validation fails when:

- only one marker exists;
- generated text or digest differs from metadata;
- an author places a second `Workflow links` block elsewhere;
- the block contains unresolved skill IDs.

Authoring may expose separate `check` and `fix` operations, but projection is
always strict. It never silently accepts a stale canonical body.

The generated block is the only body region that the adapter renderer rewrites.
Free-form prose and examples are never globally search-and-replaced.

## Typed Workflow Graph

Each skill is a node keyed by its canonical `name`.

### Edge semantics

| Edge | Ordering rule | Projection behavior |
|---|---|---|
| `requires` | Must form a DAG | Include target before source; part of prerequisite closure |
| `guard` | May point backward to a reusable verifier | Include target in the same workflow closure |
| `handoff` | May cycle through rejection/retry | Include same-role reachable targets; record cross-role transitions |
| `overlay` | Selected by adapter | Include exactly the applicable companion |

Only `requires` participates in cycle rejection. Treating every edge as
`before` is incorrect because a legitimate review loop can contain:

```text
implementation -> verification -> review
       ^                         |
       +------- rejection -------+
```

Graph closure uses a visited set, so cyclic handoffs terminate without being
mistaken for a topological order.

### Role rules

- `brain` sessions may select `brain` and `both` nodes.
- `worker` sessions may select `worker` and `both` nodes.
- `init` may materialize all valid nodes for explicit prewarming.
- A `requires`, `guard`, or same-session handoff target must be compatible with
  the source session's role.
- An incompatible handoff is recorded as a cross-role SPUR transition. It is
  rendered as a delegation/handoff instruction, not a native same-session
  invocation.
- An adapter overlay must have the same role eligibility as its base skill.

## Runtime Selection

`SelectionPolicy::AllActive` remains available for `spur skills init` and
compatibility, but it is no longer the only runtime policy.

Add two conceptual policies:

```rust
enum SelectionPolicy {
    AllActive,
    RoleScoped,
    WorkflowClosure { roots: BTreeSet<String> },
}
```

`RoleScoped` selects every role-compatible built-in and active skill, then adds
applicable overlays and prerequisite/guard closure.

`WorkflowClosure` starts from explicit roots supplied by the launch context:

- brain launch: mandatory SPUR brain roots plus the selected agent overlay;
- worker launch: mandatory worker discipline roots plus task/plan-selected
  skills;
- explicit user skill invocation known before launch: the invoked skill;
- manual initialization: not used; `AllActive` applies.

Closure includes:

1. transitive `requires`;
2. guards;
3. applicable overlays;
4. same-role handoff reachability needed by the long-running workflow.

Cross-role handoffs are recorded but do not leak role-incompatible skills into
the current agent catalog.

This structural narrowing reduces description pressure without encoding a
host-specific token constant. The ordered prerequisite/root set also becomes
the activation agenda rendered by the launch adapter. The projection summary
records selected skill count and total description characters so host warnings
can be diagnosed.

## Projection and Name Rewriting

The renderer builds a complete name map before rendering any skill:

```text
canonical ID              Codex/Claude/OpenCode visible name
spur-way               -> spurpower-spur-way
writing-plans          -> spurpower-writing-plans
brain-delegation       -> spurpower-brain-delegation
```

For every selected skill:

1. compute its target-visible name;
2. require every same-generation workflow endpoint to exist in the map;
3. render standard frontmatter using the visible name;
4. render projected `getspur.*` metadata using visible endpoints;
5. render the generated `Workflow links` block using visible endpoints;
6. preserve canonical IDs in the runtime manifest for lineage.

Adapter or plugin namespaces are data, not string literals embedded throughout
the renderer. The same mapping operation covers `spurpower-`, a Claude plugin
namespace, an unprefixed hermetic skill, or a future adapter convention.

An unresolved endpoint is a fatal generation error. Rendering a body that tells
the agent to invoke a nonexistent name is never a warning-only condition.

### Availability, activation, and enforcement

These are distinct contracts:

| Contract | Owner | Evidence |
|---|---|---|
| Availability | Resolver and projection | Target exists in the session's advertised catalog |
| Activation | Launch adapter and generated workflow instructions | Target body was loaded before dependent action |
| Enforcement | SPUR workflow/review state | Required transition or guard was recorded and validated |

The runtime graph is not expected to be interpreted by a native host. The
adapter compiles a bounded launch-prompt segment from the topologically ordered
`requires` closure and selected roots. It uses exact visible names and tells
the host to activate each prerequisite before the root.

Later guards and same-session handoffs use the projected `Workflow links`
block. The renderer must use an adapter activation primitive rather than a
single generic sentence. The adapter contract exposes the conceptual
operation:

```text
render_activation(visible_skill_name, purpose) -> bounded instruction text
```

For the probed hosts:

- OpenCode activation directs the agent to call its native `skill` tool with
  the exact visible name.
- Codex activation uses the visible `$skill-name` form for initial invocation
  and an explicit instruction to read and follow the exact projected skill for
  nested workflow steps. Codex ACP did not expose a nested skill tool.

An adapter may implement another activation style, but it must pass an
integration probe that proves the target body was loaded. Tests must not assert
that all hosts emit the same ACP tool-call variant.

Projection closure is still computed before launch. The activation agenda does
not add or remove discovered skills in a live session; it only activates the
already selected subset.

## Runtime Artifacts

The existing generation layout remains authoritative for ownership and
reconciliation. The manifest gains:

- workflow schema version;
- canonical graph digest;
- selection policy and workflow roots;
- selected canonical IDs;
- canonical-to-visible name map;
- resolved same-role and cross-role edges;
- adapter activation style and ordered launch activation agenda;
- selected skill count and description-character total.

An optional `workflow-graph.json` may be emitted beside the generation manifest
for diagnostics. It is derived from the selected canonical sources and covered
by the generation digest. Editing or copying it back into `assets/skills/` has
no effect.

## Current-Code Integration Seams

The implementation should extend existing boundaries rather than create a
parallel installer:

| Current component | Required change |
|---|---|
| `skills/frontmatter.rs::parse_source` | Parse standard metadata and compatibility fields; stop treating top-level `role` as canonical after migration |
| `SkillPayload` | Carry normalized standard frontmatter plus typed `WorkflowMetadata` |
| `projection::resolver::resolve_effective_skills` | Honor `RuntimeRole`, apply the selected policy, validate graph endpoints, and compute closure |
| `projection::SelectionPolicy` | Add role-scoped and explicit-root workflow policies |
| `Adapter::render_with_prefix` and render helpers | Build/use a name map and render rewritten workflow metadata/body blocks |
| adapter launch-prompt rendering | Compile ordered requirements and roots into exact-name activation instructions |
| projection generation/manifest | Include graph, root, name-map, activation, and context-budget diagnostics in the digest and manifest |
| bundled-skill conformance tests | Reject nonstandard top-level fields and validate generated workflow blocks |

The runtime projection design's immutable generations, locking, pending
journal, ownership proof, copy fallback, collision behavior, and garbage
collection remain unchanged.

## Backward Compatibility and Migration

### Bundled assets

Migrate bundled skills in a controlled inventory:

1. move `role:` to `metadata."getspur.role"`;
2. add `getspur.schema`;
3. encode existing explicit cross-references as typed metadata;
4. generate the workflow block;
5. shorten descriptions to triggering conditions rather than workflow prose;
6. validate the complete bundled graph before rendering any adapter.

Bundled skills are first-party and must pass the full workflow schema after
migration.

### Pool and repository skills

- A third-party skill without `getspur.schema` remains a standalone Agent Skill
  with role `both` and no workflow edges.
- A skill that declares any `getspur.*` key must declare a supported schema and
  pass strict validation.
- Source candidates are atomic. SPUR never inherits missing workflow metadata
  from a lower-precedence skill with the same ID.
- Unknown non-SPUR metadata remains untouched.

### Legacy generated targets

Existing `SPUR-MANAGED` ownership rules apply. A new generation replaces valid
owned output. User-edited or unowned files remain preserved exactly as the
runtime projection design requires.

## Error Handling

These conditions fail validation or generation:

- malformed `getspur.*` grammar;
- unsupported workflow schema;
- duplicate canonical skill ID after precedence resolution;
- unresolved edge target;
- `requires` cycle;
- role-incompatible prerequisite, guard, or overlay;
- missing adapter overlay;
- stale or malformed generated workflow block;
- projected-name collision;
- selected edge whose visible endpoint cannot be rendered;
- adapter without an activation renderer for a selected workflow skill.

These conditions are non-fatal:

- a standalone third-party skill has no SPUR metadata;
- handoff/retry cycles exist;
- a cross-role handoff is not included in the current session;
- unknown metadata outside the `getspur.*` namespace exists.

Errors identify the source skill, metadata key, endpoint, source kind, and
adapter when applicable.

## Solver Validation

The approved architecture was evaluated with `spurpower-solve`.

| Question | Status | Artifact |
|---|---|---|
| Unchecked metadata/body mirror plus prefixed names without reference rewriting | `unsat` | `sol_1a02716917404d48` |
| Validated mirror plus prefix-aware rewriting | `sat` | `sol_585dd25fe0b44414` |
| Generated body plus prefix-aware rewriting | `sat` | `sol_284f34fcc66e4686` |
| Collision-safe prefixed projection without reference rewriting | `unsat` | `sol_4bd61d96f0b545be` |
| One acyclic order containing requirements and retry handoffs | `unsat` | `sol_d38d5ba8466f41da` |
| Acyclic `requires` with separately typed handoffs | `sat` | `sol_03ab94e8c2ae4e30` |

These artifacts prove consistency relative to the encoded standards,
single-authority, drift, collision, name-resolution, role, and graph-ordering
constraints. They do not replace conformance and integration tests.

## Testing Strategy

### Schema and conformance

- Accept every Agent Skills standard top-level field used by SPUR.
- Reject top-level `role`, `agent`, `activation`, or other SPUR inventions in
  canonical bundled assets.
- Verify `metadata` is string-to-string.
- Test every metadata separator, duplicate, empty endpoint, invalid ID, and
  unsupported schema case.
- Validate every bundled asset and the complete bundled graph.

### Generator

- Golden-test canonical workflow block output.
- Detect stale digest, changed generated prose, duplicate blocks, and missing
  markers.
- Verify generation is idempotent.
- Verify free-form body prose is never rewritten.

### Graph and selection

- Detect direct and transitive `requires` cycles.
- Accept cyclic handoffs and terminate closure deterministically.
- Validate guards and adapter overlays.
- Test same-role and cross-role handoffs.
- Test `AllActive`, `RoleScoped`, and explicit `WorkflowClosure` roots.
- Verify worker and brain projections exclude role-incompatible skills.

### Adapter projection

- Golden-test canonical-to-visible name maps for every adapter.
- Verify frontmatter, metadata endpoints, and workflow body endpoints use the
  same visible name.
- Golden-test each adapter's launch activation agenda and projected handoff
  instructions.
- Reproduce the current `writing-plans` raw-reference failure and prove the new
  projection emits resolvable names.
- Include workflow graph and name map in stable generation hashing.
- Preserve supporting resources and current ownership safety.

### Launch integration

- Brain launch selects required brain roots and the correct agent overlay.
- Worker launch includes mandatory discipline plus plan-selected roots.
- Launch prompt activates ordered prerequisites and then the root using exact
  host-visible names.
- Codex and OpenCode fixtures prove target-body loading with a hidden evidence
  token; the test accepts host-specific trace shapes.
- Metadata-only fixtures prove hosts do not implicitly execute
  `getspur.requires`.
- Manual initialization still materializes the complete catalog.
- Projection summary reports skill count and description characters.
- Fatal graph/name errors prevent the agent session from starting.

All Rust compilation and tests run through `scripts/spur-cargo`.

## Task Decomposition Boundaries

The implementation plan should preserve these boundaries:

1. **Schema/parser and conformance** — typed metadata, canonical validation,
   legacy-role migration support.
2. **Graph/generator** — edge validation, workflow block generation, cycle
   rules.
3. **Adapter mapping and activation** — visible-name map, projected workflow
   rewrite, and host-specific activation rendering.
4. **Resolver selection** — role-aware and explicit-root closure policies.
5. **Asset migration** — bundled metadata, generated links, shortened
   descriptions.
6. **Launch/manifest integration** — root selection, diagnostics, generation
   digest.

Schema/parser precedes all other tasks. Graph/generator and adapter mapping may
then proceed in parallel. Resolver selection depends on the typed graph. Asset
migration depends on generator availability. Launch integration follows
resolver and renderer completion.

## Relationship to Existing Decisions

This design amends
`2026-07-17-runtime-skill-projection-design.md` by implementing its deferred
next-stage selection policy and by adding graph/name-map data to rendered
generations.

It supersedes only these earlier decisions:

- runtime selection is no longer limited to `AllActive`;
- canonical SPUR role data no longer uses a nonstandard top-level field;
- adapter rendering no longer copies workflow references unchanged.

It does not supersede projection storage, ownership, reconciliation, launch
ordering, failure atomicity, or pool precedence.

It also tightens the older brain-delegation amendment that described `role`,
`agent`, and `activation` as top-level SPUR extensions. New canonical assets
must express portable SPUR workflow properties through namespaced standard
metadata.

## Acceptance Criteria

1. Every bundled source skill validates as an Agent Skills package with no
   SPUR-only top-level frontmatter.
2. Workflow metadata is the only machine-authoritative relationship source.
3. Every linked bundled skill contains a generated, digest-checked
   agent-readable workflow block.
4. No authoritative `skill-graph.toml` exists under `assets/skills/`.
5. `requires` cycles fail validation while handoff/retry cycles succeed.
6. Brain and worker runtime projections honor role and workflow closure.
7. Every projected workflow endpoint resolves to the exact name exposed by the
   target adapter.
8. Launch activation loads every same-session prerequisite before its
   dependent root on Codex and OpenCode fixtures.
9. The runtime manifest records canonical IDs, visible names, graph digest,
   roots, activation agenda, selected count, and description characters.
10. Existing ownership and user-file preservation tests continue to pass.
11. Focused conformance, graph, adapter, resolver, and launch integration tests
    pass through `scripts/spur-cargo`.
