-- Params: none.
SELECT
    COALESCE(project, '(none)') AS project,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    COALESCE(SUM(input_tokens), 0) AS input_tokens,
    COALESCE(SUM(output_tokens), 0) AS output_tokens,
    COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS cost_usd
FROM all_events_with_cost
GROUP BY project, agent
ORDER BY cost_usd DESC;
