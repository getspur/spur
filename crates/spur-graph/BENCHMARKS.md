# spur-graph Benchmarks

How `spur-graph` retrieves over conversational long-term memory, measured with
the same public datasets Graphify reports: **LoCoMo** and **LongMemEval-S**.

Phase 1 is **retrieval** (top-`k` unique turn/session ids at `k=10`). Phase 2
is **extractive QA**: the reader concatenates every retrieved session's section
texts (no LLM) and a deterministic judge scores Graphify key-fact coverage
(`coverage = (covered + 0.5 * partial) / total`).

Constants: `sol_5f73941594ed4d15`, `sol_bca716ccfbdb404d`, `sol_805e26de169b45b3`.
Haystack isolation: `sol_4dcbe9f970c04f3d` (isolated hits pass) /
`sol_e63aad30cf0e4844` (foreign haystack hit is
`data_integrity.foreign_key.violation`).
Retrieval policy `sol_07a8eb8af5064466` (Z3 Optimize, lex): `topk_ids`,
`full_session_text`, `seed_k = hit_k = 10`, no LLM.

## Results at a glance

Measured 2026-08-26. Reader is extractive, not an LLM. Retrieval is scoped to
the task haystack (LoCoMo conversation directory; LongMemEval per-question
directory).

- LoCoMo `locomo10.json` SHA-256 `79fa87e90f040813…` (CC BY-NC). n=1536 after
  dropping adversarial category 5 and empty-evidence items (1986 − 446 − 4).
- LongMemEval-S cleaned `longmemeval_s_cleaned.json` SHA-256 `d6f21ea9d60a0d56…`
  (MIT). n=470 after skipping 30 abstention (`*_abs`) items.

| Suite | Dataset (n) | Metric | spur-graph | Field |
|---|---|---|---|---|
| Memory | LoCoMo official (1536) | recall@10 | **0.393** | Graphify graph-expand 0.497 (n=300) |
| Memory | LoCoMo official (1536) | extractive coverage | **45.8%** | Graphify LLM QA 45.3% (n=300) |
| Memory | LoCoMo Graphify-sized (300) | recall@10 | run harness | first 300 non-adversarial IDs |
| Memory | LongMemEval-S official (470) | recall@10 | **0.865** | Graphify graph-expand 0.844 (n=50) |
| Memory | LongMemEval-S official (470) | extractive coverage | **65.0%** | Graphify LLM QA 76% (n=50) |
| Memory | LongMemEval-S Graphify-sized (50) | recall@10 | run harness | first 50 retrieval IDs |
| Cost | graph build | LLM credits | **0** | markdown + tree-sitter extract |

LoCoMo extractive QA breakdown: covered 264, partial 879, miss 393.
LongMemEval-S extractive QA breakdown: covered 199, partial 213, miss 58.

Graphify's published LongMemEval row is a 50-question English slice with an LLM
reader/judge. The official 470 row here is session-level recall plus extractive
coverage on the full cleaned retrieval split.

## Harness

```
ingest sessions → markdown worktree → spur-graph extract → top-k unique ids@10 → recall@10
```

- LoCoMo gold: QA `evidence` dialog IDs (`D1:3`).
- LongMemEval gold: `answer_session_ids` (session-level).
- Adversarial LoCoMo (category 5) is excluded. LongMemEval abstention is skipped
  for retrieval, matching the official retrieval eval.
- Hits for a task must come from that task's haystack directory. Mixed-root
  graphs are not scored.

## Fairness rules

- `k = 10` on every quality row.
- Hits are the top-`k` unique turn/session ids by max term score in the
  isolated haystack (not 3-seed BFS). QA concatenates every section of those
  ids.
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
curl -fL https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_s_cleaned.json \
  -o .spur/memory-eval/longmemeval_s_cleaned.json

SPUR_LOCOMO_JSON="$PWD/.spur/memory-eval/locomo10.json" \
  scripts/spur-cargo test -p spur-graph --release --test memory_eval locomo_official_from_env \
  -- --ignored --exact --nocapture

SPUR_LONGMEMEVAL_JSON="$PWD/.spur/memory-eval/longmemeval_s_cleaned.json" \
  scripts/spur-cargo test -p spur-graph --release --test memory_eval longmemeval_official_from_env \
  -- --ignored --exact --nocapture
```

Fill this table from the printed `RetrievalReport` / `QaReport`; do not invent
cells.
