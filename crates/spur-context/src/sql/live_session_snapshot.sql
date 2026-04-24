-- Params: ?1 session_id.
-- Same aggregation shape as session_detail.sql but optimized for live
-- polling (no duration column). See P0.8 note in session_detail.sql.
SELECT
    session_id,
    any_value(agent) AS agent,
    string_agg(DISTINCT model, ',' ORDER BY model) AS models,
    strftime(MIN(timestamp), '%Y-%m-%dT%H:%M:%S') AS started_at,
    strftime(MAX(timestamp), '%Y-%m-%dT%H:%M:%S') AS last_activity,
    COALESCE(SUM(input_tokens), 0) AS input_tokens,
    COALESCE(SUM(output_tokens), 0) AS output_tokens,
    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
    COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd,
    COUNT(*) AS events
FROM all_events_with_cost
WHERE session_id = ?
GROUP BY session_id;
