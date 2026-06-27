-- Schema for all DuckLake tables. Run after ATTACH to ensure all tables
-- exist (the catalog may have been created with a partial schema).
-- CREATE TABLE IF NOT EXISTS is idempotent. ALTER TABLE SET PARTITIONED BY
-- is also idempotent in DuckLake (no-op if already partitioned).

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
    row_counts JSON
);
ALTER TABLE package_catalog SET PARTITIONED BY (source, package);

CREATE TABLE IF NOT EXISTS refs (
    source VARCHAR,
    package VARCHAR,
    ref_name VARCHAR,
    revision VARCHAR,
    updated_at TIMESTAMP
);
ALTER TABLE refs SET PARTITIONED BY (source, package);
