# Codex Cache Semantics — P0.4 Audit
- Date: 2026-04-24
- Data source used: real Codex sessions `~/.codex/sessions/*/*/*/*.jsonl` via `/tmp/codex_token_count_audit.jsonl`
- Rows queried: 13499
- Verdict: SUBSET
- Query output:
  sum_in = 311104510361
  sum_out = 1325399374
  sum_total = 312429909735
  sum_cached = 299244124032
- Interpretation: Across 13,499 real `event_msg` + `payload.type=token_count` rows, `sum_in + sum_out` exactly equals `sum_total`, while adding `sum_cached` overshoots by 299,244,124,032. This shows `cached_input_tokens` is already included inside `input_tokens`, not a separate additive bucket.
- Implication for P0.4 (Task 3 of the plan): P0.4 is real; adding both `input_tokens` and `cached_input_tokens` double-counts Codex cached input.
