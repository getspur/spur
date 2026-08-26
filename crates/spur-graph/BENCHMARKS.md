# spur-graph Benchmarks

How `spur-graph` retrieves over conversational long-term memory, measured with
the same public datasets Graphify reports: **LoCoMo** and **LongMemEval-S**.

Phase 1 (in-tree) is **retrieval-only**: ingest sessions as markdown, extract a
deterministic AST/markdown graph (zero LLM credits), then seed + expand at
`k=10`. Phase 2 (QA accuracy / key-fact coverage) needs an external reader and
judge and is not claimed here until that harness is run.

Constants: `sol_5f73941594ed4d15`, `sol_bca716ccfbdb404d`.

## Results at a glance

| Suite | Dataset (n) | Metric | spur-graph | Notes |
|---|---|---|---|---|
| Memory | LoCoMo official (1540) | recall@10 | run harness | drop adversarial category 5 (446 of 1986) |
| Memory | LoCoMo Graphify-sized (300) | recall@10 | run harness | first 300 non-adversarial IDs; not Graphify's unpublished sample |
| Memory | LongMemEval-S official retrieval (470) | recall@10 | run harness | skip 30 abstention (`*_abs`) |
| Memory | LongMemEval-S Graphify-sized (50) | recall@10 | run harness | first 50 retrieval IDs |
| Cost | graph build | LLM credits | 0 | markdown + tree-sitter extract |

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
