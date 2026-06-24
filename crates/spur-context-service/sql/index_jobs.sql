CREATE TABLE IF NOT EXISTS index_jobs (
    job_id TEXT PRIMARY KEY,
    source TEXT,
    package TEXT,
    revision TEXT,
    source_url TEXT,
    source_url_hash TEXT,
    status TEXT,
    execution_arn TEXT,
    error TEXT,
    snapshot_id BIGINT,
    row_counts JSON,
    created_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ,
    UNIQUE(source, package, revision, source_url_hash)
);
