-- Params: ?1 session_id.
SELECT
    session_id,
    agent,
    model,
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
GROUP BY session_id, agent, model;
