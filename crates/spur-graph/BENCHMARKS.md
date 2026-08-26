# spur-graph Benchmarks

How `spur-graph` retrieves over conversational long-term memory, measured with
the same public datasets Graphify reports: **LoCoMo** and **LongMemEval-S**.

Phase 1 is **retrieval** (seed + expand at `k=10`). Phase 2 is **extractive QA**:
the reader concatenates the retrieved section texts (no LLM) and a deterministic
judge scores Graphify key-fact coverage
(`coverage = (covered + 0.5 * partial) / total`).

Constants: `sol_5f73941594ed4d15`, `sol_bca716ccfbdb404d`, `sol_805e26de169b45b3`.

## Results at a glance

Measured 2026-08-26 on `locomo10.json` SHA-256 `79fa87e90f040813…` (CC BY-NC).
n=1536 after dropping adversarial category 5 and empty-evidence items
(1986 − 446 − 4). Reader is extractive, not an LLM.

| Suite | Dataset (n) | Metric | spur-graph | Field |
|---|---|---|---|---|
| Memory | LoCoMo official (1536) | recall@10 | **0.340** | Graphify graph-expand 0.497 |
| Memory | LoCoMo official (1536) | extractive coverage | **42.4%** | Graphify LLM QA 45.3% |
| Memory | LoCoMo Graphify-sized (300) | recall@10 | run harness | first 300 non-adversarial IDs |
| Memory | LongMemEval-S official retrieval (470) | recall@10 | run harness | skip 30 abstention (`*_abs`) |
| Memory | LongMemEval-S Graphify-sized (50) | recall@10 | run harness | first 50 retrieval IDs |
| Cost | graph build | LLM credits | **0** | markdown + tree-sitter extract |

LoCoMo extractive QA breakdown: covered 235, partial 834, miss 467.

## Harness

```
ingest sessions → markdown worktree → spur-graph extract → search+expand@10 → recall@10
```

- LoCoMo gold: QA `evidence` dialog IDs (`D1:3`).
- LongMemEval gold: `answer_session_ids` (session-level).
- Adversarial LoCoMo (category 5) is excluded. LongMemEval abstention is skipped
  for retrieval, matching the official retrieval eval.

## Fairness rules

- `k = 10` on every quality row.
- Same materialize + extract path for seed-only vs expand (expand is the default).
- Graph build uses no LLM. Do not vendor `locomo10.json` (CC BY-NC 4.0); fetch
  at run time and record SHA-256.

## Reproducing (tiny fixtures)

```bash
scripts/spur-cargo test -p spur-graph --test memory_eval
```

The integration tests use synthetic LoCoMo / LongMemEval JSON. They do not
download the public corpora.

## Reproducing (full public datasets)

```bash
mkdir -p .spur/memory-eval
curl -fsSL https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json \
  -o .spur/memory-eval/locomo10.json
# LongMemEval-S cleaned (MIT): huggingface.co/datasets/xiaowu0162/longmemeval-cleaned
```

Wire those files through `spur_graph::memory_eval::{parse_locomo, materialize_locomo, retrieve_seed_expand}`
with `EvalSplit::Official` and `EvalSplit::Graphify`. Fill this table from the
returned `RetrievalReport`; do not invent cells.
