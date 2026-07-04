# Code-Graph Ontology Maturity Roadmap

Status: roadmap/spec
Crate: `spur-graph`
Companions:
- `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb`
- `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-spec-live-evidence.ipynb`
- `crates/spur-graph/queries/README.md`

## Problem Statement

SPUR's current graph ontology is useful as a Tier-0 navigation and
agent-impact model. It captures files, broad symbol kinds, structural
containment, definitions, imports, calls, construction, inheritance,
implementation, references, markdown links, temporal touches, and notebook
semantic facts. It also carries relation provenance through `confidence`,
`confidence_score`, `GraphEdgeKind`, and `bind_method`.

That is enough for today's primary agent workflows: code exploration,
blast-radius analysis, review setup, notebook lineage, and queryable
repository orientation. It is not yet an IDE-grade semantic index. SPUR does
not model constructors, operators, properties, parameters, local variables,
type parameters, or other fine-grained symbols as first-class ontology
entities, and it should not add them opportunistically.

The roadmap is to evolve the ontology in layers: first make relation quality
measurable, then add semantic-lite facts where syntax is strong enough, and
only then consider compiler-backed or LSP-backed precision for languages and
workflows that require it. Broad SCIP parity is explicitly not the immediate
goal. SCIP is a useful comparison point for missing semantic categories, not a
mandate to mirror its schema.

## Workflow Anchor

Every ontology expansion should name the workflow it improves:

- Code exploration: faster, more precise "where is this thing?" answers.
- Blast-radius analysis: safer caller/callee and dependency impact bounds.
- Review: better API-change, behavior-change, and risk summaries.
- Notebook lineage: reliable cell, port, datasource, bind, and emit facts.
- Query quality: lower false positives, clearer confidence, stable contracts.

If a proposed node or relation does not materially improve at least one of
these workflows, keep it out of the first-class schema.

## Maturity Tiers

These maturity tiers are about ontology precision. They complement, but do not
replace, the existing Tier-0 T-box design spec.

| Tier | Name | What It Means | Evidence Required |
|---|---|---|---|
| 0 | Syntax-first navigation graph | Current broad `NodeKind` coverage and practical `RelationKind` facts from tree-sitter captures: files, symbols, `contains`, `defines`, `imports`, `calls`, `constructs`, `extends`, `implements`, `references`, links, touches, and notebook facts. | Query coverage matrix, `gate_contract`, capture tests, integration fixtures, stable serialized discriminators. |
| 1 | Relation-quality hardening | Improve trust in existing facts before expanding ontology shape. Standardize confidence, `bind_method`, `GraphEdgeKind`, domain/range guards, relation coverage gates, and false-positive controls. | Benchmark corpus with positive and negative fixtures; relation matrix gate; precision/recall reports for agent queries; explicit false-positive guard tests. |
| 2 | Semantic-lite enrichment | Add richer facts when syntax and local scope evidence are strong enough. Prefer relation metadata or sub-kinds first; add first-class nodes only when identity, scope, and query value are proven. | Tier-1 gates passing; workflow-backed proposal; cross-language capture contract; migration plan; benchmark delta showing better query quality without unacceptable graph growth. |
| 3 | Compiler-backed or indexer-backed precision | Use compiler, language-server, or external indexer evidence where syntax-only resolution cannot be made trustworthy. This is opt-in by language and workflow, not a universal mandate. | Clear syntax-only failure mode; adapter design for external IDs and freshness; performance budget; fallback semantics; benchmark proof that the precision gain justifies operational cost. |

## Promotion Criteria

### Capture To First-Class `NodeKind`

A syntax capture should become a new `NodeKind` only when all are true:

1. It has durable identity across edits, not just a transient token position.
2. It is queried directly by agent workflows, not merely used to explain
   another symbol.
3. It needs inbound or outbound graph edges of its own.
4. Its scope and ownership can be represented consistently across the first
   target languages.
5. It does not create high-cardinality noise that overwhelms exploration.
6. Existing `NodeKind`, labels, `symbol_kind`, or metadata cannot answer the
   workflow with comparable quality.
7. Its identity-stability cost is acceptable: re-kinding a symbol severs its
   temporal lineage because kind is part of the stable-symbol-id hash
   (`crates/spur-graph/src/identity.rs:54-66`).

If these are not true, keep the capture as one of:

- `target_label` or unresolved label evidence.
- `GraphEdgeKind` or relation metadata.
- `bind_method` or confidence provenance.
- Search/index tokens attached to the enclosing symbol.
- Scope evidence in benchmark-only or query-only tables.

### Capture To First-Class `RelationKind`

A relation should become a new `RelationKind` only when all are true:

1. It has a distinct predicate meaning, not only a confidence or binding
   variant of an existing predicate.
2. It needs different domain/range, transitivity, cardinality, or inverse
   semantics.
3. Existing `GraphEdgeKind`, `bind_method`, `confidence`, `target_label`, or
   metadata would make common queries ambiguous or error-prone.
4. It has at least one named workflow that benefits from querying it directly.
5. It has negative fixtures proving the extractor will not over-emit it.
6. It can be introduced without breaking existing clients or has a documented
   migration/versioning path.

Prefer metadata when the distinction answers "how was this edge produced?".
Prefer `GraphEdgeKind` when the distinction is a sub-mode of an existing
predicate. Prefer a new `RelationKind` only when the distinction answers "what
relationship is this?".

Treat `NodeKind` as the highest-cost carrier. It is hashed into
`stable_symbol_id` (`crates/spur-graph/src/identity.rs:54-66`), enters resolver
allowlists, and forces a `SCHEMA_VERSION` bump plus a full rebuild. A new
`NodeKind` is the last resort: justify it only when the distinction must drive
resolution or identity - an allowlist, a stable-id boundary, or a
containment/ownership rule - and no lower-cost carrier (`metadata`,
`GraphEdgeKind`, `RelationKind`, or label) can satisfy a proven consumer. The
`Resource` promotion met this bar because HCL address resolution needed a
resolver allowlist.

## Candidate Future Symbol Kinds

| Candidate | Rationale | Risks | Deferral Criteria |
|---|---|---|---|
| Constructor / initializer | Improves construction tracing and API review where languages have explicit constructors. | Rust uses ordinary functions and conventions; TS/Python/C++ differ; may duplicate `constructs`. | Defer until `constructs` precision is benchmarked and constructor identity can be represented without language-specific surprises. |
| Operator / operator overload | Useful for C++/Rust overload review and hidden call analysis. | Syntax-only call resolution is weak; operator tokens often lower to methods/traits differently by language. | Keep as call metadata until overload definitions and callsites meet Tier-2 precision thresholds. |
| Property | Helps distinguish data members, accessors, and generated getter/setter APIs. | Python decorators, TS accessors, Rust fields, and C++ members do not align cleanly. | Keep under `Field` or access metadata until read/write workflows need direct property nodes. |
| Parameter | Valuable for signature review, API compatibility, and call argument matching. | High volume; stable IDs must include function scope and position/name changes; many languages allow destructuring/defaults. | Defer until API-change review benchmarks show clear value and stable ID rules are documented. |
| Local variable | Can support local dataflow and bug review. | Very high cardinality, poor durable identity, high churn, likely to drown navigation. | Do not add as first-class Tier-2 nodes; consider compiler/indexer-backed scoped facts only for Tier 3. |
| Type parameter / generic parameter | Useful for trait/interface and generic API analysis. | Bounds and variance semantics are language-specific; syntax-only resolution may mislead. | Start as signature/scope metadata; promote only with cross-language fixture coverage and query demand. |
| Decorator / annotation / attribute | Explains framework behavior, routing, tests, and generated code hooks. | Often semantic only after macro/decorator expansion; names may be external. | Represent as references or metadata first; promote if review workflows need direct impact analysis. |
| Namespace / package | May improve import and module ownership queries in C++/TS/Python. | Overlaps existing `Module` and `File`; inconsistent source representation. | Prefer `Module` until a concrete language shows module/package ambiguity in benchmark queries. |

## Candidate Future Relations

| Candidate | Rationale | Risks | Deferral Criteria |
|---|---|---|---|
| Reads / writes / accesses | Better blast radius for stateful changes and field/property review. | Syntax-only receiver/type resolution is noisy; read/write classification can be subtle. | Start as access metadata or `references` sub-kind; split only after false-positive guards pass. |
| Overrides | Critical for OO and trait/interface review. | Requires method binding and inheritance semantics; syntax-only is insufficient in several languages. | Tier 3 unless a language has syntactically explicit, reliable override markers. |
| Decorates / annotates | Useful for framework route/test/job discovery. | Decorator semantics often depend on runtime or macro expansion. | Keep as `references` plus metadata until direct query value is proven. |
| Has type / returns / throws | Helps API review and type-impact analysis. | Syntax annotations may be absent, inferred, or wrong after aliases/imports. | Tier 2 for explicit annotations only; Tier 3 for inferred types. |
| Exports / re-exports | Improves public API and package boundary analysis. | Overlaps `imports`; language module systems vary. | Add only if API-surface workflows cannot be answered by import metadata. |
| Dataflow edges | Useful for notebook lineage and local bug review. | High cardinality and semantic complexity. Notebook ports already model the workflow-specific subset. | Keep broad code dataflow out of Tier 2; require Tier-3 indexer evidence and strict scope. |

## Backward Compatibility

Existing graph clients need these guarantees:

- Existing `NodeKind` and `RelationKind` discriminators keep their current
  meaning. Do not silently reinterpret `calls`, `constructs`, `contains`,
  `defines`, `references`, or notebook fact relations.
- Adding enum variants is a compatibility event. Rust clients with closed enum
  deserializers can fail on unknown variants, so new variants require a graph
  index version or manifest-version bump, release notes, and a migration plan.
- Kind assignment is a stable contract. Re-kinding an existing symbol severs its
  temporal lineage/history because kind is part of the `stable_symbol_id` hash
  (`crates/spur-graph/src/identity.rs:54-66`), so "add now, maybe retract later"
  is not a neutral choice.
- Prefer additive metadata or dual emission before splitting a predicate that
  existing queries depend on.
- Preserve the `contains` lexical spine. Derived semantic relations such as
  `defines` can be added beside it, but should not replace it.
- Unresolved labels remain valid evidence. A failed bind is often safer than a
  wrong resolved edge.
- Query README matrices remain the human-readable contract. If a language does
  not realize a feature, mark it `-` or `TODO`; never leave it implicit.

Migration expectations for any future schema expansion:

1. Land benchmark fixtures and contract tests first.
2. Add metadata or compatibility views before adding closed enum variants when
   possible.
3. Bump artifact/index versioning when serialized shape or enum vocabulary
   changes.
4. Document client behavior for old artifacts, new artifacts, and mixed
   worktrees.
5. Rebuild or invalidate cached graph artifacts when resolver semantics change.

## Benchmark And Test Requirements

Before expanding ontology shape, the proposal must add or update:

- Query-level tests proving tree-sitter captures compile and hit expected
  syntax.
- Integration tests proving emitted graph nodes/edges have correct
  `NodeKind`, `RelationKind`, labels, ranges, confidence, and binding metadata.
- Negative fixtures for the most likely false positives.
- Relation coverage matrix updates and a gate that fails on silent drift.
- Domain/range tests for every new relation.
- Stable-ID tests for every new node kind.
- Snapshot or SQL benchmark queries for the agent workflows being improved.
- Graph-size and query-latency checks when the candidate can multiply node or
  edge count, especially parameters and locals.

Minimum benchmark bar for a new first-class kind or relation:

- Curated fixture precision is at least 95 percent, with zero known
  high-severity false positives in negative fixtures.
- Recall is measured and documented; if it is intentionally partial, the
  matrix marks the feature with a scoped `TODO` or language note.
- At least one representative agent query becomes simpler or more accurate,
  with before/after examples.
- Existing Tier-0 gates still pass, including the `spur-graph` query contract
  gate.

## Roadmap

1. Finish Tier-1 hardening before schema growth:
   - relation coverage contract tests derived from `queries/README.md`
   - domain/range assertions on persisted artifacts
   - standardized `bind_method` vocabulary
   - explicit false-positive guard fixtures for common method-name collisions

2. Build a reusable ontology benchmark corpus:
   - small positive/negative fixtures per language
   - SQL snapshots for exploration, blast-radius, and review queries
   - graph-size and latency baselines
   - documented precision/recall for each realized predicate

3. Pilot Tier-2 semantic-lite additions without new enum variants first:
   - constructor evidence as `constructs` metadata
   - property/access evidence as `references` or access metadata
   - parameter and type-parameter evidence as signature/scope metadata
   - decorator/annotation evidence as `references` metadata

4. Promote only the candidates that pass the criteria:
   - write the workflow-backed proposal
   - add fixtures and benchmark reports
   - update README matrices and compatibility notes
   - bump graph versioning if serialized vocabulary changes

5. Evaluate Tier-3 adapters only for proven syntax-only failures:
   - use compiler/LSP/indexer evidence as an optional enrichment layer
   - keep freshness, performance, and fallback behavior explicit
   - avoid committing SPUR to broad SCIP parity unless product workflows demand
     it and operational costs are acceptable

## Non-Goals

- No new `NodeKind` or `RelationKind` variants from this roadmap alone.
- No promise of whole-workspace compiler-backed resolution.
- No attempt to model every SCIP symbol category.
- No first-class local-variable graph unless a future Tier-3 design proves it
  is useful without overwhelming navigation.
- No relation split that can be represented honestly by `GraphEdgeKind`,
  `bind_method`, confidence, or metadata.

## Decision Template

Every future ontology-expansion proposal should answer:

1. Which agent workflow is blocked or materially degraded today?
2. Why are existing `NodeKind`, `RelationKind`, `GraphEdgeKind`,
   `bind_method`, labels, and metadata insufficient?
3. What tier is the feature targeting, and what evidence promotes it?
4. What languages realize it now, and which are `-` or `TODO`?
5. What are the negative fixtures and expected false-positive guards?
6. What serialized compatibility or artifact-versioning change is required?
7. What query improves, and what before/after benchmark proves it?
