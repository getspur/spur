-- Schema for all DuckLake tables. Run after ATTACH to ensure all tables
-- exist (the catalog may have been created with a partial schema).
-- CREATE TABLE IF NOT EXISTS is idempotent. ALTER TABLE SET PARTITIONED BY
-- is also idempotent in DuckLake (no-op if already partitioned).

CREATE SCHEMA IF NOT EXISTS bronze;
CREATE SCHEMA IF NOT EXISTS silver;
CREATE SCHEMA IF NOT EXISTS gold;

CREATE TABLE IF NOT EXISTS bronze.raw_sources (
    source VARCHAR,
    package VARCHAR,
    version VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    source_kind VARCHAR,
    source_uri VARCHAR,
    artifact_s3_uri VARCHAR,
    content_sha256 VARCHAR,
    size_bytes UBIGINT,
    etag VARCHAR,
    fetched_at TIMESTAMP
);
ALTER TABLE bronze.raw_sources SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS silver.graph_artifacts (
    source VARCHAR,
    package VARCHAR,
    version VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    bronze_content_sha256 VARCHAR,
    builder_version VARCHAR,
    graph_content_hash VARCHAR,
    artifact_s3_prefix VARCHAR,
    manifest_uri VARCHAR,
    manifest_schema_hash VARCHAR,
    node_count UBIGINT,
    edge_count UBIGINT,
    file_count UBIGINT,
    embedding_count UBIGINT,
    built_at TIMESTAMP
);
ALTER TABLE silver.graph_artifacts SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS nodes (
    stable_symbol_id VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    file_path VARCHAR,
    byte_range_start INTEGER,
    byte_range_end INTEGER,
    line_start INTEGER,
    line_end INTEGER,
    entity_name VARCHAR,
    qualified_name VARCHAR,
    symbol_kind VARCHAR,
    anchor_hash VARCHAR,
    enclosing_scope VARCHAR
);
ALTER TABLE nodes SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS edges (
    source_stable_id VARCHAR,
    target_stable_id VARCHAR,
    target_package VARCHAR,
    target_label VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    relation VARCHAR,
    edge_kind VARCHAR,
    confidence VARCHAR,
    confidence_score DOUBLE,
    bind_method VARCHAR,
    receiver_text VARCHAR,
    scope_text VARCHAR
);
ALTER TABLE edges SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS edges_unresolved (
    source_stable_id VARCHAR,
    target_label VARCHAR,
    target_package VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    relation VARCHAR,
    edge_kind VARCHAR,
    confidence VARCHAR,
    confidence_score DOUBLE,
    bind_method VARCHAR,
    receiver_text VARCHAR,
    scope_text VARCHAR
);
ALTER TABLE edges_unresolved SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS files (
    stable_file_id VARCHAR,
    file_path VARCHAR,
    source_text VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER
);
ALTER TABLE files SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS file_manifests (
    stable_file_id VARCHAR,
    path VARCHAR,
    content_oid VARCHAR,
    node_ids VARCHAR[],
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER
);
ALTER TABLE file_manifests SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS section_bodies (
    section_id VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    file_path VARCHAR,
    title VARCHAR,
    body_text VARCHAR,
    body_hash VARCHAR,
    token_count INTEGER,
    vector FLOAT[],
    embedding_model VARCHAR,
    embedding_input_hash VARCHAR,
    embed_text_version VARCHAR
);
ALTER TABLE section_bodies ADD COLUMN IF NOT EXISTS vector FLOAT[];
ALTER TABLE section_bodies ADD COLUMN IF NOT EXISTS embedding_model VARCHAR;
ALTER TABLE section_bodies ADD COLUMN IF NOT EXISTS embedding_input_hash VARCHAR;
ALTER TABLE section_bodies ADD COLUMN IF NOT EXISTS embed_text_version VARCHAR;
ALTER TABLE section_bodies SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS symbol_embeddings (
    stable_symbol_id VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    file_path VARCHAR,
    entity_name VARCHAR,
    qualified_name VARCHAR,
    symbol_kind VARCHAR,
    embedding FLOAT[],
    embedding_model VARCHAR,
    embedding_input_hash VARCHAR,
    embed_text_version VARCHAR
);
ALTER TABLE symbol_embeddings SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS commits (
    sha VARCHAR,
    parents VARCHAR[],
    author_time BIGINT,
    author_name VARCHAR,
    author_email VARCHAR,
    summary VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER
);
ALTER TABLE commits SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS symbol_snapshots (
    stable_symbol_id VARCHAR,
    commit VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    file_path VARCHAR,
    entity_name VARCHAR,
    symbol_kind VARCHAR,
    enclosing_scope VARCHAR,
    byte_range INTEGER[],
    line_range INTEGER[],
    anchor_hash VARCHAR
);
ALTER TABLE symbol_snapshots SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS temporal_edges (
    source_endpoint VARCHAR,
    target_endpoint VARCHAR,
    relation VARCHAR,
    change_kind VARCHAR,
    parent VARCHAR,
    package VARCHAR,
    source VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER
);
ALTER TABLE temporal_edges SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS package_catalog (
    source VARCHAR,
    package VARCHAR,
    revision VARCHAR,
    revision_kind VARCHAR,
    semver_major INTEGER,
    semver_minor INTEGER,
    semver_patch INTEGER,
    snapshot_id BIGINT,
    indexed_at TIMESTAMP,
    index_status VARCHAR,
    embeddings_status VARCHAR,
    row_counts JSON,
    generation BIGINT,
    bronze_content_sha256 VARCHAR,
    silver_graph_content_hash VARCHAR,
    builder_version VARCHAR,
    translate_schema_version VARCHAR
);
ALTER TABLE package_catalog ADD COLUMN IF NOT EXISTS generation BIGINT;
ALTER TABLE package_catalog ADD COLUMN IF NOT EXISTS bronze_content_sha256 VARCHAR;
ALTER TABLE package_catalog ADD COLUMN IF NOT EXISTS silver_graph_content_hash VARCHAR;
ALTER TABLE package_catalog ADD COLUMN IF NOT EXISTS builder_version VARCHAR;
ALTER TABLE package_catalog ADD COLUMN IF NOT EXISTS translate_schema_version VARCHAR;
ALTER TABLE package_catalog SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS refs (
    source VARCHAR,
    package VARCHAR,
    ref_name VARCHAR,
    revision VARCHAR,
    updated_at TIMESTAMP
);
ALTER TABLE refs SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS gold.nodes AS SELECT * FROM nodes LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.edges AS SELECT * FROM edges LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.edges_unresolved AS SELECT * FROM edges_unresolved LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.files AS SELECT * FROM files LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.file_manifests AS SELECT * FROM file_manifests LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.section_bodies AS SELECT * FROM section_bodies LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.symbol_embeddings AS SELECT * FROM symbol_embeddings LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.commits AS SELECT * FROM commits LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.symbol_snapshots AS SELECT * FROM symbol_snapshots LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.temporal_edges AS SELECT * FROM temporal_edges LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.package_catalog AS SELECT * FROM package_catalog LIMIT 0;
CREATE TABLE IF NOT EXISTS gold.refs AS SELECT * FROM refs LIMIT 0;

ALTER TABLE gold.nodes SET PARTITIONED BY (source, package);
ALTER TABLE gold.edges SET PARTITIONED BY (source, package);
ALTER TABLE gold.edges_unresolved SET PARTITIONED BY (source, package);
ALTER TABLE gold.files SET PARTITIONED BY (source, package);
ALTER TABLE gold.file_manifests SET PARTITIONED BY (source, package);
ALTER TABLE gold.section_bodies SET PARTITIONED BY (source, package);
ALTER TABLE gold.symbol_embeddings SET PARTITIONED BY (source, package);
ALTER TABLE gold.commits SET PARTITIONED BY (source, package);
ALTER TABLE gold.symbol_snapshots SET PARTITIONED BY (source, package);
ALTER TABLE gold.temporal_edges SET PARTITIONED BY (source, package);
ALTER TABLE gold.package_catalog ADD COLUMN IF NOT EXISTS generation BIGINT;
ALTER TABLE gold.package_catalog ADD COLUMN IF NOT EXISTS bronze_content_sha256 VARCHAR;
ALTER TABLE gold.package_catalog ADD COLUMN IF NOT EXISTS silver_graph_content_hash VARCHAR;
ALTER TABLE gold.package_catalog ADD COLUMN IF NOT EXISTS builder_version VARCHAR;
ALTER TABLE gold.package_catalog ADD COLUMN IF NOT EXISTS translate_schema_version VARCHAR;
ALTER TABLE gold.package_catalog SET PARTITIONED BY (source, package);
ALTER TABLE gold.refs SET PARTITIONED BY (source, package);
