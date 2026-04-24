-- Params: ?1 session_id.
-- P0.8: aggregate across all models used in the session. A session that
-- switched model mid-run (e.g. opus-4 → sonnet-4) was previously split
-- into two rows by `GROUP BY session_id, agent, model`, and the Rust
-- consumer took only the first via `rows.next()`, silently dropping the
-- other model's tokens and cost. Drop `model` from the GROUP BY and
-- surface the distinct-model list via string_agg so downstream still
-- knows which models ran.
SELECT
    session_id,
    any_value(agent) AS agent,
    string_agg(DISTINCT model, ',' ORDER BY model) AS models,
    strftime(MIN(timestamp), '%Y-%m-%dT%H:%M:%S') AS started_at,
    strftime(MAX(timestamp), '%Y-%m-%dT%H:%M:%S') AS ended_at,
    EXTRACT(EPOCH FROM (MAX(timestamp) - MIN(timestamp)))::BIGINT AS duration_seconds,
    COALESCE(SUM(input_tokens), 0) AS input_tokens,
    COALESCE(SUM(output_tokens), 0) AS output_tokens,
    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
    COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd,
    COUNT(*) AS events
FROM all_events_with_cost
WHERE session_id = ?
GROUP BY session_id;
