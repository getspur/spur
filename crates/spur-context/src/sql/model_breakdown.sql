-- Params: none.
SELECT
    model,
    agent,
    COUNT(*) AS requests,
    COALESCE(SUM(input_tokens), 0) AS input_tokens,
    COALESCE(SUM(output_tokens), 0) AS output_tokens,
    COALESCE(ROUND(AVG(computed_cost_usd), 6), 0.0) AS avg_cost,
    COALESCE(ROUND(SUM(computed_cost_usd), 4), 0.0) AS total_cost
FROM all_events_with_cost
WHERE model IS NOT NULL
GROUP BY model, agent
ORDER BY total_cost DESC;
