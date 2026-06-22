//! Build-pipeline translation from spur-graph artifacts into `DuckLake` tables.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use duckdb::{params, Connection};

const DEFAULT_DATA_PATH: &str = "s3://spur-context/data/";
const DEFAULT_EMBEDDING_MODEL: &str = "JinaEmbeddingsV2BaseCode";
const DEFAULT_EMBED_TEXT_VERSION: &str = "v2-jina-code";

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
    pub catalog_dsn: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateStats {
    pub rows_inserted: HashMap<String, usize>,
    pub snapshot_id: i64,
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

pub fn translate_artifact_to_ducklake(opts: &TranslateOptions) -> Result<TranslateStats> {
    validate_options(opts)?;
    let revision = revision_parts(&opts.revision, &opts.revision_kind)?;
    let data_path = ducklake_data_path(&opts.catalog_dsn)?;

    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    load_ducklake_extensions(&conn, &opts.catalog_dsn)?;
    attach_ducklake(&conn, &opts.catalog_dsn, &data_path)?;

    let mut rows_inserted = HashMap::new();
    let embeddings_translated = run_transaction(&conn, || {
        delete_existing_revision_rows(&conn, opts)?;
        insert_structural_tables(&conn, opts, revision, &mut rows_inserted)?;
        insert_git_tables(&conn, opts, revision, &mut rows_inserted)?;
        insert_sidecar_tables(&conn, opts, revision, &mut rows_inserted)
    })
    .context("failed to translate artifact tables into DuckLake")?;

    let snapshot_id = latest_snapshot_id(&conn).context("failed to read DuckLake snapshot id")?;
    write_catalog_metadata(
        &conn,
        opts,
        revision,
        snapshot_id,
        embeddings_translated,
        &rows_inserted,
    )
    .context("failed to update package catalog metadata")?;

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
            FROM package_catalog
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
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<()> {
    let nodes = required_artifact_file(&opts.artifact_dir, "nodes.parquet")?;
    let edges = required_artifact_file(&opts.artifact_dir, "edges.parquet")?;
    let unresolved = required_artifact_file(&opts.artifact_dir, "edges_unresolved.parquet")?;
    let files = required_artifact_file(&opts.artifact_dir, "files.parquet")?;
    let file_manifests = required_artifact_file(&opts.artifact_dir, "file_manifests.parquet")?;

    insert_from_source(
        conn,
        "nodes",
        &read_parquet_source(&nodes),
        &format!(
            r"
            INSERT INTO nodes (
                stable_symbol_id, package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                file_path, byte_range_start, byte_range_end,
                line_start, line_end, entity_name, qualified_name,
                symbol_kind, anchor_hash, enclosing_scope
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
                symbol_kind, anchor_hash, enclosing_scope
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        "edges",
        &read_parquet_source(&edges),
        &format!(
            r"
            INSERT INTO edges (
                source_stable_id, target_stable_id, target_package, target_label,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                relation, edge_kind, confidence, confidence_score,
                bind_method, receiver_text, scope_text
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
                bind_method, receiver_text, scope_text
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        "edges_unresolved",
        &read_parquet_source(&unresolved),
        &format!(
            r"
            INSERT INTO edges_unresolved (
                source_stable_id, target_label, target_package,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                relation, edge_kind, confidence, confidence_score,
                bind_method, receiver_text, scope_text
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
                bind_method, receiver_text, scope_text
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        "files",
        &read_parquet_source(&files),
        &format!(
            r"
            INSERT INTO files (
                stable_file_id, file_path, source_text,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch
            )
            SELECT
                stable_file_id,
                file_path,
                CAST(NULL AS VARCHAR) AS source_text,
                {package} AS package,
                {source} AS source,
                {rev} AS revision,
                {rev_kind} AS revision_kind,
                {major} AS semver_major,
                {minor} AS semver_minor,
                {patch} AS semver_patch
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
        ),
        rows_inserted,
    )?;

    insert_from_source(
        conn,
        "file_manifests",
        &read_parquet_source(&file_manifests),
        &format!(
            r"
            INSERT INTO file_manifests (
                stable_file_id, path, content_oid, node_ids,
                package, source, revision, revision_kind,
                semver_major, semver_minor, semver_patch
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
                {patch} AS semver_patch
            FROM __SOURCE_SQL__
            ",
            package = sql_string(&opts.package),
            source = sql_string(&opts.source),
            rev = sql_string(&opts.revision),
            rev_kind = sql_string(&opts.revision_kind),
            major = sql_i32(revision.major),
            minor = sql_i32(revision.minor),
            patch = sql_i32(revision.patch),
        ),
        rows_inserted,
    )?;

    Ok(())
}

fn insert_git_tables(
    conn: &Connection,
    opts: &TranslateOptions,
    revision: RevisionParts,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<()> {
    if opts.revision_kind != "git_sha" {
        rows_inserted.insert("commits".to_owned(), 0);
        rows_inserted.insert("symbol_snapshots".to_owned(), 0);
        rows_inserted.insert("temporal_edges".to_owned(), 0);
        return Ok(());
    }

    if let Some(commits) = optional_artifact_source(&opts.artifact_dir, "commits.parquet", None)? {
        insert_from_source(
            conn,
            "commits",
            &commits,
            &format!(
                r"
                INSERT INTO commits (
                    sha, parents, author_time, author_name, author_email, summary,
                    package, source, revision, revision_kind,
                    semver_major, semver_minor, semver_patch
                )
                SELECT
                    sha, parents, author_time, author_name, author_email, summary,
                    {package} AS package,
                    {source} AS source,
                    {rev} AS revision,
                    {rev_kind} AS revision_kind,
                    {major} AS semver_major,
                    {minor} AS semver_minor,
                    {patch} AS semver_patch
                FROM __SOURCE_SQL__
                ",
                package = sql_string(&opts.package),
                source = sql_string(&opts.source),
                rev = sql_string(&opts.revision),
                rev_kind = sql_string(&opts.revision_kind),
                major = sql_i32(revision.major),
                minor = sql_i32(revision.minor),
                patch = sql_i32(revision.patch),
            ),
            rows_inserted,
        )?;
    } else {
        rows_inserted.insert("commits".to_owned(), 0);
    }

    if let Some(snapshots) = optional_artifact_source(
        &opts.artifact_dir,
        "symbol_snapshots.parquet",
        Some("symbol_snapshots/**/*.parquet"),
    )? {
        insert_from_source(
            conn,
            "symbol_snapshots",
            &snapshots,
            &format!(
                r"
                INSERT INTO symbol_snapshots (
                    stable_symbol_id, commit, package, source, revision, revision_kind,
                    semver_major, semver_minor, semver_patch,
                    file_path, entity_name, symbol_kind, enclosing_scope,
                    byte_range, line_range, anchor_hash
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
                    anchor_hash
                FROM __SOURCE_SQL__
                ",
                package = sql_string(&opts.package),
                source = sql_string(&opts.source),
                rev = sql_string(&opts.revision),
                rev_kind = sql_string(&opts.revision_kind),
                major = sql_i32(revision.major),
                minor = sql_i32(revision.minor),
                patch = sql_i32(revision.patch),
            ),
            rows_inserted,
        )?;
    } else {
        rows_inserted.insert("symbol_snapshots".to_owned(), 0);
    }

    if let Some(temporal_edges) = optional_artifact_source(
        &opts.artifact_dir,
        "temporal_edges.parquet",
        Some("temporal_edges/**/*.parquet"),
    )? {
        insert_from_source(
            conn,
            "temporal_edges",
            &temporal_edges,
            &format!(
                r"
                INSERT INTO temporal_edges (
                    source_endpoint, target_endpoint, relation, change_kind, parent,
                    package, source, revision, revision_kind,
                    semver_major, semver_minor, semver_patch
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
                    {patch} AS semver_patch
                FROM __SOURCE_SQL__
                ",
                package = sql_string(&opts.package),
                source = sql_string(&opts.source),
                rev = sql_string(&opts.revision),
                rev_kind = sql_string(&opts.revision_kind),
                major = sql_i32(revision.major),
                minor = sql_i32(revision.minor),
                patch = sql_i32(revision.patch),
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
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<bool> {
    let symbols_translated = insert_symbol_embeddings(conn, opts, revision, rows_inserted)
        .with_context(|| {
            format!(
                "failed to translate `{}`",
                opts.artifact_dir.join("code_symbols.lance").display()
            )
        })?;
    insert_section_bodies(conn, opts, revision, rows_inserted).with_context(|| {
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
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<bool> {
    let sidecar_path = opts.artifact_dir.join("code_symbols.lance");
    let Some(source) = sidecar_source(conn, &sidecar_path, SidecarKind::CodeSymbols)? else {
        rows_inserted.insert("symbol_embeddings".to_owned(), 0);
        return Ok(false);
    };

    let columns = match source_columns(conn, source.sql()) {
        Ok(columns) => columns,
        Err(error) if source.is_lance() => {
            warn_skip_sidecar(&sidecar_path, &error);
            rows_inserted.insert("symbol_embeddings".to_owned(), 0);
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let embedding_expr = if columns.contains("vector") {
        "vector"
    } else if columns.contains("embedding") {
        "embedding"
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

    let template = format!(
        r"
        INSERT INTO symbol_embeddings (
            stable_symbol_id, package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            file_path, entity_name, qualified_name, symbol_kind,
            embedding, embedding_model, embedding_input_hash, embed_text_version
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
            {embed_text_version} AS embed_text_version
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
    );

    match insert_from_source(
        conn,
        "symbol_embeddings",
        source.sql(),
        &template,
        rows_inserted,
    ) {
        Ok(()) => Ok(true),
        Err(error) if source.is_lance() => {
            warn_skip_sidecar(&sidecar_path, &error);
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
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<bool> {
    let sidecar_path = opts.artifact_dir.join("sections.lancedb");
    let Some(source) = sidecar_source(conn, &sidecar_path, SidecarKind::Sections)? else {
        rows_inserted.insert("section_bodies".to_owned(), 0);
        return Ok(false);
    };

    let columns = match source_columns(conn, source.sql()) {
        Ok(columns) => columns,
        Err(error) if source.is_lance() => {
            warn_skip_sidecar(&sidecar_path, &error);
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

    let template = format!(
        r"
        INSERT INTO section_bodies (
            section_id, package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            file_path, title, body_text, body_hash, token_count
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
            {token_count_expr} AS token_count
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
    );

    match insert_from_source(
        conn,
        "section_bodies",
        source.sql(),
        &template,
        rows_inserted,
    ) {
        Ok(()) => Ok(true),
        Err(error) if source.is_lance() => {
            warn_skip_sidecar(&sidecar_path, &error);
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

    run_transaction(conn, || {
        conn.execute(
            "DELETE FROM package_catalog WHERE source = ? AND package = ? AND revision = ?",
            params![opts.source, opts.package, opts.revision],
        )
        .context("failed to delete existing package_catalog row")?;
        conn.execute(
            r"
            INSERT INTO package_catalog (
                source, package, revision, revision_kind,
                semver_major, semver_minor, semver_patch,
                snapshot_id, indexed_at, index_status, embeddings_status, row_counts
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, 'complete', ?, CAST(? AS JSON))
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
                row_counts
            ],
        )
        .context("failed to insert package_catalog row")?;
        update_refs(conn, &opts.source, &opts.package, &opts.revision)?;
        Ok(())
    })
}

fn delete_existing_revision_rows(conn: &Connection, opts: &TranslateOptions) -> Result<()> {
    for table in REVISION_TABLES {
        let sql = format!("DELETE FROM {table} WHERE source = ? AND package = ? AND revision = ?");
        conn.execute(&sql, params![opts.source, opts.package, opts.revision])
            .with_context(|| format!("failed to delete existing rows from {table}"))?;
    }
    Ok(())
}

fn insert_from_source(
    conn: &Connection,
    table: &str,
    source_sql: &str,
    insert_template: &str,
    rows_inserted: &mut HashMap<String, usize>,
) -> Result<()> {
    let count = count_source_rows(conn, source_sql)
        .with_context(|| format!("failed to count source rows for {table}"))?;
    let sql = insert_template.replace("__SOURCE_SQL__", source_sql);
    conn.execute_batch(&sql)
        .with_context(|| format!("failed to insert {table} rows"))?;
    rows_inserted.insert(table.to_owned(), count);
    Ok(())
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
            FROM package_catalog
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
        UPDATE refs
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
        "DELETE FROM refs WHERE source = ? AND package = ? AND ref_name = ?",
        params![source, package, ref_name],
    )
    .context("failed to delete existing ref")?;
    db.execute(
        r"
        INSERT INTO refs (source, package, ref_name, revision, updated_at)
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

fn sidecar_source(
    conn: &Connection,
    path: &Path,
    kind: SidecarKind,
) -> Result<Option<SidecarSource>> {
    if let Some(glob) = parquet_sidecar_glob(path)? {
        return Ok(Some(SidecarSource::Parquet(read_parquet_glob_source(
            &glob,
        ))));
    }

    if !path.exists() {
        return Ok(None);
    }

    if let Err(error) = load_lance_extension(conn) {
        warn_skip_sidecar(path, &error);
        return Ok(None);
    }

    if matches!(kind, SidecarKind::Sections) {
        let attach_sql = format!(
            "ATTACH '{}' AS spur_context_sections_lance (TYPE lancedb);",
            escape_sql_literal(&path.display().to_string())
        );
        if conn.execute_batch(&attach_sql).is_ok() {
            return Ok(Some(SidecarSource::Lance(
                "spur_context_sections_lance.section_bodies".to_owned(),
            )));
        }
    }

    Ok(Some(SidecarSource::Lance(format!(
        "lance_scan('{}')",
        escape_sql_literal(&path.display().to_string())
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

fn required_artifact_file(artifact_dir: &Path, file_name: &str) -> Result<PathBuf> {
    let path = artifact_dir.join(file_name);
    if !path.is_file() {
        bail!("missing required artifact file `{}`", path.display());
    }
    Ok(path)
}

fn optional_artifact_source(
    artifact_dir: &Path,
    file_name: &str,
    glob: Option<&str>,
) -> Result<Option<String>> {
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

fn ducklake_data_path(catalog_dsn: &str) -> Result<String> {
    if let Ok(path) = std::env::var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH") {
        if !path.trim().is_empty() {
            create_local_data_path_if_needed(&path)?;
            return Ok(path);
        }
    }

    if let Some(sqlite_path) = sqlite_catalog_path(catalog_dsn) {
        let path = sqlite_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("data");
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create DuckLake data path `{}`", path.display()))?;
        return Ok(path.display().to_string());
    }

    Ok(DEFAULT_DATA_PATH.to_owned())
}

fn create_local_data_path_if_needed(path: &str) -> Result<()> {
    if path.contains("://") || path == ":memory:" {
        return Ok(());
    }
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create DuckLake data path `{path}`"))?;
    Ok(())
}

fn sqlite_catalog_path(catalog_dsn: &str) -> Option<PathBuf> {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    let path = dsn.strip_prefix("sqlite:")?;
    (path != ":memory:").then(|| PathBuf::from(path))
}

fn load_ducklake_extensions(conn: &Connection, catalog_dsn: &str) -> Result<()> {
    conn.execute_batch("INSTALL ducklake; LOAD ducklake;")
        .context("failed to load ducklake extension")?;

    if is_sqlite_catalog(catalog_dsn) {
        conn.execute_batch("INSTALL sqlite; LOAD sqlite;")
            .context("failed to load sqlite extension for DuckLake catalog")?;
    } else if is_postgres_catalog(catalog_dsn) {
        conn.execute_batch("INSTALL postgres; LOAD postgres;")
            .context("failed to load postgres extension for DuckLake catalog")?;
    }

    Ok(())
}

fn load_lance_extension(conn: &Connection) -> Result<()> {
    conn.execute_batch("INSTALL lance; LOAD lance;")
        .context("failed to load lance extension")
}

fn attach_ducklake(conn: &Connection, catalog_dsn: &str, data_path: &str) -> Result<()> {
    let attach_uri = if catalog_dsn.starts_with("ducklake:") {
        catalog_dsn.to_owned()
    } else {
        format!("ducklake:{catalog_dsn}")
    };

    conn.execute_batch(&format!(
        "ATTACH '{}' AS spur_context (DATA_PATH '{}'); USE spur_context;",
        escape_sql_literal(&attach_uri),
        escape_sql_literal(data_path)
    ))
    .context("failed to attach DuckLake catalog")
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
