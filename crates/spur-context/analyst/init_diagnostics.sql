CREATE OR REPLACE VIEW diagnostics AS SELECT * FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/diagnostics.parquet');
