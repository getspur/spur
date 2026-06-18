# SPUR To SCIP Ontology Comparison

Status: comparison/spec
Crate: `spur-graph`
Primary external reference:
<https://raw.githubusercontent.com/scip-code/scip/main/scip.proto>
Local references:
- `crates/spur-graph/src/schema.rs`
- `crates/spur-graph/queries/README.md`
- `docs/superpowers/specs/2026-06-18-code-graph-ontology-maturity-roadmap.md`

## Purpose And Scope

This document compares SPUR's current graph ontology with SCIP's code
intelligence model. It is a benchmark for ontology maturity, not a parity
commitment.

SCIP is useful because it names a richer set of symbol kinds, occurrence roles,
descriptor suffixes, and reference relationships than SPUR's current Tier-0
graph. SPUR is intentionally different: it is an agent-facing repository graph
that combines code structure, temporal facts, notebook lineage, and MCP/data-app
facts. SCIP is an interchange format for source-code indexes centered on
documents, occurrences, symbols, and relationships between symbols.

No schema changes are proposed by this document. Future schema or ontology
changes must still satisfy the promotion criteria in the ontology maturity
roadmap: a named workflow, measurable query improvement, fixtures, false-positive
guards, compatibility handling, and versioning where serialized vocabulary
changes.

## SPUR Model Summary

Current `NodeKind` variants:

`module`, `function`, `class`, `interface`, `struct`, `impl`, `trait`, `enum`,
`enum_variant`, `file`, `external`, `method`, `field`, `constant`, `type_alias`,
`macro`, `section`, `commit`, `mcp_tool`, `cell`, `port`.

Current `RelationKind` variants:

`imports`, `calls`, `constructs`, `contains`, `implements`, `defines`,
`references`, `extends`, `links`, `touches`, `produces`, `consumes`, `binds`,
`emits`.

Current `GraphEdgeKind` variants:

`calls`, `calls_dyn`, `references_hof`, `references_other`.

SPUR Tier-0 treats first-class nodes and relation predicates as a bounded,
workflow-oriented graph. It keeps additional precision in edge provenance such
as `confidence`, `confidence_score`, `bind_method`, `target_label`, and
`GraphEdgeKind` instead of promoting every syntax distinction to a new enum
variant.

## SCIP Model Summary

The SCIP proto's top-level `Index` contains:

- `Metadata`: index version, tool info, project root, and document text
  encoding.
- `Document`: language, relative path, occurrences, symbols defined in the
  document, optional text, and position encoding.
- `external_symbols`: optional `SymbolInformation` entries for referenced
  external symbols whose defining package may not be indexed separately.

SCIP's core code-intelligence concepts are:

- `Occurrence`: a range in a document with an optional symbol, symbol-role
  bitset, override documentation, syntax kind, diagnostics, and enclosing range.
- `SymbolInformation`: a symbol string, documentation, relationships, fine
  grained `Kind`, display name, signature documentation, and optional enclosing
  symbol for locals.
- `Symbol`: a structured identifier made from a scheme, package, and descriptor
  path.
- `Descriptor.Suffix`: `Namespace`, `Type`, `Term`, `Method`, `TypeParameter`,
  `Parameter`, `Meta`, `Local`, and `Macro`.
- `SymbolRole`: `Definition`, `Import`, `WriteAccess`, `ReadAccess`,
  `Generated`, `Test`, and `ForwardDefinition`.
- `Relationship`: booleans for `is_reference`, `is_implementation`,
  `is_type_definition`, and `is_definition`.
- `SymbolInformation.Kind`: a large language-neutral vocabulary including
  `Module`, `Namespace`, `Package`, `File`, `Function`, `Method`, `Class`,
  `Interface`, `Struct`, `Trait`, `Enum`, `EnumMember`, `Field`, `Property`,
  `Constant`, `Variable`, `Parameter`, `TypeParameter`, `TypeAlias`, `Macro`,
  `Constructor`, `Operator`, `Getter`, `Setter`, `StaticMethod`,
  `StaticField`, `StaticVariable`, `Union`, and many language-specific kinds.

The most important structural difference is that SCIP represents many facts as
occurrences of symbols with roles and ranges. SPUR represents repository facts
as typed graph nodes and edges, then uses edge metadata to retain binding and
confidence provenance.

## NodeKind Mapping

Legend:

- Direct-ish: SPUR and SCIP have a close conceptual equivalent.
- Partial: SCIP can represent related information, but not the same graph
  concept or SPUR intentionally collapses/splits the category.
- None: no meaningful SCIP equivalent in the proto.

| SPUR `NodeKind` | SCIP equivalent | Mapping | Notes |
|---|---|---:|---|
| `Module` | `SymbolInformation.Kind::Module`, `Namespace`, `Package`; descriptor `Namespace` | Direct-ish | SCIP distinguishes module, namespace, and package more finely. SPUR uses a broad `Module` node for language module constructs. |
| `Function` | `Kind::Function`; descriptor `Term` or method-shaped descriptor depending indexer | Direct-ish | SCIP's `Kind` separates display kind from descriptor suffix. |
| `Class` | `Kind::Class`; descriptor `Type` | Direct-ish | Close match. |
| `Interface` | `Kind::Interface`; also `Protocol`, `Trait`, or `TypeClass` for some languages | Direct-ish | SPUR keeps language-facing interface concepts in one node kind. |
| `Struct` | `Kind::Struct`; also `Union` for C/C++ unions | Direct-ish | SPUR intentionally folds some union-like syntax into `Struct` in current queries. |
| `Impl` | relationship evidence such as implementation/type membership; possibly descriptor `Meta` | Partial | SCIP has no generic "impl block" symbol kind. SPUR models Rust impl containers because they are useful for containment and method ownership. |
| `Trait` | `Kind::Trait`; also `TypeClass` or `Protocol` depending language | Direct-ish | Close match for Rust/Scala-style traits. |
| `Enum` | `Kind::Enum`; descriptor `Type` | Direct-ish | Close match. |
| `EnumVariant` | `Kind::EnumMember` | Direct-ish | Naming differs, concept matches. |
| `File` | `Document.relative_path`; `Kind::File` | Direct-ish | SCIP documents are file records; `Kind::File` also exists as a symbol kind. SPUR keeps files as graph nodes for traversal. |
| `External` | `Index.external_symbols`; any external `SymbolInformation.Kind` | Partial | SCIP externality is a placement/source property, not a distinct symbol kind. SPUR uses `External` nodes for unresolved packages, datasources, and out-of-graph targets. |
| `Method` | `Kind::Method`, `StaticMethod`, `AbstractMethod`, `TraitMethod`, etc.; descriptor `Method` | Direct-ish | SCIP has more method sub-kinds. SPUR keeps one method node kind and uses relation/provenance for resolution. |
| `Field` | `Kind::Field`, `Property`, `StaticField`, `StaticDataMember`, `StaticProperty` | Partial | SCIP splits fields, properties, and static data more finely. SPUR keeps a broad data-member node kind. |
| `Constant` | `Kind::Constant`, `StaticVariable`, `Value`, possibly `Variable` | Partial | SCIP distinguishes constants, values, static variables, and ordinary variables. SPUR avoids local variables and uses `Constant` for durable top-level or member constants. |
| `TypeAlias` | `Kind::TypeAlias`; descriptor `Type` or `Meta` depending indexer | Direct-ish | Close match. |
| `Macro` | `Kind::Macro`; descriptor `Macro` | Direct-ish | Close match. |
| `Section` | no dedicated kind; can be modeled as document structure, `Namespace`, or markup symbol by an indexer | Partial | SPUR intentionally models Markdown/document sections as graph nodes for docs navigation and agent context. SCIP has documents and occurrences, but no normative section kind. |
| `Commit` | no source-code symbol; SCIP has `Language::Git_Commit` for documents | None | SPUR's temporal graph includes commit nodes. SCIP indexes source artifacts, not git history lineage. |
| `McpTool` | none | None | SPUR models MCP tools as repository/workflow facts, outside SCIP's code-index scope. |
| `Cell` | no dedicated kind; possible virtual `Document` per cell by convention | Partial | SPUR notebook cells are first-class containers for lineage and cell-scoped symbols. SCIP has no notebook-cell ontology. |
| `Port` | none | None | SPUR ports are data-app lineage concepts, not source symbols. |

## RelationKind Mapping

| SPUR `RelationKind` | SCIP equivalent | Mapping | Notes |
|---|---|---:|---|
| `Imports` | `Occurrence.symbol_roles` with `Import` | Direct-ish | SCIP marks import occurrences. SPUR emits graph edges for import traversal and dependency reasoning. |
| `Calls` | symbol occurrence references, `SyntaxKind::IdentifierFunction`, enclosing ranges | Partial | SCIP has symbol references and ranges that can support call hierarchy, but no explicit normative `calls` relationship. SPUR keeps calls as first-class graph edges for blast-radius queries. |
| `Constructs` | `Kind::Constructor` and constructor/type occurrences | Partial | SCIP can represent constructor symbols and constructor references, but not a distinct construction edge. SPUR splits construction from ordinary calls. |
| `Contains` | descriptor ancestry, `SymbolInformation.enclosing_symbol`, document symbols, occurrence enclosing ranges | Partial | SCIP encodes hierarchy through symbol descriptors and ranges. SPUR keeps `contains` as a transitive graph predicate and preserves the lexical spine. |
| `Implements` | `Relationship.is_implementation` | Direct-ish | Close match for find-implementations semantics. SPUR also uses domain/range guards and language-specific syntactic heuristics. |
| `Defines` | `Occurrence` with `SymbolRole::Definition`; `Relationship.is_definition` for definition override cases | Direct-ish | SCIP definition occurrences map closely. SPUR keeps `defines` as a graph relation from file/container to symbol. |
| `References` | reference `Occurrence`; `Relationship.is_reference`; read/write roles where available | Direct-ish | SCIP references are occurrence-centered. SPUR references include HOF captures and notebook facts, with `GraphEdgeKind` and `bind_method` describing sub-mode/provenance. |
| `Extends` | no dedicated relationship; can be represented as references plus implementation/reference relationships by indexer convention | Partial | SCIP has no explicit `extends` boolean in `Relationship`. SPUR intentionally keeps inheritance/extension distinct from implementation. |
| `Links` | none beyond document text/occurrences by indexer convention | None | SPUR Markdown links are graph edges for docs navigation. |
| `Touches` | none | None | SPUR temporal commit-to-file/symbol facts are outside SCIP's source-index model. |
| `Produces` | none | None | Notebook/data-app lineage relation, intentionally SPUR-specific. |
| `Consumes` | none | None | Notebook/data-app lineage relation, intentionally SPUR-specific. |
| `Binds` | none | None | Frontend/data-app binding relation, intentionally SPUR-specific. |
| `Emits` | none | None | Frontend/data-app event/value relation, intentionally SPUR-specific. |

## GraphEdgeKind Mapping

| SPUR `GraphEdgeKind` | SCIP equivalent | Mapping | Notes |
|---|---|---:|---|
| `Calls` | function/method reference occurrence, often syntax-highlighted as function identifier | Partial | SCIP does not distinguish call edges from other references as a relationship kind. |
| `CallsDyn` | no dedicated equivalent | None | Dynamic call provenance is SPUR-specific edge evidence. |
| `ReferencesHof` | reference occurrence; maybe enclosing range or syntax kind by indexer convention | Partial | SCIP can record the referenced symbol and range, but not "higher-order-function reference" as a standard sub-kind. |
| `ReferencesOther` | reference occurrence; `Relationship.is_reference` where used | Direct-ish | Closest to general SCIP reference occurrences. |

## SPUR-Specific Concepts Not In SCIP

These are intentional differences, not missing SCIP parity items:

- Notebook lineage: `Cell`, `Port`, `Produces`, `Consumes`, `Binds`, and
  `Emits` model data-app workflows that SCIP does not attempt to standardize.
- Temporal code memory: `Commit` and `Touches` let SPUR answer history and
  rename/change-lineage questions from graph traversal.
- MCP/workflow graph facts: `McpTool` nodes make agent tools queryable beside
  code and docs.
- Markdown/document navigation: `Section` and `Links` are optimized for agent
  context assembly across specs, plans, and docs.
- Edge provenance: `GraphEdgeKind`, `bind_method`, `confidence`, and
  unresolved `target_label` are SPUR's way to expose syntax-first uncertainty.
  SCIP indexes may be compiler-backed or heuristic, but the proto does not
  standardize SPUR's provenance vocabulary.

## SCIP Concepts Not First-Class In SPUR

These are the main SCIP capabilities SPUR lacks today:

- Local symbols and scoped locals: descriptor `Local`, `Kind::Variable`, and
  local `enclosing_symbol`.
- Parameters and type parameters: descriptor suffixes `Parameter` and
  `TypeParameter`, plus `Kind::Parameter`, `SelfParameter`, `ThisParameter`,
  `MethodReceiver`, and `TypeParameter`.
- Fine-grained callable/member kinds: `Constructor`, `Operator`, `Getter`,
  `Setter`, `StaticMethod`, `TraitMethod`, `StaticField`, `StaticVariable`,
  `Property`, and related language-specific method/member categories.
- Occurrence roles: read/write access, generated code, test code, and forward
  definitions.
- Signature model: `signature_documentation` with occurrences inside the
  signature text.
- Type-definition relationship: `Relationship.is_type_definition`.
- Syntax highlighting and diagnostics: `SyntaxKind`, `Diagnostic`, severities,
  and tags.
- Package manager/name/version identity in every global symbol.

The roadmap's stance applies: absence from SPUR is not automatically a gap. A
SCIP concept becomes relevant only when a SPUR workflow needs it and the
extractor can provide trustworthy evidence.

## Workflow Gap Assessment

| Workflow | Current SPUR strength | High-value SCIP-backed gap | Impact | Risk |
|---|---|---|---:|---:|
| Agent code exploration | Broad symbol kinds, containment, definitions, imports, calls, docs sections, and graph MCP tools are already useful. | Occurrence roles and local/scope evidence could reduce ambiguity inside large functions. | Medium | Medium-high |
| Blast-radius/refactor impact | First-class `calls`, `constructs`, `references`, `implements`, `extends`, and unresolved target labels make impact queries cheap. | Read/write access, overrides, type-definition relationships, and constructor/operator precision would improve state and API impact analysis. | High | High |
| Review and dependency reasoning | `imports`, `extends`, `implements`, temporal `touches`, and notebook facts support review setup and dependency summaries. | Generated/test roles, public export/re-export facts, signature/parameter metadata, and package-version identity would improve API review. | High | Medium-high |
| Notebook lineage/data apps | SPUR is stronger than SCIP here because cells, ports, datasources, binds, emits, produces, and consumes are first-class. | SCIP offers little normative help; do not force these concepts into SCIP-shaped symbols. | High for SPUR, none for SCIP parity | Low |
| IDE-grade go-to-definition/reference precision | SPUR can answer repository-level navigation, but is not an IDE semantic index. | SCIP's occurrence roles, local symbols, descriptor grammar, typed ranges, signatures, diagnostics, and type-definition relationships are closer to IDE needs. | High if SPUR targets IDE behavior | High |

Ranked high-value gaps:

| Rank | Gap | Workflow impact | Implementation risk | Recommended tier |
|---:|---|---|---|---|
| 1 | Read/write/access evidence on existing `references` facts | High for blast-radius and review of mutable state | Medium; syntax-only can work for simple assignments but needs negative fixtures | Tier 1 metadata first, possible Tier 2 sub-kind later |
| 2 | Override/type-definition relationship evidence | High for OO/trait refactor impact and IDE-grade references | High; syntax-only evidence is weak in several languages | Tier 3 unless a language has explicit reliable syntax |
| 3 | Signature, parameter, and type-parameter metadata | High for API review and go-to-definition precision | Medium-high due cardinality and stable-ID rules | Tier 2 metadata first; first-class nodes only after benchmark proof |
| 4 | Generated/test/forward-definition roles | Medium-high for review filtering and impact ranking | Medium; often path/config/convention driven | Tier 1 role metadata or query annotations |
| 5 | Package manager/name/version identity for external symbols | Medium for dependency reasoning | Medium; requires resolver/package adapters | Tier 2 external identity enrichment |
| 6 | Local variables as first-class graph nodes | Low for broad exploration, high only for IDE/local dataflow | Very high cardinality and poor durable identity | Reject for Tier 2; reconsider only as opt-in Tier 3 scoped facts |

## Recommendations

### Borrow Now Without Schema Expansion

1. Use SCIP's occurrence-role vocabulary as a benchmark taxonomy for existing
   SPUR facts: definition, import, read, write, generated, test, and forward
   definition. This should start in fixtures, benchmark reports, or optional
   metadata, not as new `RelationKind` variants.

2. Use SCIP's descriptor suffixes as review vocabulary when evaluating stable
   identity: namespace, type, term, method, type parameter, parameter, local,
   macro, and meta. This helps reason about whether a candidate deserves a
   durable SPUR node.

3. Use SCIP's `Relationship` booleans as query-behavior vocabulary:
   reference-equivalent, implementation-equivalent, type-definition, and
   definition-alias behavior. SPUR should borrow the behaviors where useful,
   not the exact representation.

### Near-Term Ontology Additions

Recommended near-term first-class `NodeKind` or `RelationKind` additions: none.

The roadmap should finish Tier-1 relation-quality hardening before expanding
the enum vocabulary. The closest near-term candidate is access evidence, but it
should begin as syntax-query improvement or edge metadata on `references`, not a
new predicate.

Acceptance criteria before promoting any access evidence beyond metadata:

- Query fixtures cover reads, writes, compound assignments, destructuring,
  field/member access, address/reference taking, and obvious negative cases for
  each target language.
- Integration tests prove emitted facts retain the original `RelationKind` and
  expose access provenance without breaking existing `references` queries.
- SQL or MCP benchmark queries show improved blast-radius or review ranking
  compared with current `references` alone.
- Graph-size and latency measurements stay within an agreed budget.
- Artifact/index versioning and closed-enum client behavior are documented if
  any serialized enum vocabulary changes.

### Defer

- `Parameter`, `TypeParameter`, and signature facts: keep as signature/scope
  metadata until API-review benchmarks prove first-class value and stable-ID
  rules are written.
- `Overrides` or type-definition edges: defer to Tier 3 for compiler/LSP/indexer
  evidence except where a language has explicit, reliable syntax and negative
  fixtures.
- Constructor, operator, getter, setter, static-member, and property sub-kinds:
  keep under existing `constructs`, `calls`, `method`, `field`, `constant`, or
  metadata until workflow-specific queries show material improvement.
- External package manager/name/version identity: defer until dependency
  reasoning needs package-aware joins and resolver adapters exist.

### Reject For SPUR Tier-0/Tier-2

- Broad SCIP parity as a goal.
- First-class local-variable nodes for general navigation.
- Replacing SPUR notebook/data-app lineage with SCIP-shaped symbols.
- Replacing `contains` with descriptor-only hierarchy.
- Treating syntax highlighting or diagnostics as required graph ontology.

## Syntax-Query Improvements Versus Schema Changes

Syntax-query improvements that do not require new first-class ontology variants:

- Add more precise `spur-edges.scm` captures for existing relations.
- Improve `target_label`, `bind_method`, `confidence`, and
  `confidence_score`.
- Add benchmark-only role labels that mirror SCIP `SymbolRole`.
- Record read/write or type-use evidence as metadata on existing
  `references` edges.
- Add negative fixtures and relation coverage rows in
  `crates/spur-graph/queries/README.md`.

Schema or ontology changes that require explicit promotion review:

- New `NodeKind`, `RelationKind`, or `GraphEdgeKind` variants.
- Persisted edge fields that change artifact compatibility.
- Stable-ID rules for parameters, locals, type parameters, or generated symbols.
- New domain/range, cardinality, transitivity, or inverse-label semantics.
- Any change that requires graph artifact/index versioning or client migration.

## Bottom Line

SCIP confirms the obvious maturity gap: SPUR is not yet an IDE-grade occurrence
index, and it lacks many fine-grained symbol categories. The comparison also
confirms the more important design point: SPUR should not copy SCIP wholesale.

SPUR should borrow SCIP's vocabulary for evaluating gaps, especially occurrence
roles, descriptor suffixes, and relationship behaviors. It should keep
notebook lineage, temporal facts, MCP tools, sections, and uncertainty
provenance as SPUR-specific graph concepts. Future ontology expansion should
stay workflow-backed, benchmarked, and versioned.
