-- ============================================
-- Pre-built Analytics Queries
-- ============================================
--
-- These are parameterized queries that the AnalyticsEngine
-- prepares and executes via the DuckDB Rust API.

-- --------------------------------------------
-- Daily Cost Report
-- --------------------------------------------
-- Params: $start_date, $end_date (DATE)
SELECT
    strftime(timestamp, '%Y-%m-%d') AS day,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cache_read_tokens) AS cache_read_tokens,
    SUM(cache_creation_tokens) AS cache_creation_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd
FROM all_events_with_cost
WHERE timestamp >= $start_date AND timestamp < $end_date
GROUP BY day, agent
ORDER BY day DESC, cost_usd DESC;

-- --------------------------------------------
-- Weekly Cost Report
-- --------------------------------------------
-- Params: $start_date, $end_date (DATE)
SELECT
    strftime(timestamp, '%Y-%W') AS week,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd
FROM all_events_with_cost
WHERE timestamp >= $start_date AND timestamp < $end_date
GROUP BY week, agent
ORDER BY week DESC, cost_usd DESC;

-- --------------------------------------------
-- Monthly Cost Report
-- --------------------------------------------
-- Params: $start_date, $end_date (DATE)
SELECT
    strftime(timestamp, '%Y-%m') AS month,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd
FROM all_events_with_cost
WHERE timestamp >= $start_date AND timestamp < $end_date
GROUP BY month, agent
ORDER BY month DESC, cost_usd DESC;

-- --------------------------------------------
-- Model Breakdown
-- --------------------------------------------
SELECT
    model,
    agent,
    COUNT(*) AS requests,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    ROUND(AVG(computed_cost_usd), 6) AS avg_cost,
    ROUND(SUM(computed_cost_usd), 4) AS total_cost
FROM all_events_with_cost
WHERE model IS NOT NULL
GROUP BY model, agent
ORDER BY total_cost DESC;

-- --------------------------------------------
-- Project Breakdown
-- --------------------------------------------
SELECT
    COALESCE(project, '(none)') AS project,
    agent,
    COUNT(DISTINCT session_id) AS sessions,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd
FROM all_events_with_cost
GROUP BY project, agent
ORDER BY cost_usd DESC;

-- --------------------------------------------
-- Session Detail
-- --------------------------------------------
-- Params: $session_id (VARCHAR)
SELECT
    session_id,
    agent,
    model,
    MIN(timestamp) AS started_at,
    MAX(timestamp) AS ended_at,
    MAX(timestamp) - MIN(timestamp) AS duration,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cache_read_tokens) AS cache_read_tokens,
    SUM(cache_creation_tokens) AS cache_creation_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
    COUNT(*) AS events
FROM all_events_with_cost
WHERE session_id = $session_id
GROUP BY session_id, agent, model;

-- --------------------------------------------
-- Live Session Snapshot
-- --------------------------------------------
-- Params: $session_id (VARCHAR)
-- Fast query for a single active session.
SELECT
    session_id,
    agent,
    model,
    MIN(timestamp) AS started_at,
    MAX(timestamp) AS last_activity,
    SUM(input_tokens) AS input_tokens,
    SUM(output_tokens) AS output_tokens,
    SUM(cache_read_tokens) AS cache_read_tokens,
    SUM(cache_creation_tokens) AS cache_creation_tokens,
    ROUND(SUM(computed_cost_usd), 4) AS cost_usd,
    COUNT(*) AS events
FROM all_events_with_cost
WHERE session_id = $session_id
GROUP BY session_id, agent, model;
