-- Search-token adapter for temporal analyst builds.
--
-- init_search.sql depends on v_search_symbol_tokens instead of reaching
-- directly into symbol_snapshots, so non-temporal builds can provide a static
-- adapter without creating temporal-looking views.

CREATE OR REPLACE VIEW v_search_symbol_tokens AS
SELECT stable_symbol_id, tokens
FROM symbol_snapshots
WHERE stable_symbol_id IS NOT NULL;
