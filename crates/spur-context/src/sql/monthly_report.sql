-- Params: ?1 start_date (inclusive), ?2 end_date (exclusive).
SELECT
    strftime(timestamp, '%Y-%m') AS month,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    COALESCE(SUM(input_tokens), 0) AS input_tokens,
    COALESCE(SUM(output_tokens), 0) AS output_tokens,
    COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
    COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation_tokens,
    COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd
FROM all_events_with_cost
WHERE timestamp >= CAST(? AS DATE) AND timestamp < CAST(? AS DATE)
GROUP BY month, agent
ORDER BY month DESC, cost_usd DESC;
