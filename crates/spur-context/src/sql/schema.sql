-- ============================================
-- SPUR Context Engine — Base Schema
-- ============================================
--
-- Agent convert views are created at runtime by AnalyticsEngine,
-- which checks whether agent data directories exist before
-- creating read_json_auto() views. If a directory is missing,
-- an empty stub view is created instead.

-- --------------------------------------------
-- 1. PRICING TABLE (loaded from Rust registry)
-- --------------------------------------------
CREATE TABLE IF NOT EXISTS pricing (
    model VARCHAR PRIMARY KEY,
    input_price_per_1m DOUBLE,
    output_price_per_1m DOUBLE,
    cache_read_price_per_1m DOUBLE,
    cache_creation_price_per_1m DOUBLE,
    effective_from DATE DEFAULT '2020-01-01',
    effective_to DATE DEFAULT NULL
);

-- --------------------------------------------
-- 2. UNIFIED EVENT VIEW (placeholder)
-- --------------------------------------------
-- This will be replaced after agent views are created.
CREATE OR REPLACE VIEW all_events AS
SELECT
    NULL::TIMESTAMP AS timestamp,
    NULL::VARCHAR AS session_id,
    NULL::VARCHAR AS agent,
    NULL::VARCHAR AS model,
    NULL::VARCHAR AS project,
    0::BIGINT AS input_tokens,
    0::BIGINT AS output_tokens,
    0::BIGINT AS cache_read_tokens,
    0::BIGINT AS cache_creation_tokens,
    NULL::DOUBLE AS cost_usd
WHERE FALSE;

-- --------------------------------------------
-- 3. COST-ENRICHED EVENT VIEW
-- --------------------------------------------
CREATE OR REPLACE VIEW all_events_with_cost AS
SELECT
    e.*,
    COALESCE(
        e.cost_usd,
        (e.input_tokens * p.input_price_per_1m / 1000000.0)
        + (e.output_tokens * p.output_price_per_1m / 1000000.0)
        + (e.cache_read_tokens * p.cache_read_price_per_1m / 1000000.0)
        + (e.cache_creation_tokens * p.cache_creation_price_per_1m / 1000000.0)
    ) AS computed_cost_usd
FROM all_events e
LEFT JOIN pricing p
    ON lower(e.model) = lower(p.model)
    AND e.timestamp >= p.effective_from
    AND (p.effective_to IS NULL OR e.timestamp < p.effective_to);
