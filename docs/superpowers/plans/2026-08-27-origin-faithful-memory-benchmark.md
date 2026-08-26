# Origin-Faithful Memory Benchmark Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-27-origin-faithful-memory-benchmark-design.ipynb`
**Formal @spec cells:** `4a894784-6acc-4577-a020-2bc52afc4354`, `44a9f3f1-1cec-47f2-9bfd-70d47aa4bce8`, `05ba89a3-86e8-4598-a824-0f82e70e01a6`, `3bd36132-3268-4f85-b93c-172c52af6f54`
**Design epic:** `bd-1gyf` (closed)

**Goal:** Replace the current lexical-label benchmark with a lossless, origin-faithful LoCoMo and LongMemEval benchmark that separately measures retrieval, graph traversal value, and end-to-end QA.

**Architecture:** Parse both datasets into one canonical, occurrence-scoped record model before constructing any index. Non-oracle rankers receive a leakage-safe query/corpus view, write immutable ranking artifacts, and are scored independently at turn and session granularity. QA replays the origin-native contracts from those frozen rankings, while an artifact state machine prevents incomplete or invalid runs from being published as full results.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `sha2`, `chrono`, `tokio`, workspace `reqwest`, `rust-stemmers`, native `spur-graph` node/edge types, and repository build wrapper `scripts/spur-cargo`.

---

## File structure map

| File | Responsibility |
|---|---|
| `crates/spur-graph/src/memory_eval/contract.rs` | Canonical dataset records, source pins, eligibility, validation findings, contract IDs |
| `crates/spur-graph/src/memory_eval/locomo.rs` | Lossless LoCoMo source adapter and evidence resolution |
| `crates/spur-graph/src/memory_eval/longmemeval.rs` | Lossless LongMemEval adapter with independent session/turn gold |
| `crates/spur-graph/src/memory_eval/ranking.rs` | Leakage-safe ranker interface, oracle/recent/BM25 implementations, ranking artifact model |
| `crates/spur-graph/src/memory_eval/memory_graph.rs` | Question-blind memory graph construction, graph-index-only and traversal rankers |
| `crates/spur-graph/src/memory_eval/metrics.rs` | LoCoMo and LongMemEval retrieval metrics and per-slice aggregation |
| `crates/spur-graph/src/memory_eval/artifacts.rs` | Manifest, validation, rankings, metrics, QA cache, checksums, release state machine |
| `crates/spur-graph/src/memory_eval/qa.rs` | Origin-native prompts/scorers, resumable backend contract, OpenAI reader/judge |
| `crates/spur-graph/src/bin/memory_benchmark.rs` | CLI orchestration, cost/credential gates, retrieval-only and full runs |
| `crates/spur-graph/tests/memory_eval_*.rs` | Focused contract, ranker, metric, artifact, QA, runner, and full-data gates |
| `crates/spur-graph/BENCHMARKS.md` | Reproduction commands and generated-result interpretation only |

The current `materialize.rs`, `retrieve.rs`, and monolithic `tests/memory_eval.rs` remain until the new runner passes its integration tests. They are removed in Task 12 so no intermediate commit breaks the crate.

## Dependency DAG

```text
task-1
├── task-2 ──┬── task-4 ──────────────┐
│            └── task-9 ── task-10 ───┤
├── task-3 ────── task-4              │
│    └──────────────── task-10        │
├── task-5 ──┬── task-6 ──────────────┤
│            ├── task-7 ── task-8 ────┤
│            ├── task-8               │
│            └── task-9               │
└── task-7                            │
                                      ▼
                                   task-11
                                      │
                                   task-12
                                      │
                                   task-13
```

Solver evidence:

- `sol_1714a5290a4840fb`: satisfiable optimized schedule with minimum `max_stage = 6`.
- `sol_daec5923587e430f`: unsatisfiable counterexample for any same-stage pair that shares `contract.rs`, `mod.rs`, `qa.rs`, or `Cargo.toml`.

---

### Task 1: Scaffold the canonical benchmark contract

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-graph/src/memory_eval/contract.rs`
- Create: `crates/spur-graph/src/memory_eval/ranking.rs`
- Create: `crates/spur-graph/src/memory_eval/memory_graph.rs`
- Create: `crates/spur-graph/src/memory_eval/artifacts.rs`
- Modify: `crates/spur-graph/src/memory_eval/mod.rs:1-69`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Canonical records retain raw JSON plus typed dates, roles, speakers, captions, answer flags, and both gold granularities.
- [ ] Internal session/turn IDs are occurrence-scoped and do not use a source ID as identity.
- [ ] New public modules compile alongside the legacy harness.
- [ ] `scripts/spur-cargo test -p spur-graph --lib memory_eval::contract` passes.

**Suggested Worker:** claude-code-acp for the public contract and compatibility boundary.

**Scope Boundary:**
- IN scope: canonical types, deterministic occurrence-ID helper, new module declarations.
- OUT of scope: dataset parsing, validation policy, ranking algorithms, artifact I/O.
- If an existing public benchmark function must be removed to compile, emit `scope_drift`; Task 12 owns removal.

**Implementation:**

- [ ] **Step 1: Write unit tests inside `contract.rs`**

```rust
#[test]
fn occurrence_ids_distinguish_repeated_source_sessions() {
    assert_ne!(
        occurrence_id("longmemeval", "q1", 0, "shared"),
        occurrence_id("longmemeval", "q1", 1, "shared")
    );
}

#[test]
fn canonical_turn_keeps_raw_and_typed_content() {
    let turn = fixture_turn_with_caption_and_date();
    assert_eq!(turn.role, Role::Assistant);
    assert_eq!(turn.content, "line one\nline two");
    assert_eq!(turn.caption.as_deref(), Some("a blue bicycle"));
    assert!(turn.raw.get("content").is_some());
}
```

- [ ] **Step 2: Verify the tests fail**

Run: `scripts/spur-cargo test -p spur-graph --lib memory_eval::contract -- --nocapture`

Expected: FAIL because the canonical types and helpers do not exist.

- [ ] **Step 3: Add the canonical types without deleting `MemoryTask`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkDataset {
    pub kind: DatasetKind,
    pub source: SourcePin,
    pub conversations: Vec<ConversationRecord>,
    pub questions: Vec<QuestionRecord>,
    pub raw_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    pub internal_id: String,
    pub source_id: Option<String>,
    pub role: Role,
    pub speaker: Option<String>,
    pub content: String,
    pub caption: Option<String>,
    pub has_answer: Option<bool>,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionRecord {
    pub id: String,
    pub text: String,
    pub question_date: Option<String>,
    pub answer: serde_json::Value,
    pub category: Option<u32>,
    pub question_type: Option<String>,
    pub evidence: Vec<EvidenceRef>,
    pub gold_session_ids: Vec<String>,
    pub gold_turn_ids: Vec<String>,
    pub raw: serde_json::Value,
}
```

Expose `contract`, `ranking`, `memory_graph`, `artifacts`, and `qa` as public submodules. Leave the new non-contract files empty except for module documentation so this commit remains compilable.

- [ ] **Step 4: Run focused and legacy tests**

Run: `scripts/spur-cargo test -p spur-graph --lib memory_eval::contract`

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval`

Expected: PASS for both commands.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/{contract,ranking,memory_graph,artifacts,mod}.rs
git commit -m "feat(spur-graph): task-1 scaffold memory benchmark contract"
```

---

### Task 2: Implement the lossless LoCoMo adapter

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/locomo.rs:1-47`
- Create: `crates/spur-graph/tests/memory_eval_locomo.rs`

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] All conversations, sessions, turns, speakers, dates, captions, QA categories, answers, adversarial fields, and raw fields survive parsing.
- [ ] Evidence resolves by `dia_id`; malformed evidence remains raw with `resolved_turn_id = None`.
- [ ] Category 5 and evidence-free questions remain in the QA records.
- [ ] Retrieval eligibility is not decided by dropping rows inside the parser.

**Suggested Worker:** codex for a focused adapter with fixture-driven tests.

**Scope Boundary:**
- IN scope: LoCoMo JSON decoding and evidence resolution.
- OUT of scope: official-count assertions, retrieval scoring, prompt rendering.
- If source fields not represented by Task 1 are discovered, emit `scope_drift` before changing `contract.rs`.

**Implementation:**

- [ ] **Step 1: Add a fixture containing every risky field**

```rust
#[test]
fn locomo_preserves_multiline_caption_speaker_date_and_adversarial_rows() {
    let data = load_locomo(LOCOMO_ALL_FIELDS, test_pin()).unwrap();
    assert_eq!(data.questions.len(), 2);
    assert_eq!(data.conversations[0].sessions[0].occurred_at.as_deref(), Some("2023-01-01"));
    assert_eq!(data.conversations[0].sessions[0].turns[0].speaker.as_deref(), Some("Alice"));
    assert_eq!(data.conversations[0].sessions[0].turns[0].caption.as_deref(), Some("race photo"));
    assert_eq!(data.conversations[0].sessions[0].turns[0].content, "line one\nline two");
    assert_eq!(data.questions[1].category, Some(5));
}

#[test]
fn locomo_keeps_unresolved_evidence_as_a_finding_input() {
    let data = load_locomo(LOCOMO_MALFORMED_EVIDENCE, test_pin()).unwrap();
    assert_eq!(data.questions[0].evidence[0].raw, "D9:missing");
    assert!(data.questions[0].evidence[0].resolved_turn_id.is_none());
}
```

- [ ] **Step 2: Verify the adapter tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_locomo -- --nocapture`

Expected: FAIL because `load_locomo` and the lossless fields are absent.

- [ ] **Step 3: Implement `load_locomo`**

```rust
pub fn load_locomo(json: &str, source: SourcePin) -> anyhow::Result<BenchmarkDataset> {
    let raw: serde_json::Value = serde_json::from_str(json)?;
    let samples = decode_samples(&raw)?;
    let conversations = samples.iter().map(canonical_conversation).collect::<Result<_, _>>()?;
    let questions = resolve_questions(&samples, &conversations)?;
    Ok(BenchmarkDataset::new(DatasetKind::Locomo, source, conversations, questions, json))
}
```

Keep `parse_locomo` as a compatibility wrapper until Task 12.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_locomo`

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/locomo.rs crates/spur-graph/tests/memory_eval_locomo.rs
git commit -m "feat(spur-graph): task-2 add lossless LoCoMo adapter"
```

---

### Task 3: Implement the lossless LongMemEval adapter

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/longmemeval.rs:1-36`
- Create: `crates/spur-graph/tests/memory_eval_longmemeval.rs`

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] Parallel haystack IDs, dates, and sessions are length-checked and parsed without truncation.
- [ ] User and assistant roles, multiline content, `has_answer`, question date, type, answer, and raw fields survive.
- [ ] Repeated source session IDs at different occurrences receive different internal IDs.
- [ ] Session-level `answer_session_ids` and turn-level `has_answer` gold remain independent.
- [ ] All 500 questions, including `_abs`, remain in the canonical dataset.

**Suggested Worker:** codex for a focused adapter with deterministic occurrence tests.

**Scope Boundary:**
- IN scope: LongMemEval JSON decoding and two independent gold views.
- OUT of scope: deciding which view is more correct, retrieval metrics, paid judge calls.
- If parallel arrays disagree, return a typed parse error; do not zip to the shorter length.

**Implementation:**

- [ ] **Step 1: Write adapter tests**

```rust
#[test]
fn longmem_preserves_roles_dates_multiline_and_assistant_answer_turns() {
    let data = load_longmemeval(LME_ALL_FIELDS, test_pin()).unwrap();
    let q = &data.questions[0];
    assert_eq!(q.question_date.as_deref(), Some("2024-02-01"));
    assert_eq!(q.gold_session_ids.len(), 1);
    assert_eq!(q.gold_turn_ids.len(), 1);
    let answer_turn = data.turn(&q.gold_turn_ids[0]).unwrap();
    assert_eq!(answer_turn.role, Role::Assistant);
    assert_eq!(answer_turn.content, "first line\nsecond line");
}

#[test]
fn repeated_source_session_ids_remain_distinct_occurrences() {
    let data = load_longmemeval(LME_DUPLICATE_SESSION_ID, test_pin()).unwrap();
    let sessions = data.all_sessions().collect::<Vec<_>>();
    assert_eq!(sessions[0].source_id, sessions[1].source_id);
    assert_ne!(sessions[0].internal_id, sessions[1].internal_id);
    assert_ne!(sessions[0].occurred_at, sessions[1].occurred_at);
}
```

- [ ] **Step 2: Verify the adapter tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_longmemeval -- --nocapture`

Expected: FAIL because the lossless adapter does not exist.

- [ ] **Step 3: Implement checked parallel-array parsing**

```rust
ensure!(
    item.haystack_session_ids.len() == item.haystack_sessions.len()
        && item.haystack_sessions.len() == item.haystack_dates.len(),
    "parallel haystack arrays differ for {}",
    item.question_id
);
```

Generate turn IDs from question ID, session occurrence index, and turn index. Resolve `answer_session_ids` to every matching occurrence only through an explicit resolver that reports ambiguity in raw provenance.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_longmemeval`

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/longmemeval.rs crates/spur-graph/tests/memory_eval_longmemeval.rs
git commit -m "feat(spur-graph): task-3 add lossless LongMemEval adapter"
```

---

### Task 4: Enforce source validation and eligibility contracts

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/contract.rs`
- Create: `crates/spur-graph/tests/memory_eval_validation.rs`

**Depends on:** `task-2`, `task-3`

**Acceptance Criteria:**
- [ ] Fatal findings cover hash/schema failures, duplicate internal IDs, and broken parallel arrays.
- [ ] Known source defects are nonfatal findings with question IDs and explicit eligibility effects.
- [ ] Eligibility produces LoCoMo retrieval/QA and LongMemEval retrieval/QA cohorts separately.
- [ ] Audited and compatibility contract IDs cannot be merged into one report.

**Suggested Worker:** claude-code-acp for cross-dataset invariants.

**Scope Boundary:**
- IN scope: validation types, cohort construction, source-hash verification.
- OUT of scope: ranking, metric computation, automatic source repair.
- Any transform that changes source content requires a distinct compatibility contract ID.

**Implementation:**

- [ ] **Step 1: Write failing validation tests**

```rust
#[test]
fn eligibility_keeps_qa_but_excludes_unresolved_locomo_evidence_from_retrieval() {
    let report = validate_dataset(&locomo_with_one_bad_evidence(), &audited_contract());
    assert!(!report.has_fatal());
    assert_eq!(report.cohorts.locomo_qa, vec!["q-good", "q-bad", "q-adversarial"]);
    assert_eq!(report.cohorts.locomo_retrieval, vec!["q-good"]);
}

#[test]
fn duplicate_internal_ids_are_fatal() {
    let report = validate_dataset(&dataset_with_duplicate_turn_ids(), &audited_contract());
    assert!(report.fatal.iter().any(|f| f.code == "duplicate_internal_id"));
}

#[test]
fn contract_ids_refuse_blended_aggregation() {
    assert!(ensure_same_contract(&audited_contract(), &compatibility_contract()).is_err());
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_validation -- --nocapture`

Expected: FAIL because validation and cohort reports are absent.

- [ ] **Step 3: Implement validation**

```rust
pub struct ValidationReport {
    pub contract_id: ContractId,
    pub fatal: Vec<ValidationFinding>,
    pub findings: Vec<ValidationFinding>,
    pub cohorts: Cohorts,
}

pub fn validate_dataset(dataset: &BenchmarkDataset, contract: &BenchmarkContract) -> ValidationReport;
```

Use the exact eligibility predicates from formal cell `SECTION-1-SOURCE-ELIGIBILITY`: LoCoMo retrieval requires nonempty, fully resolved evidence; LongMemEval retrieval excludes abstention but QA includes every question.

- [ ] **Step 4: Run focused tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_validation`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/contract.rs crates/spur-graph/tests/memory_eval_validation.rs
git commit -m "feat(spur-graph): task-4 enforce memory benchmark contracts"
```

---

### Task 5: Add leakage-safe oracle, recent, and BM25 rankers

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/ranking.rs`
- Create: `crates/spur-graph/tests/memory_eval_ranking.rs`

**Depends on:** `task-1`

**Acceptance Criteria:**
- [ ] Non-oracle `RankRequest` cannot contain answer, type, evidence, `has_answer`, or answer-revealing IDs.
- [ ] Oracle uses a separate API that accepts scorer-only gold.
- [ ] Recent and BM25 return unique occurrence IDs with deterministic ties.
- [ ] Turn and session rankings are separate artifacts with a declared `k`.
- [ ] Query, corpus serialization, and tokenization hashes are recorded.

**Suggested Worker:** claude-code-acp for the type-level leakage boundary and deterministic BM25.

**Scope Boundary:**
- IN scope: ranker trait, ranking records, oracle/recent/BM25.
- OUT of scope: graph construction/traversal and metric aggregation.
- Non-oracle code must not receive a `QuestionRecord`; pass only `RankRequest`.

**Implementation:**

- [ ] **Step 1: Write ranker contract tests**

```rust
#[test]
fn non_oracle_request_serialization_contains_no_gold_fields() {
    let json = serde_json::to_string(&fixture_rank_request()).unwrap();
    for forbidden in ["answer", "evidence", "has_answer", "question_type", "answer_session"] {
        assert!(!json.contains(forbidden), "leaked {forbidden}: {json}");
    }
}

#[test]
fn bm25_ties_are_stable_and_top_k_ids_are_unique() {
    let ranker = Bm25Ranker::build(fixture_corpus()).unwrap();
    let first = ranker.rank(&fixture_rank_request(), 3).unwrap();
    let second = ranker.rank(&fixture_rank_request(), 3).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.hits.iter().map(|h| &h.occurrence_id).collect::<BTreeSet<_>>().len(), 3);
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_ranking -- --nocapture`

Expected: FAIL because the ranker interface is empty.

- [ ] **Step 3: Implement the interfaces and baselines**

```rust
pub trait Ranker {
    fn variant(&self) -> Variant;
    fn rank(&self, request: &RankRequest<'_>, k: usize) -> anyhow::Result<Ranking>;
}

pub struct RankRequest<'a> {
    pub question_id: &'a str,
    pub query: &'a str,
    pub granularity: Granularity,
    pub corpus: &'a [CorpusDocument],
}

pub struct Ranking {
    pub question_id: String,
    pub variant: Variant,
    pub granularity: Granularity,
    pub k: usize,
    pub hits: Vec<RankedHit>,
    pub query_sha256: String,
    pub corpus_sha256: String,
    pub serialization_sha256: String,
}

pub type RankingSet = BTreeMap<(String, Variant, Granularity), Ranking>;

pub fn oracle_ranking(request: &OracleRequest<'_>, k: usize) -> Ranking;
```

Implement BM25 directly over the shared normalized token stream so no variant-specific serialization can enter the experiment. Break equal scores by occurrence ID.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_ranking`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/ranking.rs crates/spur-graph/tests/memory_eval_ranking.rs
git commit -m "feat(spur-graph): task-5 add memory retrieval baselines"
```

---

### Task 6: Build the question-blind memory graph and graph rankers

**Task ID:** `task-6`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/memory_graph.rs`
- Create: `crates/spur-graph/tests/memory_eval_graph.rs`

**Depends on:** `task-5`

**Acceptance Criteria:**
- [ ] Graph construction receives canonical corpus records but no questions or gold fields.
- [ ] Every graph result resolves to one canonical turn/session occurrence before scoring.
- [ ] Graph-index-only performs no query-time edge traversal.
- [ ] Graph-traversal uses an explicit, manifest-recorded config with no hidden defaults.
- [ ] `k` counts unique provenance occurrences rather than graph nodes.
- [ ] Traversal and indexing build/query timing plus index size are exposed.

**Suggested Worker:** claude-code-acp for graph construction and provenance invariants.

**Scope Boundary:**
- IN scope: session/turn/speaker/chronology nodes and edges, provenance map, graph rankers.
- OUT of scope: question-derived entity insertion, final metric selection, tuning on official evaluation questions.
- If a new relation depends on a question or gold label, emit `risk` and stop.

**Implementation:**

- [ ] **Step 1: Write graph-value isolation tests**

```rust
#[test]
fn graph_is_identical_before_and_after_questions_are_attached() {
    let corpus = fixture_corpus();
    let without_questions = MemoryGraph::build(corpus.records()).unwrap();
    let with_questions = MemoryGraph::build(corpus.with_questions().records()).unwrap();
    assert_eq!(without_questions.content_hash(), with_questions.content_hash());
}

#[test]
fn traversal_returns_unique_provenance_not_internal_nodes() {
    let graph = fixture_graph();
    let config = TraversalConfig { seed_k: 2, max_depth: 2, relations: allowed_relations() };
    let ranking = GraphTraversalRanker::new(graph, config).rank(&fixture_rank_request(), 5).unwrap();
    assert!(ranking.hits.iter().all(|hit| hit.provenance_id.is_some()));
    assert_eq!(ranking.hits.iter().map(|h| &h.occurrence_id).collect::<BTreeSet<_>>().len(), ranking.hits.len());
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_graph -- --nocapture`

Expected: FAIL because graph construction and rankers are absent.

- [ ] **Step 3: Implement graph construction and both variants**

```rust
pub struct TraversalConfig {
    pub seed_k: usize,
    pub max_depth: usize,
    pub relations: BTreeSet<MemoryRelation>,
}

pub enum MemoryRelation { Contains, NextTurn, PreviousTurn, SpokenBy }

pub struct MemoryGraph {
    pub facts: GraphFacts,
    provenance: HashMap<NodeId, String>,
}
```

Use the same BM25 scores as Task 5 for seed ordering. `GraphIndexOnlyRanker` stops after node scoring. `GraphTraversalRanker` expands only configured relations, then ranks unique provenance occurrences by seed score, distance, and occurrence ID. Require config on the CLI; selection is calibrated on a development partition in Task 13.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_graph`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/memory_graph.rs crates/spur-graph/tests/memory_eval_graph.rs
git commit -m "feat(spur-graph): task-6 add memory graph retrieval variants"
```

---

### Task 7: Implement origin-faithful retrieval metrics

**Task ID:** `task-7`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/metrics.rs:1-26`
- Create: `crates/spur-graph/tests/memory_eval_metrics.rs`

**Depends on:** `task-1`, `task-5`

**Acceptance Criteria:**
- [ ] LoCoMo reports macro evidence Recall@1/5/10 and all-evidence-hit diagnostics.
- [ ] LongMemEval reports Recall-All and NDCG at 5/10, turn-level at 50, and diagnostic Recall-Any.
- [ ] Session and turn gold are never substituted.
- [ ] Every aggregate carries numerator, denominator, exclusions, and per-category/type slices.
- [ ] Metrics are finite and bounded; Recall@k is monotonic; Recall-All is no greater than Recall-Any.

**Suggested Worker:** codex for pure deterministic metric functions and property tests.

**Scope Boundary:**
- IN scope: scoring immutable rankings.
- OUT of scope: creating rankings, cross-dataset combined scores, QA accuracy.

**Implementation:**

- [ ] **Step 1: Add metric property and golden tests**

```rust
#[test]
fn recall_is_monotonic_and_all_is_bounded_by_any() {
    let gold = ["a", "b"];
    let hits = ["a", "x", "b"];
    assert!(recall_at_k(&gold, &hits, 1) <= recall_at_k(&gold, &hits, 3));
    assert!(recall_all_at_k(&gold, &hits, 3) <= recall_any_at_k(&gold, &hits, 3));
}

#[test]
fn ndcg_uses_graded_binary_relevance_and_exact_denominator() {
    let metric = ndcg_at_k(&["a", "b"], &["a", "x", "b"], 3);
    assert!((metric - 0.919720789).abs() < 1e-9);
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_metrics -- --nocapture`

Expected: FAIL because Recall-All, Recall-Any, NDCG, and aggregate reports are absent.

- [ ] **Step 3: Implement metric reports**

```rust
pub struct MetricValue {
    pub value: f64,
    pub numerator: u64,
    pub denominator: u64,
}

pub struct RetrievalMetrics {
    pub dataset: DatasetKind,
    pub granularity: Granularity,
    pub variant: Variant,
    pub overall: BTreeMap<String, MetricValue>,
    pub slices: BTreeMap<String, BTreeMap<String, MetricValue>>,
    pub exclusions: Vec<String>,
}
```

Reject empty unexpected denominators and non-finite values rather than serializing them.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_metrics`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/metrics.rs crates/spur-graph/tests/memory_eval_metrics.rs
git commit -m "feat(spur-graph): task-7 add origin retrieval metrics"
```

---

### Task 8: Persist immutable artifacts and enforce release states

**Task ID:** `task-8`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/artifacts.rs`
- Create: `crates/spur-graph/tests/memory_eval_artifacts.rs`

**Depends on:** `task-4`, `task-5`, `task-7`

**Acceptance Criteria:**
- [ ] Writer produces the approved manifest, validation, ranking, metric, QA-cache, report, and checksum layout.
- [ ] Writes are atomic and rankings are immutable once their SHA-256 is recorded.
- [ ] Retrieval publication requires every Section 4 gate.
- [ ] Full publication additionally requires complete QA.
- [ ] API failures retain the question denominator and leave the run resumable as `QaPending`.

**Suggested Worker:** claude-code-acp for artifact consistency and lifecycle enforcement.

**Scope Boundary:**
- IN scope: artifact schemas, atomic writer, hashes, release-state transitions.
- OUT of scope: running rankers or calling an API.
- Do not weaken fatal validation to make a fixture publishable.

**Implementation:**

- [ ] **Step 1: Write lifecycle and layout tests**

```rust
#[test]
fn qa_pending_cannot_publish_full() {
    let mut run = valid_retrieval_run();
    run.transition(RunEvent::QaPending).unwrap();
    assert!(run.transition(RunEvent::PublishFull).is_err());
}

#[test]
fn artifact_writer_hashes_every_published_file() {
    let root = tempfile::tempdir().unwrap();
    write_fixture_run(root.path()).unwrap();
    let sums = std::fs::read_to_string(root.path().join("SHA256SUMS")).unwrap();
    for required in ["manifest.json", "validation.json", "report.md"] {
        assert!(sums.contains(required));
    }
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_artifacts -- --nocapture`

Expected: FAIL because the artifact writer and state machine are empty.

- [ ] **Step 3: Implement state and writer**

```rust
pub enum RunState { Validated, RetrievalComplete, PublishedRetrieval, QaPending, QaComplete, PublishedFull }

pub struct RunManifest {
    pub run_id: String,
    pub repository_revision: String,
    pub repository_dirty: bool,
    pub sources: Vec<SourcePin>,
    pub contract_id: ContractId,
    pub ranking_hashes: BTreeMap<Variant, String>,
    pub model: Option<String>,
    pub prompt_hashes: BTreeMap<String, String>,
    pub command: Vec<String>,
}
```

Write to a sibling temporary file, `sync_all`, rename, and only then update `SHA256SUMS`. Encode the exact `SECTION-4-RELEASE-GATE` branches.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_artifacts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/artifacts.rs crates/spur-graph/tests/memory_eval_artifacts.rs
git commit -m "feat(spur-graph): task-8 persist auditable memory benchmark runs"
```

---

### Task 9: Implement origin-native LoCoMo QA

**Task ID:** `task-9`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/qa.rs:1-119`
- Modify: `crates/spur-graph/Cargo.toml:16-64`
- Create: `crates/spur-graph/tests/memory_eval_qa_locomo.rs`

**Depends on:** `task-2`, `task-5`

**Acceptance Criteria:**
- [ ] Prompt includes retrieved dates, speakers, complete turn content, and captions.
- [ ] Category 1 uses multi-answer F1; categories 2–4 use stemmed token F1; category 5 uses binary adversarial scoring.
- [ ] Released `adversarial_answer` is mapped by an explicitly named compatibility shim.
- [ ] Adversarial option order is deterministic from a recorded seed.
- [ ] QA consumes a frozen ranking and cannot rerank it.

**Suggested Worker:** claude-code-acp for evaluator parity and prompt fidelity.

**Scope Boundary:**
- IN scope: LoCoMo prompt rendering, scoring, backend trait, deterministic adversarial shim.
- OUT of scope: LongMemEval judge and HTTP implementation.
- Add `rust-stemmers` only for parity with the origin stemmed scorer; do not replace the scoring contract.

**Implementation:**

- [ ] **Step 1: Add golden prompt and scorer tests**

```rust
#[test]
fn locomo_prompt_contains_date_speaker_caption_and_full_multiline_text() {
    let prompt = render_locomo_prompt(&fixture_question(), &frozen_ranking(), &fixture_dataset()).unwrap();
    insta::assert_snapshot!(prompt);
}

#[test]
fn locomo_category_scorers_match_origin_golden_cases() {
    assert_eq!(score_locomo(1, "Alice; Bob", json!(["Alice", "Bob"])), 1.0);
    assert_eq!(score_locomo(2, "running races", json!("ran a race")), 1.0);
    assert_eq!(score_locomo(5, "no", json!("no")), 1.0);
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_qa_locomo -- --nocapture`

Expected: FAIL because the origin-native prompt and scorers are absent.

- [ ] **Step 3: Implement the frozen-ranking QA contract**

```rust
pub struct QaRequest {
    pub question_id: String,
    pub prompt: String,
    pub prompt_sha256: String,
    pub ranking_sha256: String,
}

pub struct QaResponse {
    pub output_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub enum QaStatus { Complete, Pending }

pub trait QaBackend {
    fn complete(&mut self, request: &QaRequest) -> anyhow::Result<QaResponse>;
}

pub fn evaluate_locomo(
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
    backend: &mut dyn QaBackend,
    seed: u64,
) -> anyhow::Result<Vec<QaRecord>>;
```

Store the ranking hash in every `QaRecord`. Reject records whose ranking hash differs from the run manifest.

Add `rust-stemmers = "1.2.0"` to `crates/spur-graph/Cargo.toml` and commit the resulting lockfile update.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_qa_locomo`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/qa.rs crates/spur-graph/Cargo.toml crates/spur-graph/tests/memory_eval_qa_locomo.rs Cargo.lock
git commit -m "feat(spur-graph): task-9 add origin LoCoMo QA"
```

---

### Task 10: Implement LongMemEval QA, judge, cache, and paid backend

**Task ID:** `task-10`

**Files:**
- Modify: `crates/spur-graph/src/memory_eval/qa.rs`
- Modify: `crates/spur-graph/Cargo.toml`
- Create: `crates/spur-graph/tests/memory_eval_qa_longmem.rs`

**Depends on:** `task-3`, `task-9`

**Acceptance Criteria:**
- [ ] LongMemEval prompt serializes chronological JSON history with both roles, session dates, and question date.
- [ ] Reader and judge model are pinned to `gpt-4o-2024-08-06` for the audited contract.
- [ ] Backend uses `POST /v1/responses`, `store: false`, parses `output_text`, and records token usage.
- [ ] Reader hypotheses and complete judge inputs/outputs are cached by question, prompt, model, and ranking hashes.
- [ ] Missing credentials, budget exhaustion, incomplete responses, and HTTP failures return `QaPending`; they never create a label or drop a denominator.
- [ ] Tests use a fake backend and make no network requests.

**Suggested Worker:** claude-code-acp for the paid integration seam and resume semantics.

**Scope Boundary:**
- IN scope: LongMemEval prompt/judge, OpenAI Responses API adapter, cache/resume, cost ceiling.
- OUT of scope: changing the model pin, automatic retry without a bounded policy, publishing incomplete aggregates.
- The API reference is `https://developers.openai.com/api/reference/cli/resources/responses/methods/create`; keep the HTTP shape aligned with it.

**Implementation:**

- [ ] **Step 1: Add fake-backend and failure tests**

```rust
#[test]
fn longmem_prompt_is_chronological_and_keeps_both_roles_and_dates() {
    let request = build_longmem_reader_request(&fixture_longmem(), &frozen_ranking()).unwrap();
    insta::assert_snapshot!(request.input);
}

#[test]
fn missing_key_and_api_failure_leave_qa_pending_without_a_label() {
    let mut backend = FakeBackend::fail("network unavailable");
    let result = evaluate_longmem(&fixture_longmem(), &frozen_rankings(), &mut backend, &budget()).unwrap();
    assert_eq!(result.status, QaStatus::Pending);
    assert!(result.records[0].label.is_none());
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_qa_longmem -- --nocapture`

Expected: FAIL because LongMemEval QA and the paid backend are absent.

- [ ] **Step 3: Implement the backend and cache key**

```rust
#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    input: &'a [ResponseInput],
    store: bool,
}

pub struct QaCacheKey {
    pub question_id: String,
    pub ranking_sha256: String,
    pub prompt_sha256: String,
    pub model: String,
}
```

Send `Authorization: Bearer <OPENAI_API_KEY>`, require completed status and nonempty top-level `output_text`, and persist `usage.input_tokens`, `usage.output_tokens`, and `usage.total_tokens`. Check the declared maximum requests and currency/token ceiling before every request.

Add `reqwest = { workspace = true }` to `crates/spur-graph/Cargo.toml`; use the existing Tokio dependency for the async HTTP boundary.

- [ ] **Step 4: Run tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_qa_longmem`

Expected: PASS with zero network calls.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/memory_eval/qa.rs crates/spur-graph/Cargo.toml crates/spur-graph/tests/memory_eval_qa_longmem.rs Cargo.lock
git commit -m "feat(spur-graph): task-10 add resumable LongMemEval QA"
```

---

### Task 11: Add the benchmark CLI and end-to-end runner

**Task ID:** `task-11`

**Files:**
- Create: `crates/spur-graph/src/bin/memory_benchmark.rs`
- Modify: `crates/spur-graph/Cargo.toml`
- Create: `crates/spur-graph/tests/memory_eval_runner.rs`

**Depends on:** `task-4`, `task-6`, `task-8`, `task-10`

**Acceptance Criteria:**
- [ ] `validate`, `retrieve`, `qa`, `resume`, and `report` subcommands operate on one run directory.
- [ ] Retrieval runs all five variants from one canonical dataset and freezes rankings before QA.
- [ ] `--paid-qa` additionally requires `--max-requests`, `--max-usd`, and `OPENAI_API_KEY`.
- [ ] Without paid authorization the command succeeds as retrieval-only and records `QaPending`.
- [ ] API failures never remove completed cache records or rankings.
- [ ] CLI records repository revision/dirty state, exact command, timing, peak RSS, index bytes, and context tokens.

**Suggested Worker:** claude-code-acp for multi-component orchestration.

**Scope Boundary:**
- IN scope: CLI parsing and composition of existing library components.
- OUT of scope: new ranking/scoring logic and editing benchmark results by hand.
- If the runner requires changing a library interface owned by another completed task, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add CLI behavior tests**

```rust
#[test]
fn retrieval_only_run_finishes_as_published_retrieval_and_qa_pending() {
    let output = run_fixture_cli(["retrieve", "--locomo", fixture_path(), "--output", run_dir()]);
    assert!(output.status.success());
    let manifest = read_manifest(run_dir());
    assert_eq!(manifest.state, RunState::PublishedRetrieval);
    assert_eq!(manifest.qa_state, Some(RunState::QaPending));
}

#[test]
fn paid_qa_requires_key_and_both_cost_bounds() {
    let output = run_fixture_cli(["qa", "--paid-qa", "--max-requests", "10"]);
    assert!(!output.status.success());
    assert!(stderr(output).contains("--max-usd"));
}
```

- [ ] **Step 2: Verify tests fail**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_runner -- --nocapture`

Expected: FAIL because the binary does not exist.

- [ ] **Step 3: Implement the command surface**

```rust
#[derive(clap::Subcommand)]
enum Command {
    Validate(DatasetArgs),
    Retrieve(RetrieveArgs),
    Qa(QaArgs),
    Resume(QaArgs),
    Report(RunArgs),
}
```

Open rankings read-only in `qa` and `resume`. The runner may truncate a frozen ranking to the declared `k`, but it must not reorder hits.

Add `clap = { workspace = true }` to `crates/spur-graph/Cargo.toml`.

- [ ] **Step 4: Run runner and crate tests**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_runner`

Run: `scripts/spur-cargo test -p spur-graph --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/bin/memory_benchmark.rs crates/spur-graph/Cargo.toml crates/spur-graph/tests/memory_eval_runner.rs Cargo.lock
git commit -m "feat(spur-graph): task-11 add memory benchmark runner"
```

---

### Task 12: Remove the legacy phase-1 harness

**Task ID:** `task-12`

**Files:**
- Delete: `crates/spur-graph/src/memory_eval/materialize.rs`
- Delete: `crates/spur-graph/src/memory_eval/retrieve.rs`
- Modify: `crates/spur-graph/src/memory_eval/mod.rs`
- Delete: `crates/spur-graph/tests/memory_eval.rs`

**Depends on:** `task-11`

**Acceptance Criteria:**
- [ ] Old `MemoryTask`, extractive answer-substring coverage, label parsing, Graphify slices, and compatibility constants are absent from the public audited API.
- [ ] New focused tests cover every retained behavior.
- [ ] No repository reference treats answer-substring coverage as QA accuracy.
- [ ] `scripts/spur-cargo test -p spur-graph` passes.

**Suggested Worker:** codex for a bounded mechanical removal after replacement coverage exists.

**Scope Boundary:**
- IN scope: deleting only the superseded memory-eval paths and exports.
- OUT of scope: unrelated graph extraction or benchmark crates.
- If another crate imports a legacy symbol, emit `scope_drift` with the exact caller before editing that crate.

**Implementation:**

- [ ] **Step 1: Add a negative source check to the runner test**

```rust
#[test]
fn audited_report_has_no_extractive_coverage_field() {
    let report = generated_fixture_report();
    assert!(!report.contains("extractive coverage"));
    assert!(!report.contains("coverage_milli"));
}
```

- [ ] **Step 2: Verify it fails against the legacy report path**

Run: `scripts/spur-cargo test -p spur-graph --test memory_eval_runner audited_report_has_no_extractive_coverage_field -- --nocapture`

Expected: FAIL while legacy report fields are still emitted.

- [ ] **Step 3: Delete the legacy files and exports**

Remove `MemoryTask`, `EvalSplit`, `RECALL_K`, Graphify slice constants, `grade_key_fact`, `extractive_qa`, `evaluate_tasks`, `materialize_*`, and `retrieve_*`. Keep only the audited contract modules.

- [ ] **Step 4: Run the full crate suite**

Run: `scripts/spur-cargo test -p spur-graph`

Expected: PASS with no ignored legacy full-data tests.

- [ ] **Step 5: Commit**

```bash
git add -A crates/spur-graph/src/memory_eval crates/spur-graph/tests/memory_eval.rs crates/spur-graph/tests/memory_eval_runner.rs
git commit -m "refactor(spur-graph): task-12 remove legacy memory benchmark"
```

---

### Task 13: Add pinned full-data gates and rewrite benchmark documentation

**Task ID:** `task-13`

**Files:**
- Create: `crates/spur-graph/tests/memory_eval_official.rs`
- Create: `crates/spur-graph/benchmarks/memory_eval.toml`
- Modify: `crates/spur-graph/BENCHMARKS.md:1-96`

**Depends on:** `task-12`

**Acceptance Criteria:**
- [ ] Full-data validation checks exact pinned hashes and audited counts before ranking.
- [ ] LoCoMo gates 10 samples, 272 sessions, 5,882 turns, 1,986 QA, and 1,973 retrieval-eligible questions.
- [ ] LongMemEval gates 500 QA, 470 retrieval questions, 30 abstention questions, 23,867 sessions, 246,750 turns, 886 answer turns, and 13 repeated source session IDs.
- [ ] Development calibration produces and freezes `memory_eval.toml` without inspecting official evaluation labels.
- [ ] Documentation uses the new CLI and distinguishes retrieval-only, `qa_pending`, full audited, compatibility, and smoke runs.
- [ ] No result cell is populated unless its run directory passes checksums and the release gate.

**Suggested Worker:** claude-code-acp for full-contract review and reproducibility documentation.

**Scope Boundary:**
- IN scope: ignored environment-backed full-data tests, frozen non-gold config, documentation.
- OUT of scope: downloading or committing CC BY-NC LoCoMo data, hand-editing scores, tuning on official gold.
- A pinned upstream incompatibility must be documented as `not_runnable`, not patched invisibly.

**Implementation:**

- [ ] **Step 1: Add full-data gates**

```rust
#[test]
#[ignore = "set SPUR_LOCOMO_JSON to the pinned locomo10.json"]
fn locomo_official_contract_counts_match() {
    let data = load_pinned_locomo_from_env();
    let report = validate_dataset(&data, &audited_contract());
    assert!(!report.has_fatal(), "{:#?}", report.fatal);
    assert_eq!(data.questions.len(), 1_986);
    assert_eq!(report.cohorts.locomo_retrieval.len(), 1_973);
}

#[test]
#[ignore = "set SPUR_LONGMEMEVAL_JSON to the pinned cleaned file"]
fn longmem_official_contract_counts_match() {
    let data = load_pinned_longmem_from_env();
    assert_eq!(data.questions.len(), 500);
    assert_eq!(data.all_sessions().count(), 23_867);
    assert_eq!(data.all_turns().count(), 246_750);
}
```

- [ ] **Step 2: Run fixture tests and the pinned tests when data are present**

Run: `scripts/spur-cargo test -p spur-graph`

Run when the pinned files exist:

```bash
SPUR_LOCOMO_JSON="$PWD/.spur/memory-eval/locomo10.json" \
SPUR_LONGMEMEVAL_JSON="$PWD/.spur/memory-eval/longmemeval_s_cleaned.json" \
scripts/spur-cargo test -p spur-graph --release --test memory_eval_official -- --ignored --nocapture
```

Expected: fixture suite PASS; full-data tests PASS only for the exact pinned bytes.

- [ ] **Step 3: Freeze a development-only graph configuration**

Write `memory_eval.toml` with explicit `seed_k`, `max_depth`, relations, tie policy, tokenization contract, bootstrap seed, and prompt/model hashes. The calibration report must list development question IDs and prove they do not overlap the official scoring cohort.

- [ ] **Step 4: Rewrite `BENCHMARKS.md`**

Document source revisions/hashes, commands, artifact layout, cohort denominators, metrics, variants, paid-QA guardrails, and interpretation rules. Link the design notebook and this implementation plan. Remove all old Graphify apples-to-oranges result rows.

- [ ] **Step 5: Run final verification**

Run: `scripts/spur-cargo fmt --check`

Run: `scripts/spur-cargo test -p spur-graph`

Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph --all-targets -- -D warnings`

Expected: all commands exit 0.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-graph/tests/memory_eval_official.rs crates/spur-graph/benchmarks/memory_eval.toml crates/spur-graph/BENCHMARKS.md
git commit -m "docs(spur-graph): task-13 publish audited memory benchmark workflow"
```

---

## Plan self-review

### Spec coverage

| Approved design requirement | Implementing tasks |
|---|---|
| Lossless records, occurrence IDs, source pins | 1–4 |
| LoCoMo 1,973 retrieval / 1,986 QA | 2, 4, 13 |
| LongMemEval 470 retrieval / 500 QA and independent gold | 3, 4, 7, 13 |
| Oracle/recent/BM25/graph-index/graph-traversal | 5–6 |
| No gold leakage and shared inputs | 5–6, 11 |
| Dataset-native retrieval metrics | 7 |
| Immutable rankings and artifact lifecycle | 8, 11 |
| Origin-native LoCoMo QA | 9 |
| Origin-native LongMemEval reader/judge | 10 |
| Missing credentials become `qa_pending` | 8, 10–11 |
| Full-data validation, config freeze, reproducibility docs | 13 |
| Remove misleading extractive coverage | 12–13 |

### DAG and scope checks

- Every task has a unique ID and explicit dependencies.
- The solver found a seven-stage feasible schedule and proved the encoded shared-file collision counterexample unsatisfiable.
- Tasks 2, 3, 5, and 7 can begin after Task 1 according to their own dependencies; Tasks 4, 6, and 9 follow independently once their inputs are approved.
- Shared-file sequences are ordered: `contract.rs` 1→4, `qa.rs` 9→10, `Cargo.toml` 9→10→11, `mod.rs` 1→12.
- Each task owns at most five files and includes an out-of-scope signal boundary.
- No implementation task is dispatched before the plan is submitted and its dependencies are approved.

### Required review evidence

For every completed task, the brain must inspect the task diff, run its listed verification command, and approve or reject through `review_task`. Workers do not close their own beads issues. The implementation epic closes only after Task 13 is approved and the final crate verification is fresh.
