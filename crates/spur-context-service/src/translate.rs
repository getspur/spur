//! Build-pipeline translation from spur-graph artifacts into `DuckLake` tables.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use duckdb::{params, Connection};
use serde::Deserialize;

use crate::catalog::{
    catalog_dsn_with_env_password, ducklake_data_path, export_frozen_snapshot, gold_table,
    load_duckdb_extension, postgres_metadata_dsn,
};
use crate::medallion::SilverManifest;

const DEFAULT_EMBEDDING_MODEL: &str = "EmbeddingGemma300M";
pub const DEFAULT_EMBED_TEXT_VERSION: &str = "v4-embeddinggemma-300m-titled";
pub const DEFAULT_TRANSLATE_SCHEMA_VERSION: &str = "translate-v1";
pub(crate) const CATALOG_TABLES_SQL: &str = include_str!("../sql/catalog_tables.sql");

#[derive(Debug)]
struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    provider: &'static str,
}

/// Fetches AWS credentials from env vars or the ECS credentials endpoint.
/// On Fargate, the ECS endpoint at http://169.254.170.2 provides temporary
/// credentials via the task IAM role. DuckDB's credential_chain provider
/// may not support this endpoint, so we resolve credentials explicitly.
fn fetch_aws_credentials() -> Option<AwsCredentials> {
    if let (Some(key), Some(secret)) = (
        std::env::var("AWS_ACCESS_KEY_ID")
            .ok()
            .filter(|s| !s.is_empty()),
        std::env::var("AWS_SECRET_ACCESS_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
    ) {
        return Some(AwsCredentials {
            access_key_id: key,
            secret_access_key: secret,
            session_token: std::env::var("AWS_SESSION_TOKEN")
                .ok()
                .filter(|s| !s.is_empty()),
            provider: "env",
        });
    }

    let relative_uri = std::env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").ok()?;
    let url = format!("http://169.254.170.2{relative_uri}");
    let body = http_get(&url, 64 * 1024).ok()?;
    let creds: EcsCredentials = serde_json::from_slice(&body).ok()?;
    Some(AwsCredentials {
        access_key_id: creds.access_key_id,
        secret_access_key: creds.secret_access_key,
        session_token: creds.token,
        provider: "ecs",
    })
}

#[derive(Debug, Deserialize)]
struct EcsCredentials {
    #[serde(rename = "AccessKeyId")]
    access_key_id: String,
    #[serde(rename = "SecretAccessKey")]
    secret_access_key: String,
    #[serde(rename = "Token")]
    token: Option<String>,
}

fn http_get(url: &str, body_cap: usize) -> std::result::Result<Vec<u8>, String> {
    let rest = url.strip_prefix("http://").ok_or("not http")?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_owned()),
    };
    let mut stream = TcpStream::connect((authority, 80)).map_err(|e| format!("connect: {e}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut response = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..n]);
        if response.len() > body_cap + 64 * 1024 {
            return Err("response too large".into());
        }
    }
    let header_end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .ok_or("no header end")?;
    Ok(response[header_end..].to_vec())
}

const REVISION_TABLES: &[&str] = &[
    "nodes",
    "edges",
    "edges_unresolved",
    "files",
    "file_manifests",
    "section_bodies",
    "symbol_embeddings",
    "commits",
    "symbol_snapshots",
    "temporal_edges",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateOptions {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub revision_kind: String,
    pub artifact_dir: PathBuf,
    pub artifact_manifest: Option<SilverManifest>,
    pub source_root: Option<PathBuf>,
    pub catalog_dsn: String,
    pub lineage: Option<TranslateLineage>,
    pub allow_missing_embeddings: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateStats {
    pub rows_inserted: HashMap<String, usize>,
    pub snapshot_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateLineage {
    pub bronze_content_sha256: String,
    pub silver_graph_content_hash: String,
    pub builder_version: String,
    pub translate_schema_version: String,
    pub embed_text_version: String,
}

#[derive(Debug, Clone, Copy)]
struct RevisionParts {
    major: Option<i32>,
    minor: Option<i32>,
    patch: Option<i32>,
}

#[derive(Debug, Clone)]
enum SidecarSource {
    Parquet(String),
    Lance(String),
}

impl SidecarSource {
    fn sql(&self) -> &str {
        match self {
            Self::Parquet(sql) | Self::Lance(sql) => sql,
        }
    }

    fn is_lance(&self) -> bool {
        matches!(self, Self::Lance(_))
    }
}

fn translate_phase_timing_line(phase: &str, elapsed: Duration) -> String {
    format!(
        "[translate] phase {phase} elapsed_ms={}",
        elapsed.as_millis()
    )
}

fn emit_translate_phase_timing(phase: &str, elapsed: Duration) {
    let line = translate_phase_timing_line(phase, elapsed);
    eprintln!("{line}");
}

fn log_translate_phase_timing(phase: &str, started: Instant) {
    emit_translate_phase_timing(phase, started.elapsed());
}

pub fn translate_artifact_to_ducklake(opts: &TranslateOptions) -> Result<TranslateStats> {
    validate_options(opts)?;
    let revision = revision_parts(&opts.revision, &opts.revision_kind)?;
    let catalog_dsn = catalog_dsn_with_env_password(&opts.catalog_dsn);
    let data_path = ducklake_data_path(&catalog_dsn)?;

    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    let phase_started = Instant::now();
    load_ducklake_extensions(&conn, &catalog_dsn, &data_path)?;
    attach_ducklake(&conn, &catalog_dsn, &data_path)?;
    log_translate_phase_timing("load_ducklake_extensions+attach_ducklake", phase_started);

    let phase_started = Instant::now();
    ensure_catalog_schema(&conn)?;
    log_translate_phase_timing("ensure_catalog_schema", phase_started);

    let phase_started = Instant::now();
    let prepared_lance = prepare_lance_sidecar_extensions(&conn, opts)?;
    log_translate_phase_timing("prepare_lance_sidecar_extensions", phase_started);

    let phase_started = Instant::now();
    let postgres_metadata = attach_postgres_metadata_catalog(&conn, &catalog_dsn)?;
    log_translate_phase_timing("attach_postgres_metadata_catalog", phase_started);

    let phase_started = Instant::now();
    acquire_postgres_gold_publish_lock(&conn, postgres_metadata.as_ref())?;
    let generation = next_generation(&conn, &catalog_dsn, postgres_metadata.as_ref())?;
    emit_translate_phase_timing(
        "acquire_postgres_gold_publish_lock+next_generation",
        phase_started.elapsed(),
    );

    // Keep generation-scoped writes and the metadata publish/checkpoint/export
    // under the Postgres session advisory lock. Rows are generation-stamped, but
    // existing tests do not prove concurrent DuckLake metadata writers can
    // safely publish, checkpoint, and export frozen snapshots in parallel.
    let mut rows_inserted = HashMap::new();
    let phase_started = Instant::now();
    run_transaction(&conn, || {
        delete_generation_rows(&conn, opts, generation)?;
        insert_structural_tables(&conn, opts, revision, generation, &mut rows_inserted)?;
        insert_git_tables(&conn, opts, revision, generation, &mut rows_inserted)
    })
    .context("failed to translate artifact tables into DuckLake")?;
    log_translate_phase_timing(
        "delete_generation_rows+insert_structural_tables+insert_git_tables",
        phase_started,
    );

    let phase_started = Instant::now();
    let embeddings_translated = insert_sidecar_tables(
        &conn,
        opts,
        revision,
        generation,
        prepared_lance,
        &mut rows_inserted,
    )
    .context("failed to translate artifact sidecars into DuckLake")?;
    log_translate_phase_timing("insert_sidecar_tables", phase_started);

    let snapshot_id = latest_snapshot_id(&conn).context("failed to read DuckLake snapshot id")?;
    let phase_started = Instant::now();
    validate_generation(&conn, opts, generation, &rows_inserted)
        .context("failed to validate translated gold generation")?;
    log_translate_phase_timing("validate_generation", phase_started);

    let phase_started = Instant::now();
    write_catalog_metadata(
        &conn,
        opts,
        revision,
        generation,
        snapshot_id,
        embeddings_translated,
        &rows_inserted,
    )
    .context("failed to update package catalog metadata")?;
    log_translate_phase_timing("write_catalog_metadata", phase_started);

    // Force DuckLake to flush all inlined data to parquet files on the data path.
    // DuckLake 1.0 inlines small inserts (<10 rows) into the catalog metadata.
    // Flushing moves them to parquet files so they're accessible to read-only
    // consumers (Lambda) that open the catalog without the same inlining state.
    //
    // CRITICAL: ducklake_flush_inlined_data is a set-returning table function.
    // Its parquet-writing work happens lazily as the result stream is consumed.
    // execute_batch() uses duckdb_query_arrow internally which creates the stream
    // but never drains it — so the writes never happen. We must use prepare().query()
    // and fully drain the result set to force materialization.
    let phase_started = Instant::now();
    {
        let mut stmt = conn
            .prepare("CALL ducklake_flush_inlined_data('spur_context')")
            .context("failed to prepare ducklake_flush_inlined_data")?;
        let mut rows = stmt.query([]).context("failed to execute flush")?;
        while rows.next()?.is_some() {}
    }

    // FORCE CHECKPOINT makes the just-committed DuckLake changes durable before
    // the worker uploads the catalog metadata back to S3.
    conn.execute("FORCE CHECKPOINT", [])
        .context("failed to force checkpoint DuckLake")?;
    export_frozen_snapshot(&catalog_dsn, &data_path, generation)
        .context("failed to export frozen DuckLake catalog snapshot")?;
    log_translate_phase_timing(
        "ducklake_flush_inlined_data+FORCE CHECKPOINT+export_frozen_snapshot",
        phase_started,
    );

    Ok(TranslateStats {
        rows_inserted,
        snapshot_id,
    })
}

pub fn update_refs(db: &Connection, source: &str, package: &str, revision: &str) -> Result<()> {
    let revision_kind = optional_string(
        db.query_row(
            r"
            SELECT revision_kind
            FROM gold.package_catalog
            WHERE source = ? AND package = ? AND revision = ?
            ORDER BY indexed_at DESC NULLS LAST
            LIMIT 1
            ",
            params![source, package, revision],
            |row| row.get(0),
        ),
        "failed to read revision kind for refs update",
    )?
    .ok_or_else(|| {
        anyhow!("cannot update refs for unknown revision: {source}/{package}@{revision}")
    })?;

    match revision_kind.as_str() {
        "semver" => update_latest_semver_ref(db, source, package, revision),
        "git_sha" => update_existing_git_refs(db, source, package, revision),
        other => bail!("unsupported revision_kind in package_catalog: {other}"),
    }
}

fn validate_options(opts: &TranslateOptions) -> Result<()> {
    if opts.source.trim().is_empty() {
        bail!("translate source must be non-empty");
    }
    if opts.package.trim().is_empty() {
        bail!("translate package must be non-empty");
    }
    if opts.revision.trim().is_empty() {
        bail!("translate revision must be non-empty");
    }
    if !matches!(opts.revision_kind.as_str(), "semver" | "git_sha") {
        bail!("unsupported revision_kind `{}`", opts.revision_kind);
    }
    if !opts.artifact_dir.is_dir() {
        bail!(
            "artifact_dir does not exist or is not a directory: {}",
            opts.artifact_dir.display()
        );
    }
    if let Some(source_root) = &opts.source_root {
        if !source_root.is_dir() {
            bail!(
                "source_root does not exist or is not a directory: {}",
                source_root.display()
            );
        }
    }
    if opts.catalog_dsn.trim().is_empty() {
        bail!("catalog_dsn must be non-empty");
    }
    Ok(())
}

fn revision_parts(revision: &str, revision_kind: &str) -> Result<RevisionParts> {
    if revision_kind == "git_sha" {
        return Ok(RevisionParts {
            major: None,
            minor: None,
            patch: None,
        });
    }

    let (major, minor, patch) = parse_semver_triplet(revision)
        .ok_or_else(|| anyhow!("failed to parse semver revision `{revision}`"))?;
    Ok(RevisionParts {
        major: Some(major),
        minor: Some(minor),
        patch: Some(patch),
    })
}

fn insert_structural_tables(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    generation: i64,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<()> {
    let nodes = required_artifact_file(opts, "nodes.parquet")?;
    let edges = required_artifact_file(opts, "edges.parquet")?;
    let unresolved = required_artifact_file(opts, "edges_unresolved.parquet")?;
    let files = required_artifact_file(opts, "files.parquet")?;
    let file_manifests = required_artifact_file(opts, "file_manifests.parquet")?;
    stage_source_files(conn, opts.source_root.as_deref(), &files)?;

    insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "nodes",
        &read_parquet_source(&nodes),
        &format!(
            r"
            INSERT INTO gold.nodes (
                stable_symbol_id, package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                file_path, byte_range_start, byte_range_end,
                line_start, line_end, entity_name, qualified_name,
                symbol_kind, anchor_hash, enclosing_scope, generation
            )
            SELECT
                stable_symbol_id,
                {package} AS package,
                {source} AS source,
                {rev} AS revision,
                {rev_kind} AS revision_kind,
                {major} AS semver_major,
                {minor} AS semver_minor,
                {patch} AS semver_patch,
                file_path, byte_range_start, byte_range_end,
                line_start, line_end, entity_name, qualified_name,
                symbol_kind, anchor_hash, enclosing_scope,
                {generation} AS generation
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
            generation = generation,
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "edges",
        &read_parquet_source(&edges),
        &format!(
            r"
            INSERT INTO gold.edges (
                source_stable_id, target_stable_id, target_package, target_label,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                relation, edge_kind, confidence, confidence_score,
                bind_method, receiver_text, scope_text, generation
            )
            SELECT
                source_stable_id,
                target_stable_id,
                CAST(NULL AS VARCHAR) AS target_package,
                target_label,
                {package} AS package,
                {source} AS source,
                {rev} AS revision,
                {rev_kind} AS revision_kind,
                {major} AS semver_major,
                {minor} AS semver_minor,
                {patch} AS semver_patch,
                relation, edge_kind, confidence,
                confidence_score::DOUBLE AS confidence_score,
                bind_method, receiver_text, scope_text,
                {generation} AS generation
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
            generation = generation,
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "edges_unresolved",
        &read_parquet_source(&unresolved),
        &format!(
            r"
            INSERT INTO gold.edges_unresolved (
                source_stable_id, target_label, target_package,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                relation, edge_kind, confidence, confidence_score,
                bind_method, receiver_text, scope_text, generation
            )
            SELECT
                source_stable_id,
                target_label,
                import_path AS target_package,
                {package} AS package,
                {source} AS source,
                {rev} AS revision,
                {rev_kind} AS revision_kind,
                {major} AS semver_major,
                {minor} AS semver_minor,
                {patch} AS semver_patch,
                relation, edge_kind, confidence,
                confidence_score::DOUBLE AS confidence_score,
                bind_method, receiver_text, scope_text,
                {generation} AS generation
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
            generation = generation,
        ),
        rows_inserted,
    )?;

    let (source_text_expr, source_text_join) = if opts.source_root.is_some() {
        (
            "sf.source_text",
            "LEFT JOIN __spur_context_source_files sf ON sf.file_path = artifact_files.file_path",
        )
    } else {
        ("CAST(NULL AS VARCHAR)", "")
    };

    insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "files",
        &read_parquet_source(&files),
        &format!(
            r"
            INSERT INTO gold.files (
                stable_file_id, file_path, source_text,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch, generation
            )
            SELECT
                artifact_files.stable_file_id,
                artifact_files.file_path,
                {source_text_expr} AS source_text,
                {package} AS package,
                {source} AS source,
                {rev} AS revision,
                {rev_kind} AS revision_kind,
                {major} AS semver_major,
                {minor} AS semver_minor,
                {patch} AS semver_patch,
                {generation} AS generation
            FROM __SOURCE_SQL__ AS artifact_files
            {source_text_join}
            ",
            source_text_expr = source_text_expr,
            source_text_join = source_text_join,
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
            generation = generation,
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "file_manifests",
        &read_parquet_source(&file_manifests),
        &format!(
            r"
            INSERT INTO gold.file_manifests (
                stable_file_id, path, content_oid, node_ids,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch, generation
            )
            SELECT
                stable_file_id,
                path,
                content_oid,
                list_transform(node_ids, node_id -> CAST(node_id AS VARCHAR)) AS node_ids,
                {package} AS package,
                {source} AS source,
                {rev} AS revision,
                {rev_kind} AS revision_kind,
                {major} AS semver_major,
                {minor} AS semver_minor,
                {patch} AS semver_patch,
                {generation} AS generation
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
            generation = generation,
        ),
        rows_inserted,
    )?;

    Ok(())
}

fn stage_source_files(conn: &Connection, source_root: Option<&Path>, files: &Path) -> Result<()> {
    conn.execute_batch(
        r"
        DROP TABLE IF EXISTS __spur_context_source_files;
        CREATE TEMP TABLE __spur_context_source_files (
            file_path VARCHAR,
            source_text VARCHAR
        );
        ",
    )
    .context("failed to initialize source file staging table")?;

    let Some(source_root) = source_root else {
        return Ok(());
    };

    let source_sql = read_parquet_source(files);
    let query = format!("SELECT DISTINCT file_path FROM {source_sql} ORDER BY file_path");
    let mut stmt = conn
        .prepare(&query)
        .context("failed to prepare artifact file path query")?;
    let paths = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read artifact file paths")?;
    let mut insert = conn
        .prepare(
            r"
            INSERT INTO __spur_context_source_files (file_path, source_text)
            VALUES (?, ?)
            ",
        )
        .context("failed to prepare source file staging insert")?;

    for file_path in paths {
        let source_path = source_file_path(source_root, &file_path)?;
        let source_text = fs::read_to_string(&source_path).with_context(|| {
            format!(
                "failed to read source file `{}` for artifact path `{file_path}`",
                source_path.display()
            )
        })?;
        insert
            .execute(params![file_path, source_text])
            .with_context(|| format!("failed to stage source text for `{file_path}`"))?;
    }

    Ok(())
}

fn source_file_path(source_root: &Path, file_path: &str) -> Result<PathBuf> {
    let relative = Path::new(file_path);
    if relative.is_absolute() {
        bail!("artifact file path must be relative: {file_path}");
    }

    let mut resolved = source_root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("artifact file path escapes source_root: {file_path}");
            }
        }
    }

    Ok(resolved)
}

fn insert_git_tables(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    generation: i64,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<()> {
    if opts.revision_kind != "git_sha" {
        rows_inserted.insert("commits".to_owned(), 0);
        rows_inserted.insert("symbol_snapshots".to_owned(), 0);
        rows_inserted.insert("temporal_edges".to_owned(), 0);
        return Ok(());
    }

    if let Some(commits) = optional_artifact_source(opts, "commits.parquet", None)? {
        insert_from_source(
            conn,
            GoldInsertScope::new(opts, generation),
            "commits",
            &commits,
            &format!(
                r"
                INSERT INTO gold.commits (
                    sha, parents, author_time, author_name, author_email, summary,
                    package, source, revision, revision_kind,
                    semver_major, semver_minor, semver_patch, generation
                )
                SELECT
                    sha, parents, author_time, author_name, author_email, summary,
                    {package} AS package,
                    {source} AS source,
                    {rev} AS revision,
                    {rev_kind} AS revision_kind,
                    {major} AS semver_major,
                    {minor} AS semver_minor,
                    {patch} AS semver_patch,
                    {generation} AS generation
                FROM __SOURCE_SQL__
                ",
                package = sql_string(&opts.package),
                source = sql_string(&opts.source),
                rev = sql_string(&opts.revision),
                rev_kind = sql_string(&opts.revision_kind),
                major = sql_i32(revision.major),
                minor = sql_i32(revision.minor),
                patch = sql_i32(revision.patch),
                generation = generation,
            ),
            rows_inserted,
        )?;
    } else {
        rows_inserted.insert("commits".to_owned(), 0);
    }

    if let Some(snapshots) = optional_artifact_source(
        opts,
        "symbol_snapshots.parquet",
        Some("symbol_snapshots/**/*.parquet"),
    )? {
        insert_from_source(
            conn,
            GoldInsertScope::new(opts, generation),
            "symbol_snapshots",
            &snapshots,
            &format!(
                r"
                INSERT INTO gold.symbol_snapshots (
                    stable_symbol_id, commit, package, source, revision, revision_kind,
                    semver_major, semver_minor, semver_patch,
                    file_path, entity_name, symbol_kind, enclosing_scope,
                    byte_range, line_range, anchor_hash, generation
                )
                SELECT
                    key_stable_symbol_id AS stable_symbol_id,
                    key_commit AS commit,
                    {package} AS package,
                    {source} AS source,
                    {rev} AS revision,
                    {rev_kind} AS revision_kind,
                    {major} AS semver_major,
                    {minor} AS semver_minor,
                    {patch} AS semver_patch,
                    file_path_b64 AS file_path,
                    entity_name,
                    symbol_kind,
                    enclosing_scope,
                    [byte_range_start::INTEGER, byte_range_end::INTEGER] AS byte_range,
                    [line_range_start::INTEGER, line_range_end::INTEGER] AS line_range,
                    anchor_hash,
                    {generation} AS generation
                FROM __SOURCE_SQL__
                ",
                package = sql_string(&opts.package),
                source = sql_string(&opts.source),
                rev = sql_string(&opts.revision),
                rev_kind = sql_string(&opts.revision_kind),
                major = sql_i32(revision.major),
                minor = sql_i32(revision.minor),
                patch = sql_i32(revision.patch),
                generation = generation,
            ),
            rows_inserted,
        )?;
    } else {
        rows_inserted.insert("symbol_snapshots".to_owned(), 0);
    }

    if let Some(temporal_edges) = optional_artifact_source(
        opts,
        "temporal_edges.parquet",
        Some("temporal_edges/**/*.parquet"),
    )? {
        insert_from_source(
            conn,
            GoldInsertScope::new(opts, generation),
            "temporal_edges",
            &temporal_edges,
            &format!(
                r"
                INSERT INTO gold.temporal_edges (
                    source_endpoint, target_endpoint, relation, change_kind, parent,
                    package, source, revision, revision_kind,
                    semver_major, semver_minor, semver_patch, generation
                )
                SELECT
                    json_object(
                        'kind', source_kind,
                        'path_b64', source_path_b64,
                        'stable_symbol_id', source_stable_symbol_id,
                        'commit', source_commit
                    )::VARCHAR AS source_endpoint,
                    json_object(
                        'kind', target_kind,
                        'path_b64', target_path_b64,
                        'stable_symbol_id', target_stable_symbol_id,
                        'commit', target_commit
                    )::VARCHAR AS target_endpoint,
                    relation,
                    change_kind,
                    parent,
                    {package} AS package,
                    {source} AS source,
                    {rev} AS revision,
                    {rev_kind} AS revision_kind,
                    {major} AS semver_major,
                    {minor} AS semver_minor,
                    {patch} AS semver_patch,
                    {generation} AS generation
                FROM __SOURCE_SQL__
                ",
                package = sql_string(&opts.package),
                source = sql_string(&opts.source),
                rev = sql_string(&opts.revision),
                rev_kind = sql_string(&opts.revision_kind),
                major = sql_i32(revision.major),
                minor = sql_i32(revision.minor),
                patch = sql_i32(revision.patch),
                generation = generation,
            ),
            rows_inserted,
        )?;
    } else {
        rows_inserted.insert("temporal_edges".to_owned(), 0);
    }

    Ok(())
}

fn insert_sidecar_tables(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    generation: i64,
    prepared_lance: PreparedLanceSidecars,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<bool> {
    let symbols_translated = insert_symbol_embeddings(
        conn,
        opts,
        revision,
        generation,
        prepared_lance,
        rows_inserted,
    )
    .with_context(|| {
        format!(
            "failed to translate `{}`",
            opts.artifact_dir.join("code_symbols.lance").display()
        )
    })?;
    insert_section_bodies(
        conn,
        opts,
        revision,
        generation,
        prepared_lance,
        rows_inserted,
    )
    .with_context(|| {
        format!(
            "failed to translate `{}`",
            opts.artifact_dir.join("sections.lancedb").display()
        )
    })?;
    Ok(symbols_translated)
}

fn insert_symbol_embeddings(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    generation: i64,
    prepared_lance: PreparedLanceSidecars,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<bool> {
    let sidecar_path = opts.artifact_dir.join("code_symbols.lance");
    let Some(source) = sidecar_source(
        conn,
        opts,
        "code_symbols.lance",
        SidecarKind::CodeSymbols,
        prepared_lance,
    )?
    else {
        if !opts.allow_missing_embeddings {
            bail!(
                "expected symbol embeddings from `{}` but the sidecar was unavailable",
                sidecar_path.display()
            );
        }
        rows_inserted.insert("symbol_embeddings".to_owned(), 0);
        return Ok(false);
    };

    let columns = match source_columns(conn, source.sql()) {
        Ok(columns) => columns,
        Err(error) if source.is_lance() => {
            skip_lance_sidecar_or_fail(
                opts,
                &sidecar_path,
                error,
                "expected symbol embeddings but failed to read Lance sidecar",
            )?;
            rows_inserted.insert("symbol_embeddings".to_owned(), 0);
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let embedding_expr = if columns.contains("vector") {
        "vector"
    } else if columns.contains("embedding") {
        "embedding"
    } else if opts.allow_missing_embeddings {
        // Embedding-free artifact (e.g. the no-embed worker, where `spur graph
        // build` writes symbol rows without a vector column). Skip the symbol
        // embeddings table gracefully rather than failing translation.
        warn_skip_sidecar(
            &sidecar_path,
            &anyhow::anyhow!("code symbol sidecar is missing vector/embedding column"),
        );
        rows_inserted.insert("symbol_embeddings".to_owned(), 0);
        return Ok(false);
    } else {
        bail!("code symbol sidecar is missing vector/embedding column");
    };
    let model_expr = if columns.contains("embedding_model") {
        format!(
            "COALESCE(embedding_model, {})",
            sql_string(DEFAULT_EMBEDDING_MODEL)
        )
    } else {
        sql_string(DEFAULT_EMBEDDING_MODEL)
    };
    let input_hash_expr = if columns.contains("embedding_input_hash") {
        "embedding_input_hash".to_owned()
    } else if columns.contains("content_hash") {
        "content_hash".to_owned()
    } else {
        "CAST(NULL AS VARCHAR)".to_owned()
    };

    let filtered_source_sql = format!(
        "(SELECT * FROM {} WHERE {embedding_expr} IS NOT NULL)",
        source.sql()
    );

    let template = format!(
        r"
        INSERT INTO gold.symbol_embeddings (
            stable_symbol_id, package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            file_path, entity_name, qualified_name, symbol_kind,
            embedding, embedding_model, embedding_input_hash, embed_text_version, generation
        )
        SELECT
            stable_symbol_id,
            {package} AS package,
            {source} AS source,
            {rev} AS revision,
            {rev_kind} AS revision_kind,
            {major} AS semver_major,
            {minor} AS semver_minor,
            {patch} AS semver_patch,
            file_path,
            entity_name,
            qualified_name,
            symbol_kind,
            {embedding_expr}::FLOAT[] AS embedding,
            {model_expr} AS embedding_model,
            {input_hash_expr} AS embedding_input_hash,
            {embed_text_version} AS embed_text_version,
            {generation} AS generation
        FROM __SOURCE_SQL__
        ",
        package = sql_string(&opts.package),
        source = sql_string(&opts.source),
        rev = sql_string(&opts.revision),
        rev_kind = sql_string(&opts.revision_kind),
        major = sql_i32(revision.major),
        minor = sql_i32(revision.minor),
        patch = sql_i32(revision.patch),
        embedding_expr = embedding_expr,
        model_expr = model_expr,
        input_hash_expr = input_hash_expr,
        embed_text_version = sql_string(DEFAULT_EMBED_TEXT_VERSION),
        generation = generation,
    );

    match insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "symbol_embeddings",
        &filtered_source_sql,
        &template,
        rows_inserted,
    ) {
        Ok(()) => {
            let inserted = rows_inserted
                .get("symbol_embeddings")
                .copied()
                .unwrap_or_default();
            if inserted == 0 && !opts.allow_missing_embeddings {
                bail!(
                    "expected symbol embeddings from `{}` but zero embedding rows landed",
                    sidecar_path.display()
                );
            }
            Ok(inserted > 0)
        }
        Err(error) if source.is_lance() => {
            skip_lance_sidecar_or_fail(
                opts,
                &sidecar_path,
                error,
                "expected symbol embeddings but failed to insert Lance sidecar",
            )?;
            rows_inserted.insert("symbol_embeddings".to_owned(), 0);
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn insert_section_bodies(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    generation: i64,
    prepared_lance: PreparedLanceSidecars,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<bool> {
    let sidecar_path = opts.artifact_dir.join("sections.lancedb");
    let Some(source) = sidecar_source(
        conn,
        opts,
        "sections.lancedb",
        SidecarKind::Sections,
        prepared_lance,
    )?
    else {
        if !opts.allow_missing_embeddings {
            bail!(
                "expected section bodies from `{}` but the sidecar was unavailable",
                sidecar_path.display()
            );
        }
        rows_inserted.insert("section_bodies".to_owned(), 0);
        return Ok(false);
    };

    let columns = match source_columns(conn, source.sql()) {
        Ok(columns) => columns,
        Err(error) if source.is_lance() => {
            skip_lance_sidecar_or_fail(
                opts,
                &sidecar_path,
                error,
                "expected section bodies but failed to read Lance sidecar",
            )?;
            rows_inserted.insert("section_bodies".to_owned(), 0);
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let section_id_expr = if columns.contains("section_id") {
        "section_id"
    } else if columns.contains("stable_symbol_id") {
        "stable_symbol_id"
    } else {
        bail!("section sidecar is missing section_id/stable_symbol_id column");
    };
    let title_expr = if columns.contains("title") {
        "title"
    } else if columns.contains("qualified_name") {
        "qualified_name"
    } else {
        "file_path"
    };
    let body_hash_expr = if columns.contains("body_hash") {
        "body_hash"
    } else if columns.contains("content_hash") {
        "content_hash"
    } else {
        "CAST(NULL AS VARCHAR)"
    };
    let token_count_expr = if columns.contains("token_count") {
        "token_count::INTEGER"
    } else {
        "array_length(regexp_extract_all(COALESCE(body_text, ''), '[[:alnum:]]+'))::INTEGER"
    };
    let vector_expr = if columns.contains("vector") {
        "vector::FLOAT[]"
    } else {
        "CAST(NULL AS FLOAT[])"
    };
    let model_expr = if columns.contains("embedding_model") {
        format!(
            "COALESCE(embedding_model, {})",
            sql_string(DEFAULT_EMBEDDING_MODEL)
        )
    } else {
        sql_string(DEFAULT_EMBEDDING_MODEL)
    };
    let input_hash_expr = if columns.contains("embedding_input_hash") {
        "embedding_input_hash".to_owned()
    } else if columns.contains("content_hash") {
        "content_hash".to_owned()
    } else if columns.contains("body_hash") {
        "body_hash".to_owned()
    } else {
        "CAST(NULL AS VARCHAR)".to_owned()
    };

    let template = format!(
        r"
        INSERT INTO gold.section_bodies (
            section_id, package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            file_path, title, body_text, body_hash, token_count,
            vector, embedding_model, embedding_input_hash, embed_text_version, generation
        )
        SELECT
            {section_id_expr} AS section_id,
            {package} AS package,
            {source} AS source,
            {rev} AS revision,
            {rev_kind} AS revision_kind,
            {major} AS semver_major,
            {minor} AS semver_minor,
            {patch} AS semver_patch,
            file_path,
            {title_expr} AS title,
            body_text,
            {body_hash_expr} AS body_hash,
            {token_count_expr} AS token_count,
            {vector_expr} AS vector,
            {model_expr} AS embedding_model,
            {input_hash_expr} AS embedding_input_hash,
            {embed_text_version} AS embed_text_version,
            {generation} AS generation
        FROM __SOURCE_SQL__
        ",
        section_id_expr = section_id_expr,
        package = sql_string(&opts.package),
        source = sql_string(&opts.source),
        rev = sql_string(&opts.revision),
        rev_kind = sql_string(&opts.revision_kind),
        major = sql_i32(revision.major),
        minor = sql_i32(revision.minor),
        patch = sql_i32(revision.patch),
        title_expr = title_expr,
        body_hash_expr = body_hash_expr,
        token_count_expr = token_count_expr,
        vector_expr = vector_expr,
        model_expr = model_expr,
        input_hash_expr = input_hash_expr,
        embed_text_version = sql_string(DEFAULT_EMBED_TEXT_VERSION),
        generation = generation,
    );

    match insert_from_source(
        conn,
        GoldInsertScope::new(opts, generation),
        "section_bodies",
        source.sql(),
        &template,
        rows_inserted,
    ) {
        Ok(()) => {
            let inserted = rows_inserted
                .get("section_bodies")
                .copied()
                .unwrap_or_default();
            if inserted == 0 && !opts.allow_missing_embeddings {
                bail!(
                    "expected section bodies from `{}` but zero rows landed",
                    sidecar_path.display()
                );
            }
            Ok(inserted > 0)
        }
        Err(error) if source.is_lance() => {
            skip_lance_sidecar_or_fail(
                opts,
                &sidecar_path,
                error,
                "expected section bodies but failed to insert Lance sidecar",
            )?;
            rows_inserted.insert("section_bodies".to_owned(), 0);
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn write_catalog_metadata(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    generation: i64,
    snapshot_id: i64,
    embeddings_translated: bool,
    rows_inserted: &HashMap<String, usize>,
) -> Result<()> {
    let row_counts = serde_json::to_string(rows_inserted).context("failed to encode row counts")?;
    let embeddings_status = if embeddings_translated {
        "complete"
    } else {
        "skipped"
    };
    let lineage = opts.lineage.clone().unwrap_or_else(default_lineage);

    run_transaction(conn, || {
        conn.execute(
            "DELETE FROM gold.package_catalog WHERE source = ? AND package = ? AND revision = ?",
            params![opts.source, opts.package, opts.revision],
        )
        .context("failed to delete existing package_catalog row")?;
        conn.execute(
            r"
            INSERT INTO gold.package_catalog (
                source, package, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                snapshot_id, indexed_at, index_status, embeddings_status, row_counts,
                generation, bronze_content_sha256, silver_graph_content_hash,
                builder_version, translate_schema_version, embed_text_version
            )
            VALUES (
                ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, 'complete', ?, CAST(? AS JSON),
                ?, ?, ?, ?, ?, ?
            )
            ",
            params![
                opts.source,
                opts.package,
                opts.revision,
                opts.revision_kind,
                revision.major,
                revision.minor,
                revision.patch,
                snapshot_id,
                embeddings_status,
                row_counts,
                generation,
                lineage.bronze_content_sha256,
                lineage.silver_graph_content_hash,
                lineage.builder_version,
                lineage.translate_schema_version,
                lineage.embed_text_version,
            ],
        )
        .context("failed to insert package_catalog row")?;
        update_refs(conn, &opts.source, &opts.package, &opts.revision)?;
        Ok(())
    })
}

fn default_lineage() -> TranslateLineage {
    TranslateLineage {
        bronze_content_sha256: "unknown".to_owned(),
        silver_graph_content_hash: "unknown".to_owned(),
        builder_version: "unknown".to_owned(),
        translate_schema_version: DEFAULT_TRANSLATE_SCHEMA_VERSION.to_owned(),
        embed_text_version: DEFAULT_EMBED_TEXT_VERSION.to_owned(),
    }
}

fn next_generation(
    conn: &Connection,
    catalog_dsn: &str,
    postgres_metadata: Option<&PostgresMetadataCatalog>,
) -> Result<i64> {
    if is_postgres_catalog(catalog_dsn) {
        let postgres_metadata = postgres_metadata
            .context("Postgres metadata catalog must be attached before generation reservation")?;
        return next_postgres_generation(conn, postgres_metadata);
    }
    reserve_transactional_generation(conn)
}

#[derive(Debug, Clone)]
struct PostgresMetadataCatalog {
    alias: String,
}

fn attach_postgres_metadata_catalog(
    conn: &Connection,
    catalog_dsn: &str,
) -> Result<Option<PostgresMetadataCatalog>> {
    if !is_postgres_catalog(catalog_dsn) {
        return Ok(None);
    }

    let alias = format!("spur_metadata_{}", uuid::Uuid::new_v4().simple());
    let dsn = postgres_metadata_dsn(catalog_dsn_with_env_password(catalog_dsn).as_str());
    conn.execute_batch(&format!(
        "ATTACH '{}' AS {alias} (TYPE postgres);",
        escape_sql_literal(&dsn)
    ))
    .context("failed to attach Postgres metadata catalog")?;

    Ok(Some(PostgresMetadataCatalog { alias }))
}

fn next_postgres_generation(
    conn: &Connection,
    postgres_metadata: &PostgresMetadataCatalog,
) -> Result<i64> {
    let sql = postgres_generation_allocator_sql(&postgres_metadata.alias);
    conn.execute_batch(&sql.create_sequence)
        .context("failed to ensure Postgres gold generation sequence")?;
    conn.query_row(&sql.reserve_generation, [], |row| row.get(0))
        .context("failed to reserve Postgres gold generation")
}

struct PostgresGenerationAllocatorSql {
    create_sequence: String,
    reserve_generation: String,
}

fn postgres_generation_allocator_sql(alias: &str) -> PostgresGenerationAllocatorSql {
    PostgresGenerationAllocatorSql {
        create_sequence: format!(
            "CALL postgres_execute('{alias}', 'CREATE SEQUENCE IF NOT EXISTS public.spur_context_gold_generation_seq AS BIGINT START WITH 1')"
        ),
        reserve_generation: format!(
            "SELECT generation::BIGINT FROM postgres_query('{alias}', 'SELECT nextval(''public.spur_context_gold_generation_seq'') AS generation')"
        ),
    }
}

fn acquire_postgres_gold_publish_lock(
    conn: &Connection,
    postgres_metadata: Option<&PostgresMetadataCatalog>,
) -> Result<()> {
    let Some(postgres_metadata) = postgres_metadata else {
        return Ok(());
    };

    let sql = postgres_gold_publish_lock_sql(&postgres_metadata.alias);
    conn.query_row(&sql.acquire_lock, [], |_| Ok(()))
        .context("failed to acquire Postgres gold publish lock")
}

struct PostgresGoldPublishLockSql {
    acquire_lock: String,
}

fn postgres_gold_publish_lock_sql(alias: &str) -> PostgresGoldPublishLockSql {
    PostgresGoldPublishLockSql {
        acquire_lock: format!(
            "SELECT locked FROM postgres_query('{alias}', 'SELECT TRUE AS locked FROM pg_advisory_lock(7830668896113191951)')"
        ),
    }
}

fn reserve_transactional_generation(conn: &Connection) -> Result<i64> {
    run_transaction(conn, || {
        conn.execute(
            r"
            INSERT INTO gold.generation_allocator (allocator_id, next_generation)
            SELECT 1, 1
            WHERE NOT EXISTS (
                SELECT 1 FROM gold.generation_allocator WHERE allocator_id = 1
            )
            ",
            [],
        )
        .context("failed to initialize gold generation allocator")?;
        let generation: i64 = conn
            .query_row(
                r"
                SELECT next_generation::BIGINT
                FROM gold.generation_allocator
                WHERE allocator_id = 1
                ",
                [],
                |row| row.get(0),
            )
            .context("failed to read reserved gold generation")?;
        conn.execute(
            r"
            UPDATE gold.generation_allocator
            SET next_generation = next_generation + 1
            WHERE allocator_id = 1
            ",
            [],
        )
        .context("failed to advance gold generation allocator")?;
        Ok(generation)
    })
    .context("failed to reserve transactional gold generation")
}

fn delete_generation_rows(
    conn: &Connection,
    opts: &TranslateOptions,
    generation: i64,
) -> Result<()> {
    for table in REVISION_TABLES {
        let sql = format!(
            "DELETE FROM {} WHERE source = ? AND package = ? AND revision = ? AND generation = ?",
            gold_table(table)
        );
        conn.execute(
            &sql,
            params![opts.source, opts.package, opts.revision, generation],
        )
        .with_context(|| format!("failed to delete existing rows from {table}"))?;
    }
    Ok(())
}

fn validate_generation(
    conn: &Connection,
    opts: &TranslateOptions,
    generation: i64,
    rows_inserted: &HashMap<String, usize>,
) -> Result<()> {
    for table in REVISION_TABLES {
        let expected = *rows_inserted.get(*table).unwrap_or(&0) as i64;
        let sql = format!(
            "SELECT COUNT(*)::BIGINT FROM {} WHERE source = ? AND package = ? AND revision = ? AND generation = ?",
            gold_table(table)
        );
        let actual: i64 = conn
            .query_row(
                &sql,
                params![opts.source, opts.package, opts.revision, generation],
                |row| row.get(0),
            )
            .with_context(|| format!("failed to validate gold.{table} row count"))?;
        if actual != expected {
            bail!(
                "gold.{table} generation {generation} row count mismatch: expected {expected}, got {actual}"
            );
        }
    }

    let lineage = opts.lineage.clone().unwrap_or_else(default_lineage);
    if lineage.bronze_content_sha256.trim().is_empty()
        || lineage.silver_graph_content_hash.trim().is_empty()
        || lineage.builder_version.trim().is_empty()
        || lineage.translate_schema_version.trim().is_empty()
    {
        bail!("gold lineage values must be non-empty before publish");
    }
    Ok(())
}

fn insert_from_source(
    conn: &Connection,
    scope: GoldInsertScope<'_>,
    table: &str,
    source_sql: &str,
    insert_template: &str,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<()> {
    let count = count_source_rows(conn, source_sql)
        .with_context(|| format!("failed to count source rows for {table}"))?;
    let before = count_gold_generation_rows(conn, table, scope)
        .with_context(|| format!("failed to count pre-insert gold.{table} rows"))?;
    let sql = insert_template.replace("__SOURCE_SQL__", source_sql);

    // Use the single-statement prepared path for DuckLake DML. In duckdb-rs
    // 1.10504.0, execute_batch is a no-params multi-statement convenience path
    // backed by duckdb_query_arrow, which is not the same path as CLI execution
    // or prepared execution. DuckLake writes are validated after commit/flush.
    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("failed to prepare {table} insert"))?;
    stmt.execute([])
        .with_context(|| format!("failed to insert {table} rows"))?;

    let after = count_gold_generation_rows(conn, table, scope)
        .with_context(|| format!("failed to count post-insert gold.{table} rows"))?;
    if after < before {
        bail!(
            "gold.{table} generation {} row count went backwards: before {before}, after {after}",
            scope.generation
        );
    }
    let landed = usize::try_from(after - before).context("landed row delta does not fit usize")?;
    if landed != count {
        bail!(
            "gold.{table} generation {} row count mismatch: source rows {count}, landed delta {landed} (before {before}, after {after})",
            scope.generation
        );
    }
    eprintln!("[translate] {table}: inserted {count} source rows, scoped rows {before}->{after}");

    rows_inserted.insert(table.to_owned(), count);
    Ok(())
}

#[derive(Clone, Copy)]
struct GoldInsertScope<'a> {
    source: &'a str,
    package: &'a str,
    revision: &'a str,
    generation: i64,
}

impl<'a> GoldInsertScope<'a> {
    fn new(opts: &'a TranslateOptions, generation: i64) -> Self {
        Self {
            source: &opts.source,
            package: &opts.package,
            revision: &opts.revision,
            generation,
        }
    }
}

fn count_gold_generation_rows(
    conn: &Connection,
    table: &str,
    scope: GoldInsertScope<'_>,
) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*)::BIGINT FROM {} WHERE source = ? AND package = ? AND revision = ? AND generation = ?",
        gold_table(table)
    );
    conn.query_row(
        &sql,
        params![
            scope.source,
            scope.package,
            scope.revision,
            scope.generation
        ],
        |row| row.get(0),
    )
    .with_context(|| format!("failed to count gold.{table} generation rows"))
}

fn count_source_rows(conn: &Connection, source_sql: &str) -> Result<usize> {
    let sql = format!("SELECT COUNT(*)::BIGINT FROM {source_sql}");
    let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    usize::try_from(count).context("source row count does not fit usize")
}

fn latest_snapshot_id(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(snapshot_id), 0)::BIGINT FROM ducklake_snapshots('spur_context')",
        [],
        |row| row.get(0),
    )
    .context("failed to query ducklake snapshots")
}

fn update_latest_semver_ref(
    db: &Connection,
    source: &str,
    package: &str,
    revision: &str,
) -> Result<()> {
    let latest = optional_string(
        db.query_row(
            r"
            SELECT revision
            FROM gold.package_catalog
            WHERE source = ? AND package = ? AND revision_kind = 'semver'
            ORDER BY
                semver_major DESC NULLS LAST,
                semver_minor DESC NULLS LAST,
                semver_patch DESC NULLS LAST,
                revision DESC
            LIMIT 1
            ",
            params![source, package],
            |row| row.get(0),
        ),
        "failed to find latest semver revision",
    )?;

    if latest.as_deref() == Some(revision) {
        replace_ref(db, source, package, "latest", revision)?;
    }
    Ok(())
}

fn update_existing_git_refs(
    db: &Connection,
    source: &str,
    package: &str,
    revision: &str,
) -> Result<()> {
    db.execute(
        r"
        UPDATE gold.refs
        SET updated_at = CURRENT_TIMESTAMP
        WHERE source = ?
          AND package = ?
          AND revision = ?
          AND ref_name <> 'latest'
        ",
        params![source, package, revision],
    )
    .context("failed to refresh existing git refs")?;
    Ok(())
}

fn replace_ref(
    db: &Connection,
    source: &str,
    package: &str,
    ref_name: &str,
    revision: &str,
) -> Result<()> {
    db.execute(
        "DELETE FROM gold.refs WHERE source = ? AND package = ? AND ref_name = ?",
        params![source, package, ref_name],
    )
    .context("failed to delete existing ref")?;
    db.execute(
        r"
        INSERT INTO gold.refs (source, package, ref_name, revision, updated_at)
        VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
        ",
        params![source, package, ref_name, revision],
    )
    .context("failed to insert ref")?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum SidecarKind {
    CodeSymbols,
    Sections,
}

impl SidecarKind {
    fn expected_sidecar_error_context(self) -> &'static str {
        match self {
            Self::CodeSymbols => "expected symbol embeddings but failed to load Lance sidecar",
            Self::Sections => "expected section bodies but failed to load Lance sidecar",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PreparedLanceSidecars {
    code_symbols: bool,
    sections: bool,
}

impl PreparedLanceSidecars {
    fn mark_loaded(&mut self, kind: SidecarKind) {
        match kind {
            SidecarKind::CodeSymbols => self.code_symbols = true,
            SidecarKind::Sections => self.sections = true,
        }
    }

    fn is_loaded(self, kind: SidecarKind) -> bool {
        match kind {
            SidecarKind::CodeSymbols => self.code_symbols,
            SidecarKind::Sections => self.sections,
        }
    }
}

fn prepare_lance_sidecar_extensions(
    conn: &Connection,
    opts: &TranslateOptions,
) -> Result<PreparedLanceSidecars> {
    let mut prepared = PreparedLanceSidecars::default();
    let mut extension_loaded = false;

    for (relative_dir, kind) in [
        ("code_symbols.lance", SidecarKind::CodeSymbols),
        ("sections.lancedb", SidecarKind::Sections),
    ] {
        let Some(path) = lance_sidecar_path_requiring_extension(opts, relative_dir)? else {
            continue;
        };

        if !extension_loaded {
            if let Err(error) = load_lance_extension(conn) {
                skip_lance_sidecar_or_fail(
                    opts,
                    &path,
                    error,
                    kind.expected_sidecar_error_context(),
                )?;
                continue;
            }
            extension_loaded = true;
        }
        prepared.mark_loaded(kind);
    }

    Ok(prepared)
}

fn lance_sidecar_path_requiring_extension(
    opts: &TranslateOptions,
    relative_dir: &str,
) -> Result<Option<PathBuf>> {
    let path = opts.artifact_dir.join(relative_dir);
    if let Some(manifest) = &opts.artifact_manifest {
        let files = manifest_files_under(opts, manifest, relative_dir)?;
        if files.is_empty()
            || files
                .iter()
                .any(|path| path.extension().and_then(|ext| ext.to_str()) == Some("parquet"))
        {
            return Ok(None);
        }
    } else if parquet_sidecar_glob(&path)?.is_some() {
        return Ok(None);
    }

    Ok(path.exists().then_some(path))
}

fn sidecar_source(
    conn: &Connection,
    opts: &TranslateOptions,
    relative_dir: &str,
    kind: SidecarKind,
    prepared_lance: PreparedLanceSidecars,
) -> Result<Option<SidecarSource>> {
    let path = opts.artifact_dir.join(relative_dir);
    if let Some(manifest) = &opts.artifact_manifest {
        let files = manifest_files_under(opts, manifest, relative_dir)?;
        let parquet_files = files
            .iter()
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("parquet"))
            .cloned()
            .collect::<Vec<_>>();
        if !parquet_files.is_empty() {
            return Ok(Some(SidecarSource::Parquet(read_parquet_files_source(
                &parquet_files,
            ))));
        }
        if files.is_empty() {
            return Ok(None);
        }
    } else if let Some(glob) = parquet_sidecar_glob(&path)? {
        return Ok(Some(SidecarSource::Parquet(read_parquet_glob_source(
            &glob,
        ))));
    }

    if !path.exists() {
        return Ok(None);
    }

    if !prepared_lance.is_loaded(kind) {
        return Ok(None);
    }

    if matches!(kind, SidecarKind::Sections) {
        let attach_sql = format!(
            "ATTACH '{}' AS spur_context_sections_lance (TYPE lance);",
            escape_sql_literal(&path.display().to_string())
        );
        if conn.execute_batch(&attach_sql).is_ok() {
            return Ok(Some(SidecarSource::Lance(
                "spur_context_sections_lance.main.section_bodies".to_owned(),
            )));
        }
    }

    Ok(Some(SidecarSource::Lance(sql_string(
        &path.display().to_string(),
    ))))
}

fn source_columns(conn: &Connection, source_sql: &str) -> Result<BTreeSet<String>> {
    let sql = format!("DESCRIBE SELECT * FROM {source_sql} LIMIT 0");
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to describe sidecar source")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read sidecar source columns")?;
    Ok(columns.into_iter().collect())
}

fn parquet_sidecar_glob(path: &Path) -> Result<Option<PathBuf>> {
    if path.is_file() {
        return Ok(
            (path.extension().and_then(|ext| ext.to_str()) == Some("parquet"))
                .then(|| path.to_path_buf()),
        );
    }
    if !path.is_dir() || !contains_parquet_file(path)? {
        return Ok(None);
    }
    Ok(Some(path.join("**").join("*.parquet")))
}

fn contains_parquet_file(path: &Path) -> Result<bool> {
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read `{}`", dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in `{}`", dir.display()))?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if entry_path.extension().and_then(|ext| ext.to_str()) == Some("parquet") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn required_artifact_file(opts: &TranslateOptions, file_name: &str) -> Result<PathBuf> {
    let path = if let Some(manifest) = &opts.artifact_manifest {
        manifest_file_path(opts, manifest, file_name)?
            .ok_or_else(|| anyhow!("manifest missing required artifact file `{file_name}`"))?
    } else {
        opts.artifact_dir.join(file_name)
    };
    if !path.is_file() {
        bail!("missing required artifact file `{}`", path.display());
    }
    Ok(path)
}

fn optional_artifact_source(
    opts: &TranslateOptions,
    file_name: &str,
    glob: Option<&str>,
) -> Result<Option<String>> {
    if let Some(manifest) = &opts.artifact_manifest {
        if let Some(path) = manifest_file_path(opts, manifest, file_name)? {
            if path.is_file() {
                return Ok(Some(read_parquet_source(&path)));
            }
        }
        if let Some(glob) = glob {
            let paths = manifest_files_for_glob(opts, manifest, glob)?;
            if !paths.is_empty() {
                return Ok(Some(read_parquet_files_source(&paths)));
            }
        }
        return Ok(None);
    }

    let artifact_dir = &opts.artifact_dir;
    let file = artifact_dir.join(file_name);
    if file.is_file() {
        return Ok(Some(read_parquet_source(&file)));
    }
    if let Some(glob) = glob {
        let glob_path = artifact_dir.join(glob);
        let glob_root = glob_search_root(artifact_dir, glob);
        if glob_root.is_dir() && contains_parquet_file(&glob_root)? {
            return Ok(Some(read_parquet_glob_source(&glob_path)));
        }
    }
    Ok(None)
}

fn glob_search_root(artifact_dir: &Path, glob: &str) -> PathBuf {
    let literal_prefix = glob
        .find(|ch| ['*', '?'].contains(&ch))
        .map(|index| &glob[..index])
        .unwrap_or(glob)
        .trim_end_matches('/');
    artifact_dir.join(literal_prefix)
}

fn read_parquet_source(path: &Path) -> String {
    format!(
        "read_parquet('{}')",
        escape_sql_literal(&path.display().to_string())
    )
}

fn read_parquet_glob_source(path: &Path) -> String {
    format!(
        "read_parquet('{}')",
        escape_sql_literal(&path.display().to_string())
    )
}

fn read_parquet_files_source(paths: &[PathBuf]) -> String {
    if paths.len() == 1 {
        return read_parquet_source(&paths[0]);
    }
    let paths = paths
        .iter()
        .map(|path| format!("'{}'", escape_sql_literal(&path.display().to_string())))
        .collect::<Vec<_>>()
        .join(", ");
    format!("read_parquet([{paths}])")
}

fn manifest_file_path(
    opts: &TranslateOptions,
    manifest: &SilverManifest,
    relative_path: &str,
) -> Result<Option<PathBuf>> {
    validate_manifest_relative_path(relative_path)?;
    Ok(manifest
        .files
        .iter()
        .any(|file| file.path == relative_path)
        .then(|| {
            opts.artifact_dir
                .join(path_from_manifest_relative(relative_path))
        }))
}

fn manifest_files_under(
    opts: &TranslateOptions,
    manifest: &SilverManifest,
    relative_dir: &str,
) -> Result<Vec<PathBuf>> {
    validate_manifest_relative_path(relative_dir)?;
    let prefix = format!("{}/", relative_dir.trim_end_matches('/'));
    let mut files = Vec::new();
    for file in &manifest.files {
        validate_manifest_relative_path(&file.path)?;
        if file.path.starts_with(&prefix) {
            files.push(
                opts.artifact_dir
                    .join(path_from_manifest_relative(&file.path)),
            );
        }
    }
    files.sort();
    Ok(files)
}

fn manifest_files_for_glob(
    opts: &TranslateOptions,
    manifest: &SilverManifest,
    glob: &str,
) -> Result<Vec<PathBuf>> {
    let root = glob
        .find(|ch| ['*', '?'].contains(&ch))
        .map(|index| &glob[..index])
        .unwrap_or(glob)
        .trim_end_matches('/');
    let mut paths = manifest_files_under(opts, manifest, root)?;
    paths.retain(|path| path.extension().and_then(|ext| ext.to_str()) == Some("parquet"));
    Ok(paths)
}

fn validate_manifest_relative_path(path: &str) -> Result<()> {
    if path.trim().is_empty() || path.contains('\\') {
        bail!("invalid manifest path `{path}`");
    }
    let relative = Path::new(path);
    if relative.is_absolute() {
        bail!("manifest path must be relative: `{path}`");
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("manifest path escapes artifact root: `{path}`");
            }
        }
    }
    Ok(())
}

fn path_from_manifest_relative(relative_path: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for part in relative_path.split('/') {
        path.push(part);
    }
    path
}

fn run_transaction<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN TRANSACTION")
        .context("failed to begin DuckLake transaction")?;
    match f() {
        Ok(value) => {
            conn.execute_batch("COMMIT")
                .context("failed to commit DuckLake transaction")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn load_ducklake_extensions(conn: &Connection, catalog_dsn: &str, data_path: &str) -> Result<()> {
    load_duckdb_extension(conn, "ducklake", "failed to load ducklake extension")?;

    // Always load httpfs + S3 credentials when either the catalog OR the data
    // path is on S3. The catalog may be local (downloaded via CatalogDownload)
    // while the data path is still s3:// — without S3 creds, DuckLake's INSERT
    // fails with HTTP 403 writing parquet data files to S3.
    let needs_s3 = is_remote_catalog(catalog_dsn) || data_path.starts_with("s3://");
    if needs_s3 {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());

        // Resolve AWS credentials. On Fargate, the credential_chain provider
        // may not support the ECS credentials endpoint, so we fetch explicit
        // credentials and create an S3 secret with them.
        let creds = fetch_aws_credentials();
        match creds {
            Some(c) => {
                eprintln!(
                    "[translate] using explicit AWS credentials (provider={})",
                    c.provider
                );
                let token_clause = if let Some(session_token) = &c.session_token {
                    format!(", SESSION_TOKEN '{}'", session_token.replace('\'', "''"))
                } else {
                    String::new()
                };
                load_duckdb_extension(
                    conn,
                    "httpfs",
                    "failed to load httpfs with explicit S3 credentials",
                )?;
                conn.execute_batch(&format!(
                    "CREATE OR REPLACE SECRET s3_creds (TYPE s3, \
                     KEY_ID '{}', SECRET '{}', REGION '{}'{});",
                    c.access_key_id.replace('\'', "''"),
                    c.secret_access_key.replace('\'', "''"),
                    region,
                    token_clause,
                ))
                .context("failed to configure explicit S3 credentials")?;
            }
            None => {
                eprintln!(
                    "[translate] no explicit AWS creds found, falling back to credential_chain"
                );
                load_duckdb_extension(
                    conn,
                    "httpfs",
                    "failed to load httpfs extension for S3 access",
                )?;
                conn.execute_batch(&format!(
                    "CREATE OR REPLACE SECRET s3_creds (TYPE s3, PROVIDER credential_chain, REGION '{}');",
                    region,
                ))
                .context("failed to configure S3 credentials")?;
            }
        }
    } else if is_sqlite_catalog(catalog_dsn) {
        load_duckdb_extension(
            conn,
            "sqlite",
            "failed to load sqlite extension for DuckLake catalog",
        )?;
    } else if is_postgres_catalog(catalog_dsn) {
        load_duckdb_extension(
            conn,
            "postgres",
            "failed to load postgres extension for DuckLake catalog",
        )?;
    }

    Ok(())
}

fn load_lance_extension(conn: &Connection) -> Result<()> {
    // Load lance via the standard bundled-extension path: sets extension_directory
    // to the baked-in /opt/duckdb/extensions and autoinstall_known_extensions=false,
    // then LOADs from disk (same as httpfs/ducklake/etc.). Replaces the old bare
    // `LOAD lance` + network `INSTALL lance` fallback, which hung on the worker's
    // egress-less VPC. Local dev (no SPUR_CONTEXT_DUCKDB_EXTENSION_DIR) still
    // INSTALL+LOADs via the helper's None branch.
    load_duckdb_extension(conn, "lance", "failed to load lance extension")
}

fn attach_ducklake(conn: &Connection, catalog_dsn: &str, data_path: &str) -> Result<()> {
    let attach_uri = if catalog_dsn.starts_with("ducklake:") {
        catalog_dsn.to_owned()
    } else {
        format!("ducklake:{catalog_dsn}")
    };

    // OVERRIDE_DATA_PATH TRUE allows the ATTACH to use a different DATA_PATH
    // than what's stored in the catalog metadata. This is needed for the
    // download-modify-upload pattern where the worker uses a local data path
    // during translate and uploads data files to S3 afterwards.
    conn.execute_batch(&format!(
        "ATTACH '{}' AS spur_context (DATA_PATH '{}', OVERRIDE_DATA_PATH TRUE, AUTOMATIC_MIGRATION TRUE); USE spur_context;",
        escape_sql_literal(&attach_uri),
        escape_sql_literal(data_path)
    ))
    .context("failed to attach DuckLake catalog")
}

/// Ensures all expected catalog tables exist. The catalog may have been
/// created with a partial schema (e.g. only `nodes`), so we run the full DDL
/// with CREATE TABLE IF NOT EXISTS to add any missing tables before translate.
fn ensure_catalog_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(CATALOG_TABLES_SQL)
        .context("failed to ensure catalog schema (CREATE TABLE IF NOT EXISTS)")?;
    Ok(())
}

fn is_remote_catalog(catalog_dsn: &str) -> bool {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    dsn.starts_with("s3://") || dsn.starts_with("https://") || dsn.starts_with("http://")
}

fn is_sqlite_catalog(catalog_dsn: &str) -> bool {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    dsn.starts_with("sqlite:")
}

fn is_postgres_catalog(catalog_dsn: &str) -> bool {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    dsn.starts_with("postgres:")
        || dsn.starts_with("postgresql:")
        || dsn.starts_with("postgresql://")
}

fn optional_string(
    result: duckdb::Result<String>,
    context: &'static str,
) -> Result<Option<String>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context(context),
    }
}

fn parse_semver_triplet(revision: &str) -> Option<(i32, i32, i32)> {
    let version = revision.strip_prefix('v').unwrap_or(revision);
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_part = parts.next()?;
    let patch_digits = patch_part
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    let patch = patch_digits.parse().ok()?;
    Some((major, minor, patch))
}

fn sql_string(value: &str) -> String {
    format!("'{}'", escape_sql_literal(value))
}

fn sql_i32(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "NULL::INTEGER".to_owned())
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn warn_skip_sidecar(path: &Path, error: &anyhow::Error) {
    eprintln!(
        "warning: skipping Lance sidecar `{}` during DuckLake translation: {error:#}",
        path.display()
    );
}

fn skip_lance_sidecar_or_fail(
    opts: &TranslateOptions,
    path: &Path,
    error: anyhow::Error,
    context: &'static str,
) -> Result<()> {
    if opts.allow_missing_embeddings {
        warn_skip_sidecar(path, &error);
        Ok(())
    } else {
        Err(error).with_context(|| format!("{context}: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock should not be poisoned")
    }

    #[test]
    fn insert_from_source_fails_when_landed_delta_mismatches_source_count() -> Result<()> {
        let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
        conn.execute_batch(
            r"
            CREATE SCHEMA gold;
            CREATE TABLE gold.nodes (
                source VARCHAR,
                package VARCHAR,
                revision VARCHAR,
                generation BIGINT,
                value BIGINT
            );
            CREATE TEMP TABLE source_rows (value BIGINT);
            INSERT INTO source_rows VALUES (1);
            ",
        )
        .context("create test tables")?;

        let mut rows_inserted = HashMap::new();
        let scope = GoldInsertScope {
            source: "registry:crates-io",
            package: "demo",
            revision: "1.2.3",
            generation: 7,
        };
        let error = insert_from_source(
            &conn,
            scope,
            "nodes",
            "(SELECT * FROM source_rows)",
            r"
            INSERT INTO gold.nodes (source, package, revision, generation, value)
            SELECT 'registry:crates-io', 'demo', '1.2.3', 7, value
            FROM __SOURCE_SQL__
            WHERE false
            ",
            &mut rows_inserted,
        )
        .expect_err("insert helper must reject a landed row delta mismatch");

        assert!(
            format!("{error:#}").contains("row count mismatch"),
            "unexpected error: {error:#}"
        );
        assert!(!rows_inserted.contains_key("nodes"));
        Ok(())
    }

    #[test]
    fn ducklake_data_path_requires_env_for_postgres_catalog() {
        let _guard = lock_env();
        let previous = std::env::var_os("SPUR_CONTEXT_DUCKLAKE_DATA_PATH");
        std::env::remove_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH");

        let error = ducklake_data_path("postgres:host=localhost port=5432 dbname=spur_context")
            .expect_err("postgres catalogs must not fall back to a hard-coded S3 data path");

        match previous {
            Some(value) => std::env::set_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH", value),
            None => std::env::remove_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH"),
        }

        assert!(
            format!("{error:#}").contains("SPUR_CONTEXT_DUCKLAKE_DATA_PATH"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn postgres_generation_allocator_uses_sequence_not_max_scan() {
        let sql = postgres_generation_allocator_sql("pg_alloc");

        assert!(
            sql.create_sequence
                .contains("CREATE SEQUENCE IF NOT EXISTS public.spur_context_gold_generation_seq"),
            "sequence creation SQL must create the shared Postgres sequence: {}",
            sql.create_sequence
        );
        assert!(
            sql.reserve_generation
                .contains("nextval(''public.spur_context_gold_generation_seq'')"),
            "reservation SQL must use nextval(): {}",
            sql.reserve_generation
        );
        assert!(
            !sql.reserve_generation.to_ascii_uppercase().contains("MAX("),
            "generation allocation must not use MAX()+1: {}",
            sql.reserve_generation
        );
    }

    #[test]
    fn postgres_gold_publish_lock_uses_advisory_session_lock() {
        let sql = postgres_gold_publish_lock_sql("pg_publish");

        assert!(
            sql.acquire_lock.contains("pg_advisory_lock"),
            "Aurora gold publish must use a Postgres advisory lock: {}",
            sql.acquire_lock
        );
        assert!(
            sql.acquire_lock.contains("postgres_query('pg_publish'"),
            "lock SQL must run against the attached Postgres metadata catalog: {}",
            sql.acquire_lock
        );
    }

    #[test]
    fn postgres_publish_lock_boundary_excludes_schema_and_lance_prepare() {
        let body = translate_artifact_to_ducklake_source();

        let attach = body
            .find("attach_postgres_metadata_catalog(&conn, &catalog_dsn)?")
            .expect("translate must attach the Postgres metadata catalog once");
        let ensure_schema = body
            .find("ensure_catalog_schema(&conn)?")
            .expect("translate must ensure the catalog schema");
        let prepare_lance = body
            .find("prepare_lance_sidecar_extensions(&conn, opts)?")
            .expect("translate must prepare Lance extensions before publishing");
        let acquire_lock = body
            .find("acquire_postgres_gold_publish_lock(&conn, postgres_metadata.as_ref())?")
            .expect("translate must acquire the Postgres publish lock");
        let reserve_generation = body
            .find("next_generation(&conn, &catalog_dsn, postgres_metadata.as_ref())?")
            .expect("translate must reserve a gold generation");

        assert!(
            attach < acquire_lock,
            "Postgres metadata ATTACH must happen before lock acquisition"
        );
        assert!(
            ensure_schema < acquire_lock,
            "catalog schema DDL must run outside the Postgres publish lock"
        );
        assert!(
            prepare_lance < acquire_lock,
            "Lance extension INSTALL/LOAD must run outside the Postgres publish lock"
        );
        assert!(
            acquire_lock < reserve_generation,
            "Postgres generation reservation must remain inside the publish lock"
        );
    }

    #[test]
    fn translate_phase_timing_line_uses_translate_prefix_and_elapsed_millis() {
        let line = translate_phase_timing_line(
            "load_ducklake_extensions+attach_ducklake",
            std::time::Duration::from_millis(42),
        );

        assert_eq!(
            line,
            "[translate] phase load_ducklake_extensions+attach_ducklake elapsed_ms=42"
        );
    }

    fn translate_artifact_to_ducklake_source() -> &'static str {
        let source = include_str!("translate.rs");
        let start = source
            .find("pub fn translate_artifact_to_ducklake")
            .expect("translate_artifact_to_ducklake should exist");
        let rest = &source[start..];
        let end = rest
            .find("\npub fn update_refs")
            .expect("update_refs should follow translate_artifact_to_ducklake");
        &rest[..end]
    }
}
