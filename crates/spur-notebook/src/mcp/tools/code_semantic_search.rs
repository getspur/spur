//! `code_semantic_search` — BM25 retrieval over the analyst DuckDB's doc + code
//! corpora (`search()` / `search_docs()` / `search_code()` macros).
//!
//! Complements `code_symbol_search` (spur-mcp): that tool matches symbol *names*
//! lexically; this one ranks *content* (section bodies + symbol token text) by
//! relevance, and — for code hits — fuses the scorecard signal (centrality /
//! churn / posture). Reads `.spur/analyst.duckdb` read-only via the bundled
//! libduckdb. Two bundled extensions are needed at query time: `fts` (autoloads
//! for `match_bm25`) and `icu`. The temporal views behind the code scorecard
//! (`v_symbol_churn_90d` → `posture`) filter on `now() - INTERVAL '90 day'`
//! against a TIMESTAMP WITH TIME ZONE column; that `-(TIMESTAMPTZ, INTERVAL)`
//! overload lives in `icu`, NOT core, and is not autoloaded — so the `code`/`all`
//! scopes fail to bind read-only ("No function matches ... -(TIMESTAMP WITH TIME
//! ZONE, INTERVAL)") unless we `LOAD icu` first. icu is statically linked into
//! libduckdb-sys, so a plain `LOAD` (no network install) suffices. Other
//! community extensions used to BUILD the DB are irrelevant to the read path.

#[cfg(feature = "datasource-introspect")]
use arrow_array::{Array, Float32Array, StringArray};
#[cfg(feature = "datasource-introspect")]
use futures::TryStreamExt;
#[cfg(feature = "datasource-introspect")]
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(feature = "datasource-introspect")]
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const METHOD: &str = "code_semantic_search";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
#[cfg(feature = "datasource-introspect")]
const EMBED_DIM: usize = 768;
#[cfg(feature = "datasource-introspect")]
const EMBED_CACHE_ENTRIES: usize = 1024;
#[cfg(feature = "datasource-introspect")]
const HYBRID_CANDIDATES: usize = 30;
#[cfg(feature = "datasource-introspect")]
const HYBRID_RRF_K: f64 = 60.0;
#[cfg(feature = "datasource-introspect")]
const HYBRID_PER_DOC_LIMIT: usize = 3;
#[cfg(feature = "datasource-introspect")]
const SECTIONS_DATASET_DIR: &str = "sections.lancedb";
#[cfg(feature = "datasource-introspect")]
const SECTIONS_TABLE: &str = "section_bodies";

#[cfg(feature = "datasource-introspect")]
static EMBED_MODEL: OnceLock<Option<fastembed::TextEmbedding>> = OnceLock::new();
#[cfg(feature = "datasource-introspect")]
static EMBED_CACHE: OnceLock<Mutex<EmbedCache>> = OnceLock::new();
#[cfg(feature = "datasource-introspect")]
static LANCE_CONNECTIONS: OnceLock<Mutex<HashMap<PathBuf, lancedb::Connection>>> = OnceLock::new();

#[cfg(feature = "datasource-introspect")]
struct EmbedCache {
    entries: HashMap<u64, [f32; EMBED_DIM]>,
    order: VecDeque<u64>,
    capacity: usize,
}

#[cfg(feature = "datasource-introspect")]
impl EmbedCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, key: u64) -> Option<[f32; EMBED_DIM]> {
        let embedding = self.entries.get(&key).copied()?;
        self.touch(key);
        Some(embedding)
    }

    fn insert(&mut self, key: u64, embedding: [f32; EMBED_DIM]) {
        self.entries.insert(key, embedding);
        self.touch(key);
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn touch(&mut self, key: u64) {
        self.order.retain(|existing| *existing != key);
        self.order.push_back(key);
    }
}

#[cfg(feature = "datasource-introspect")]
fn get_embed_model() -> Option<&'static fastembed::TextEmbedding> {
    EMBED_MODEL
        .get_or_init(|| {
            tracing::info!(
                "Loading embedding model NomicEmbedTextV15 (~270 MB, cached after first run)"
            );
            fastembed::TextEmbedding::try_new(
                fastembed::InitOptions::new(fastembed::EmbeddingModel::NomicEmbedTextV15)
                    .with_show_download_progress(true),
            )
            .ok()
        })
        .as_ref()
}

#[cfg(feature = "datasource-introspect")]
fn embed_cache_key(query: &str) -> u64 {
    let digest = blake3::hash(query.as_bytes());
    u64::from_le_bytes(
        digest.as_bytes()[..8]
            .try_into()
            .expect("blake3 digest has at least eight bytes"),
    )
}

#[cfg(feature = "datasource-introspect")]
fn embed_cache() -> &'static Mutex<EmbedCache> {
    EMBED_CACHE.get_or_init(|| Mutex::new(EmbedCache::new(EMBED_CACHE_ENTRIES)))
}

#[cfg(feature = "datasource-introspect")]
async fn embed_query(query: &str) -> Option<[f32; EMBED_DIM]> {
    let key = embed_cache_key(query);
    let cached = match embed_cache().lock() {
        Ok(mut cache) => cache.get(key),
        Err(_) => None,
    };
    if let Some(embedding) = cached {
        return Some(embedding);
    }

    let query = query.to_owned();
    let embedding = tokio::task::spawn_blocking(move || {
        let model = get_embed_model()?;
        let embeddings = model.embed(vec![query.as_str()], None).ok()?;
        let vec = embeddings.into_iter().next()?;
        let embedding: [f32; EMBED_DIM] = vec.try_into().ok()?;
        Some(embedding)
    })
    .await
    .ok()
    .flatten()?;

    if let Ok(mut cache) = embed_cache().lock() {
        cache.insert(key, embedding);
    }

    Some(embedding)
}

#[cfg(feature = "datasource-introspect")]
#[derive(Debug, Clone)]
struct DocSearchRow {
    stable_symbol_id: String,
    title: String,
    file: String,
    score: f64,
    signal: Option<String>,
}

#[cfg(feature = "datasource-introspect")]
impl DocSearchRow {
    fn new(
        stable_symbol_id: impl Into<String>,
        title: impl Into<String>,
        file: impl Into<String>,
        score: f64,
    ) -> Self {
        Self {
            stable_symbol_id: stable_symbol_id.into(),
            title: title.into(),
            file: file.into(),
            score,
            signal: None,
        }
    }

    fn into_value(self) -> Value {
        json!({
            "kind": "doc",
            "title": self.title,
            "file": self.file,
            "score": self.score,
            "signal": self.signal,
        })
    }
}

#[cfg(feature = "datasource-introspect")]
#[derive(Debug, Clone)]
struct DocMetadata {
    title: String,
    file: String,
}

#[cfg(feature = "datasource-introspect")]
fn rrf_fuse(
    bm25_rows: &[DocSearchRow],
    ann_rows: &[(String, f64)],
    metadata: &HashMap<String, DocMetadata>,
    limit: usize,
) -> Vec<DocSearchRow> {
    let mut scores = HashMap::<String, f64>::new();
    let mut first_seen = HashMap::<String, usize>::new();
    let mut metadata = metadata.clone();

    for row in bm25_rows {
        metadata
            .entry(row.stable_symbol_id.clone())
            .or_insert_with(|| DocMetadata {
                title: row.title.clone(),
                file: row.file.clone(),
            });
    }

    for (idx, row) in bm25_rows.iter().enumerate() {
        let rank = idx + 1;
        *scores.entry(row.stable_symbol_id.clone()).or_default() +=
            1.0 / (HYBRID_RRF_K + rank as f64);
        first_seen
            .entry(row.stable_symbol_id.clone())
            .or_insert(rank);
    }

    for (idx, (stable_symbol_id, _distance)) in ann_rows.iter().enumerate() {
        let rank = idx + 1;
        *scores.entry(stable_symbol_id.clone()).or_default() += 1.0 / (HYBRID_RRF_K + rank as f64);
        first_seen
            .entry(stable_symbol_id.clone())
            .or_insert(bm25_rows.len() + rank);
    }

    let mut fused = scores
        .into_iter()
        .filter_map(|(stable_symbol_id, score)| {
            let metadata = metadata.get(&stable_symbol_id)?;
            Some(DocSearchRow {
                stable_symbol_id,
                title: metadata.title.clone(),
                file: metadata.file.clone(),
                score,
                signal: None,
            })
        })
        .collect::<Vec<_>>();

    fused.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                first_seen
                    .get(&left.stable_symbol_id)
                    .cmp(&first_seen.get(&right.stable_symbol_id))
            })
            .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
    });

    let mut per_doc = HashMap::<String, usize>::new();
    let mut deduped = Vec::with_capacity(limit.min(fused.len()));
    for row in fused {
        let count = per_doc.entry(row.file.clone()).or_default();
        if *count >= HYBRID_PER_DOC_LIMIT {
            continue;
        }
        *count += 1;
        deduped.push(row);
        if deduped.len() >= limit {
            break;
        }
    }
    deduped
}

#[cfg(feature = "datasource-introspect")]
fn search_docs_bm25_rows(
    conn: &duckdb::Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<DocSearchRow>, McpError> {
    let q = query.replace('\'', "''");
    let sql = format!(
        "SELECT stable_symbol_id, section AS title, \
         regexp_replace(file_path, '^(crates|docs|\\.claude|\\.spur|\\.codex|\\.kiro|\\.gemini)/', '') AS file, \
         round(bm25, 3) AS score \
         FROM search_docs_bm25('{q}') LIMIT {limit}"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| internal("failed to prepare BM25 docs query", &e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DocSearchRow::new(
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })
        .map_err(|e| internal("failed to run BM25 docs query", &e))?;

    rows.collect::<Result<_, _>>()
        .map_err(|e| internal("failed to read BM25 docs rows", &e))
}

#[cfg(feature = "datasource-introspect")]
fn fetch_doc_metadata(
    conn: &duckdb::Connection,
    stable_symbol_ids: &[String],
) -> Result<HashMap<String, DocMetadata>, McpError> {
    if stable_symbol_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique = Vec::<&str>::new();
    for stable_symbol_id in stable_symbol_ids {
        if !unique.contains(&stable_symbol_id.as_str()) {
            unique.push(stable_symbol_id.as_str());
        }
    }
    let ids = unique
        .into_iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT stable_symbol_id, qualified_name AS title, \
         regexp_replace(file_path, '^(crates|docs|\\.claude|\\.spur|\\.codex|\\.kiro|\\.gemini)/', '') AS file \
         FROM sections_search WHERE stable_symbol_id IN ({ids})"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| internal("failed to prepare doc metadata query", &e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                DocMetadata {
                    title: row.get::<_, String>(1)?,
                    file: row.get::<_, String>(2)?,
                },
            ))
        })
        .map_err(|e| internal("failed to run doc metadata query", &e))?;

    rows.collect::<Result<_, _>>()
        .map_err(|e| internal("failed to read doc metadata rows", &e))
}

#[cfg(feature = "datasource-introspect")]
async fn lance_ann_search(query_vec: &[f32; EMBED_DIM], limit: usize) -> Vec<(String, f64)> {
    match lance_ann_search_inner(query_vec, limit).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(%error, "Lance ANN search unavailable; returning BM25-only hybrid rows");
            Vec::new()
        }
    }
}

#[cfg(feature = "datasource-introspect")]
async fn lance_ann_search_inner(
    query_vec: &[f32; EMBED_DIM],
    limit: usize,
) -> Result<Vec<(String, f64)>, McpError> {
    let dataset_path = resolve_lance_dataset_path()?;
    if !dataset_path.is_dir() {
        return Ok(Vec::new());
    }

    let conn = cached_lance_connection(&dataset_path).await?;
    let table = conn
        .open_table(SECTIONS_TABLE)
        .execute()
        .await
        .map_err(|e| internal("failed to open Lance sections table", &e))?;
    let batches = table
        .query()
        .only_if("vector IS NOT NULL AND heading_level >= 2 AND length(body_text) <= 4096")
        .select(Select::columns(&["stable_symbol_id", "_distance"]))
        .nearest_to(query_vec.as_slice())
        .map_err(|e| internal("failed to build Lance vector query", &e))?
        .limit(limit)
        .execute()
        .await
        .map_err(|e| internal("failed to execute Lance vector query", &e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| internal("failed to read Lance vector rows", &e))?;

    let mut rows = Vec::new();
    for batch in batches {
        let ids = batch
            .column_by_name("stable_symbol_id")
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("{METHOD} Lance vector query missing stable_symbol_id column"),
                    None,
                )
            })?;
        let distances = batch
            .column_by_name("_distance")
            .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("{METHOD} Lance vector query missing _distance column"),
                    None,
                )
            })?;

        for idx in 0..batch.num_rows() {
            if ids.is_null(idx) {
                continue;
            }
            let distance = if distances.is_valid(idx) {
                distances.value(idx) as f64
            } else {
                0.0
            };
            rows.push((ids.value(idx).to_owned(), distance));
        }
    }
    Ok(rows)
}

#[cfg(feature = "datasource-introspect")]
async fn cached_lance_connection(dataset_path: &Path) -> Result<lancedb::Connection, McpError> {
    let cache = LANCE_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(conn) = cache.get(dataset_path).cloned() {
            return Ok(conn);
        }
    }

    let conn = lancedb::connect(dataset_path.to_string_lossy().as_ref())
        .execute()
        .await
        .map_err(|e| internal("failed to connect to Lance sections dataset", &e))?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(dataset_path.to_path_buf(), conn.clone());
    }
    Ok(conn)
}

#[derive(Debug, Deserialize)]
struct Params {
    query: String,
    /// "all" (docs + code), "docs", "code", "graph", or "hybrid". Defaults to "all".
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Optional explicit path to analyst.duckdb; otherwise discovered from cwd.
    #[serde(default)]
    db_path: Option<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Semantic (BM25) search over the analyst index: ranks documentation/skill/plan section bodies AND code (symbol token text) by relevance to a natural-language query. Code hits carry their scorecard signal (pagerank/churn/posture). Use this for concept/content questions ('how does oauth refresh work', 'where is the review gate documented'); use code_symbol_search for exact symbol-name lookup and code_callers/code_callees for the call graph.",
        rmcp_object(json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "scope": { "type": "string", "enum": ["all", "docs", "code", "graph", "hybrid"], "default": "all" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT },
                "db_path": { "type": "string", "description": "Optional path to analyst.duckdb; discovered from cwd upward by default." }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    _deps: &crate::mcp::ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: Params = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ query, scope?, limit?, db_path? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.query.trim().is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} query must be non-empty"),
            None,
        ));
    }
    let scope = params.scope.as_deref().unwrap_or("all");
    if !matches!(scope, "all" | "docs" | "code" | "graph" | "hybrid") {
        return Err(McpError::invalid_params(
            format!("{METHOD} scope must be one of all|docs|code|graph|hybrid"),
            Some(json!({ "scope": scope })),
        ));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    run(&params.query, scope, limit, params.db_path.as_deref()).await
}

#[cfg(feature = "datasource-introspect")]
async fn run(
    query: &str,
    scope: &str,
    limit: usize,
    db_path: Option<&str>,
) -> Result<CallToolResult, McpError> {
    let (path, results, graph_hash, ann_rows) = search_rows(query, scope, limit, db_path).await?;
    Ok(CallToolResult::structured(json!({
        "scope": scope,
        "query": query,
        "count": results.len(),
        "ann_rows": ann_rows,
        "results": results,
        "graph_content_hash": graph_hash,
        "db_path": path,
    })))
}

/// Core query: open analyst.duckdb read-only and run the chosen `search*` macro.
/// Returns (resolved_db_path, rows, graph_content_hash, ann_rows).
#[cfg(feature = "datasource-introspect")]
async fn search_rows(
    query: &str,
    scope: &str,
    limit: usize,
    db_path: Option<&str>,
) -> Result<(String, Vec<Value>, Option<String>, usize), McpError> {
    let path = resolve_db_path(db_path)?;

    // Read-only so the tool coexists with the read-only spur-analyst MCP and never
    // takes a write lock on the live DB.
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .map_err(|e| internal("failed to configure read-only duckdb", &e))?;
    let conn = duckdb::Connection::open_with_flags(&path, config).map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not open analyst.duckdb (version skew between writer and bundled reader can cause this)"),
            Some(json!({ "db_path": path.display().to_string(), "error": e.to_string() })),
        )
    })?;

    // The temporal views behind the code scorecard (v_symbol_churn_90d → posture)
    // filter on `now() - INTERVAL '90 day'` over a TIMESTAMPTZ column; that
    // `-(TIMESTAMPTZ, INTERVAL)` overload lives in icu, not core, and is not
    // autoloaded — so binding `search_code`/`search` read-only fails without it.
    // icu is statically bundled, so LOAD (no network install) is enough.
    // Best-effort: the `docs` scope never touches the temporal views, and if icu
    // were genuinely unavailable the `code`/`all` prepare below still surfaces the
    // precise binder error — so a load failure here must not break `docs`.
    let _ = conn.execute_batch("LOAD icu;");

    if scope == "hybrid" {
        let bm25_rows = search_docs_bm25_rows(&conn, query, HYBRID_CANDIDATES.max(limit))?;
        let ann_rows = match embed_query(query).await {
            Some(query_vec) => lance_ann_search(&query_vec, HYBRID_CANDIDATES.max(limit)).await,
            None => Vec::new(),
        };
        let mut metadata_ids = bm25_rows
            .iter()
            .map(|row| row.stable_symbol_id.clone())
            .collect::<Vec<_>>();
        metadata_ids.extend(
            ann_rows
                .iter()
                .map(|(stable_symbol_id, _)| stable_symbol_id.clone()),
        );
        let metadata = fetch_doc_metadata(&conn, &metadata_ids)?;
        let ann_count = ann_rows.len();
        let results = rrf_fuse(&bm25_rows, &ann_rows, &metadata, limit)
            .into_iter()
            .map(DocSearchRow::into_value)
            .collect::<Vec<_>>();

        let graph_hash: Option<String> = conn
            .query_row("SELECT graph_content_hash FROM _meta", [], |r| r.get(0))
            .ok();

        return Ok((path.display().to_string(), results, graph_hash, ann_count));
    }

    // The macro arg flows into FTS match_bm25, which wants a constant — inline the
    // query as an escaped string literal rather than a bind parameter.
    let q = query.replace('\'', "''");
    let sql = match scope {
        "docs" => format!(
            "SELECT 'doc' AS kind, section AS title, file_path AS file, \
             round(bm25, 3) AS score, CAST(NULL AS VARCHAR) AS signal \
             FROM search_docs('{q}') LIMIT {limit}"
        ),
        "code" => format!(
            "SELECT 'code' AS kind, symbol AS title, file_path AS file, \
             round(bm25, 3) AS score, posture AS signal \
             FROM search_code('{q}') LIMIT {limit}"
        ),
        "graph" => format!(
            "SELECT kind, title, file, round(score, 3) AS score, signal, \
             neighbor_kind, edge_bind_method \
             FROM search_graph('{q}') LIMIT {limit}"
        ),
        "hybrid" => unreachable!("hybrid search is handled by Lance ANN plus Rust RRF"),
        _ => format!(
            "SELECT kind, title, file, round(score, 3) AS score, signal \
             FROM search('{q}') LIMIT {limit}"
        ),
    };

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| internal("failed to prepare search query", &e))?;
    let rows = stmt
        .query_map([], |row| {
            let mut obj = json!({
                "kind": row.get::<_, String>(0)?,
                "title": row.get::<_, String>(1)?,
                "file": row.get::<_, String>(2)?,
                "score": row.get::<_, f64>(3)?,
                "signal": row.get::<_, Option<String>>(4)?,
            });
            if scope == "graph" {
                obj["neighbor_kind"] = json!(row.get::<_, Option<String>>(5)?);
                obj["edge_bind_method"] = json!(row.get::<_, Option<String>>(6)?);
            }
            Ok(obj)
        })
        .map_err(|e| internal("failed to run search query", &e))?;
    let results: Vec<Value> = rows
        .collect::<Result<_, _>>()
        .map_err(|e| internal("failed to read search rows", &e))?;

    // Surface the indexed graph hash so callers can detect staleness vs code_* tools.
    let graph_hash: Option<String> = conn
        .query_row("SELECT graph_content_hash FROM _meta", [], |r| r.get(0))
        .ok();

    Ok((path.display().to_string(), results, graph_hash, 0))
}

#[cfg(not(feature = "datasource-introspect"))]
async fn run(
    _query: &str,
    _scope: &str,
    _limit: usize,
    _db_path: Option<&str>,
) -> Result<CallToolResult, McpError> {
    Err(McpError::internal_error(
        format!(
            "{METHOD} requires the `datasource-introspect` feature (bundled duckdb) to be enabled"
        ),
        Some(json!({ "code": "duckdb_unavailable" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn resolve_db_path(explicit: Option<&str>) -> Result<std::path::PathBuf, McpError> {
    use std::path::PathBuf;
    if let Some(raw) = explicit {
        let pb = PathBuf::from(raw);
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(McpError::invalid_params(
            format!("{METHOD} db_path does not exist: {raw}"),
            Some(json!({ "db_path": raw })),
        ));
    }
    let mut dir = std::env::current_dir().map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not read current directory"),
            Some(json!({ "error": e.to_string() })),
        )
    })?;
    loop {
        let candidate = dir.join(".spur").join("analyst.duckdb");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(McpError::internal_error(
        format!("{METHOD} found no .spur/analyst.duckdb from the working directory upward; run `spur-cli graph build`"),
        Some(json!({ "code": "analyst_db_not_found" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn resolve_lance_dataset_path() -> Result<PathBuf, McpError> {
    if let Some(raw) = std::env::var_os("SPUR_GRAPH_ARTIFACT_DIR") {
        if !raw.is_empty() {
            return Ok(PathBuf::from(raw).join(SECTIONS_DATASET_DIR));
        }
    }

    let mut dir = std::env::current_dir().map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not read current directory"),
            Some(json!({ "error": e.to_string() })),
        )
    })?;
    loop {
        let pointer = dir.join(".spur").join("graph").join("CURRENT");
        if std::fs::symlink_metadata(&pointer).is_ok() {
            let artifact_dir = resolve_graph_current_pointer(&pointer)?;
            return Ok(artifact_dir.join(SECTIONS_DATASET_DIR));
        }
        if !dir.pop() {
            break;
        }
    }
    Err(McpError::internal_error(
        format!("{METHOD} found no .spur/graph/CURRENT from the working directory upward; run `spur-cli graph build`"),
        Some(json!({ "code": "graph_artifact_not_found" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn resolve_graph_current_pointer(pointer: &Path) -> Result<PathBuf, McpError> {
    let metadata = std::fs::symlink_metadata(pointer)
        .map_err(|e| internal("failed to inspect .spur/graph/CURRENT", &e))?;
    if metadata.file_type().is_symlink() || metadata.is_dir() {
        return std::fs::canonicalize(pointer)
            .map_err(|e| internal("failed to resolve .spur/graph/CURRENT", &e));
    }

    let raw = std::fs::read_to_string(pointer)
        .map_err(|e| internal("failed to read .spur/graph/CURRENT", &e))?;
    let raw = raw.trim();
    let target = PathBuf::from(raw);
    let target = if target.is_absolute() {
        target
    } else {
        pointer
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    };
    Ok(std::fs::canonicalize(&target).unwrap_or(target))
}

#[cfg(feature = "datasource-introspect")]
fn internal(message: &str, error: &impl std::fmt::Display) -> McpError {
    McpError::internal_error(
        format!("{METHOD} {message}"),
        Some(json!({ "error": error.to_string() })),
    )
}

#[cfg(all(test, feature = "datasource-introspect"))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::{
        bridge::{AgentBridge, TauriBridgeRequester},
        ServerDeps,
    };

    fn deps_without_app() -> ServerDeps {
        ServerDeps::from_bridge(Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))))
    }

    fn embedding_with_marker(marker: f32) -> [f32; 768] {
        let mut embedding = [0.0; 768];
        embedding[0] = marker;
        embedding
    }

    #[test]
    fn embed_cache_get_refreshes_lru_order() {
        let mut cache = EmbedCache::new(2);
        let first = embedding_with_marker(1.0);
        let second = embedding_with_marker(2.0);
        let third = embedding_with_marker(3.0);

        cache.insert(1, first);
        cache.insert(2, second);
        assert_eq!(cache.get(1), Some(first));

        cache.insert(3, third);

        assert_eq!(cache.get(1), Some(first));
        assert_eq!(cache.get(2), None);
        assert_eq!(cache.get(3), Some(third));
    }

    #[test]
    fn embed_cache_insert_refreshes_lru_order() {
        let mut cache = EmbedCache::new(2);
        let updated_first = embedding_with_marker(10.0);
        let second = embedding_with_marker(2.0);
        let third = embedding_with_marker(3.0);

        cache.insert(1, embedding_with_marker(1.0));
        cache.insert(2, second);
        cache.insert(1, updated_first);
        cache.insert(3, third);

        assert_eq!(cache.get(1), Some(updated_first));
        assert_eq!(cache.get(2), None);
        assert_eq!(cache.get(3), Some(third));
    }

    #[test]
    fn embed_cache_key_uses_first_eight_blake3_bytes() {
        let digest = blake3::hash(b"architecture");
        let expected = u64::from_le_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("blake3 digest has at least eight bytes"),
        );

        assert_eq!(embed_cache_key("architecture"), expected);
    }

    #[test]
    fn rrf_fuse_merges_bm25_and_ann_with_per_document_cap() {
        let bm25 = vec![
            DocSearchRow::new("bm25-only", "BM25 only", "docs/a.md", 3.0),
            DocSearchRow::new("shared", "Shared", "docs/a.md", 2.0),
            DocSearchRow::new("a-third", "A third", "docs/a.md", 1.0),
            DocSearchRow::new("a-fourth", "A fourth", "docs/a.md", 0.5),
            DocSearchRow::new("other", "Other", "docs/b.md", 0.25),
        ];
        let metadata = bm25
            .iter()
            .map(|row| {
                (
                    row.stable_symbol_id.clone(),
                    DocMetadata {
                        title: row.title.clone(),
                        file: row.file.clone(),
                    },
                )
            })
            .chain([(
                "ann-only".to_owned(),
                DocMetadata {
                    title: "ANN only".to_owned(),
                    file: "docs/c.md".to_owned(),
                },
            )])
            .collect::<HashMap<_, _>>();
        let ann = vec![
            ("ann-only".to_owned(), 0.01),
            ("shared".to_owned(), 0.02),
            ("a-fourth".to_owned(), 0.03),
        ];

        let fused = rrf_fuse(&bm25, &ann, &metadata, 10);

        assert_eq!(fused[0].stable_symbol_id, "shared");
        assert!(fused.iter().any(|row| row.stable_symbol_id == "ann-only"));
        assert_eq!(
            fused.iter().filter(|row| row.file == "docs/a.md").count(),
            3,
            "one file must not contribute more than three fused rows"
        );
        assert!(
            fused.iter().all(|row| (0.0..1.0).contains(&row.score)),
            "RRF scores should be normalized reciprocal-rank contributions"
        );
    }

    #[tokio::test]
    async fn search_hybrid_scope_accepted_by_validation() {
        let deps = deps_without_app();
        let error = call(
            &deps,
            json!({
                "query": "architecture",
                "scope": "hybrid",
                "db_path": "/definitely/missing/analyst.duckdb"
            }),
        )
        .await
        .expect_err("missing db should fail after scope validation");

        assert!(
            !error.to_string().contains("scope must be one of"),
            "hybrid scope must not be rejected by validation: {error}"
        );
    }

    #[test]
    fn rrf_fuse_returns_bm25_rows_when_ann_is_unavailable() {
        let bm25 = vec![DocSearchRow::new(
            "s1",
            "design/overview",
            "docs/design.md",
            3.0,
        )];
        let metadata = bm25
            .iter()
            .map(|row| {
                (
                    row.stable_symbol_id.clone(),
                    DocMetadata {
                        title: row.title.clone(),
                        file: row.file.clone(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        let fused = rrf_fuse(&bm25, &[], &metadata, 10);

        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].stable_symbol_id, "s1");
        assert_eq!(fused[0].score, 1.0 / 61.0);
    }

    // Rerank: a high-BM25 leaf CONSTANT must not outrank a lower-BM25, high-pagerank
    // FUNCTION. Mirrors the production search_code ORDER BY against a bundled-duckdb fixture
    // (the WORKTREE_CORE_ORPHAN_CLEANUP-over-cleanup_orphans inversion from the live eval).
    #[test]
    fn search_code_rerank_floats_impl_over_leaf_constant() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("a.duckdb");
        let conn = duckdb::Connection::open(&db)?;
        conn.execute_batch("INSTALL fts; LOAD fts;")?;
        conn.execute_batch(
            r#"
            CREATE TABLE symbol_text(stable_symbol_id VARCHAR, entity_name VARCHAR,
                symbol_kind VARCHAR, file_path VARCHAR, doc_text VARCHAR);
            INSERT INTO symbol_text VALUES
              ('c1','WORKTREE_CORE_ORPHAN_CLEANUP','constant',
               'crates/spur-license/src/policy/feature_key.rs',
               'worktree orphan cleanup worktree orphan cleanup'),
              ('f1','cleanup_orphans','function',
               'crates/spur-worktree/src/manager.rs',
               'worktree orphan cleanup sweep');
            -- scorecard: the function is a load-bearing wall (high pagerank), constant is a leaf.
            CREATE TABLE v_symbol_scorecard(stable_symbol_id VARCHAR, pagerank DOUBLE,
                churn_90d BIGINT, posture VARCHAR, component_size BIGINT);
            INSERT INTO v_symbol_scorecard VALUES
              ('c1', 0.0, 0, 'leaf', 1),
              ('f1', 0.02, 3, 'load-bearing wall', 50);
            "#,
        )?;
        conn.execute_batch(
            "PRAGMA create_fts_index('symbol_text','stable_symbol_id','doc_text', overwrite=1);",
        )?;
        // The PRODUCTION search_code ORDER BY (kept in sync with init_search.sql).
        conn.execute_batch(
            r#"
            CREATE OR REPLACE MACRO search_code(q) AS TABLE
              SELECT * FROM (
                SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
                       fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
                       sc.pagerank
                FROM symbol_text st
                JOIN v_symbol_scorecard sc USING (stable_symbol_id)
                WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
              )
              ORDER BY bm25_raw
                * CASE WHEN file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
                * CASE WHEN symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                       WHEN symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
                * (1 + 0.15 * ln(1 + pagerank * 1e4)) DESC NULLS LAST
              LIMIT 25;
            "#,
        )?;
        let first: String = conn.query_row(
            "SELECT symbol FROM search_code('worktree orphan cleanup') LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            first, "cleanup_orphans",
            "the load-bearing function must outrank the leaf constant after rerank"
        );
        Ok(())
    }

    // Diversity + dedup: vendored copies collapse to one, and one document cannot occupy
    // more than 2 of the top result slots.
    #[test]
    fn search_dedups_vendored_copies_and_caps_per_document() -> anyhow::Result<()> {
        const INIT_SEARCH_SQL: &str =
            include_str!("../../../../spur-context/poc/duckdb-analyst/init_search.sql");

        let dir = tempfile::tempdir()?;
        let db = dir.path().join("a.duckdb");
        let conn = duckdb::Connection::open(&db)?;
        conn.execute_batch("INSTALL fts; LOAD fts;")?;
        // sections raw: same skill body vendored 3x (differ only by SPUR-MANAGED header +
        // dot-dir path) plus one 3-section plan document.
        conn.execute_batch(
            r#"
            CREATE SCHEMA lance_ns;
            CREATE TABLE lance_ns.section_bodies(
                stable_symbol_id VARCHAR,
                parent_stable_id VARCHAR,
                qualified_name VARCHAR,
                file_path VARCHAR,
                heading_level BIGINT,
                child_count BIGINT,
                content_hash VARCHAR,
                body_byte_start BIGINT,
                body_text VARCHAR,
                vector FLOAT[768]
            );
            INSERT INTO lance_ns.section_bodies VALUES
              ('a',NULL,'Brain Review Gate','.claude/skills/brain-review-gate/SKILL.md',
               1,0,'ha',0,
               '<!-- SPUR-MANAGED v=1 sha256=aaa -->\napprove or reject worker output gate',
               NULL),
              ('b',NULL,'Brain Review Gate','.codex/skills/brain-review-gate/SKILL.md',
               1,0,'hb',0,
               '<!-- SPUR-MANAGED v=1 sha256=bbb -->\napprove or reject worker output gate',
               NULL),
              ('c',NULL,'Brain Review Gate','crates/spur-core/src/skills/brain-review-gate/SKILL.md',
               1,0,'hc',0,
               '<!-- SPUR-MANAGED v=1 sha256=ccc -->\napprove or reject worker output gate',
               NULL),
              ('p1',NULL,'Plan::S1','docs/superpowers/plans/p.md',
               2,0,'hp',0,'worker output review section one', NULL),
              ('p2',NULL,'Plan::S2','docs/superpowers/plans/p.md',
               2,0,'hp',42,'worker output review section two', NULL),
              ('p3',NULL,'Plan::S3','docs/superpowers/plans/p.md',
               2,0,'hp',84,'worker output review section three', NULL);

            CREATE TABLE nodes(
                stable_symbol_id VARCHAR,
                entity_name VARCHAR,
                qualified_name VARCHAR,
                file_path VARCHAR,
                symbol_kind VARCHAR
            );
            CREATE TABLE symbol_snapshots(stable_symbol_id VARCHAR, tokens VARCHAR[]);
            CREATE TABLE v_symbol_scorecard(
                stable_symbol_id VARCHAR,
                pagerank DOUBLE,
                churn_90d BIGINT,
                posture VARCHAR,
                component_size BIGINT
            );
            CREATE VIEW v_symbol_inbound AS
                SELECT stable_symbol_id, 0 AS callers FROM nodes;
            CREATE TABLE edges(
                source_stable_id VARCHAR,
                target_stable_id VARCHAR,
                relation VARCHAR,
                bind_method VARCHAR
            );
            "#,
        )?;
        let sections_fts =
            "PRAGMA create_fts_index('sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');";
        let symbol_fts =
            "PRAGMA create_fts_index('symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);";
        let (before_sections_fts, after_sections_fts) = INIT_SEARCH_SQL
            .split_once(sections_fts)
            .expect("init_search.sql should create the sections_search FTS index");
        let (before_symbol_fts, after_symbol_fts) = after_sections_fts
            .split_once(symbol_fts)
            .expect("init_search.sql should create the symbol_text FTS index");

        // create_fts_index starts its own internal transaction in bundled DuckDB,
        // so table creation must be committed before each PRAGMA runs.
        conn.execute_batch(before_sections_fts)?;
        conn.execute_batch(sections_fts)?;
        conn.execute_batch(before_symbol_fts)?;
        conn.execute_batch(symbol_fts)?;
        conn.execute_batch(after_symbol_fts)?;

        // 3 vendored copies must collapse to exactly 1.
        let copies: i64 = conn.query_row(
            "SELECT count(*) FROM sections_search WHERE qualified_name='Brain Review Gate'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            copies, 1,
            "vendored skill copies must dedup to one canonical row"
        );

        // Per-document cap: the 3-section plan must contribute at most 2 rows.
        let docs_plan_rows: i64 = conn.query_row(
            "SELECT count(*) FROM search_docs('worker output review') \
             WHERE file_path='docs/superpowers/plans/p.md'",
            [],
            |r| r.get(0),
        )?;
        assert!(
            docs_plan_rows <= 2,
            "one document must not exceed 2 search_docs rows"
        );

        let unified_plan_rows: i64 = conn.query_row(
            "SELECT count(*) FROM search('worker output review') \
             WHERE kind='doc' AND file='superpowers/plans/p.md'",
            [],
            |r| r.get(0),
        )?;
        assert!(
            unified_plan_rows <= 2,
            "one document must not exceed 2 unified search rows"
        );
        Ok(())
    }

    // Builds a tiny analyst-shaped DB with the BUNDLED duckdb (same lib the tool
    // reads with), creates the FTS index + search macro, and asserts the tool's
    // query path returns ranked rows. Proves fts + create_fts_index + the search
    // macro all work with the linked libduckdb — independent of the CLI version.
    #[tokio::test]
    async fn search_docs_over_bundled_fixture_returns_ranked_rows() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("analyst.duckdb");
        {
            let conn = duckdb::Connection::open(&db)?;
            conn.execute_batch("INSTALL fts; LOAD fts;")?;
            // Separate statements: create_fts_index runs its own internal
            // transaction and must see `sections` already committed, so the
            // table population can't share a batch with the index build.
            conn.execute_batch(
                r#"
                CREATE TABLE sections(stable_symbol_id VARCHAR, section VARCHAR,
                                      file_path VARCHAR, body_text VARCHAR);
                INSERT INTO sections VALUES
                  ('s1','OAuth refresh','docs/oauth.md','how the oauth token refresh grant works'),
                  ('s2','Unrelated','docs/misc.md','notebook cells and kernels');
                CREATE TABLE _meta(graph_content_hash VARCHAR);
                INSERT INTO _meta VALUES ('deadbeef');
                "#,
            )?;
            conn.execute_batch(
                "PRAGMA create_fts_index('sections','stable_symbol_id','body_text', overwrite=1);",
            )?;
            conn.execute_batch(
                r#"
                CREATE OR REPLACE MACRO search_docs(q) AS TABLE
                  SELECT s.section AS section, s.file_path AS file_path,
                         fts_main_sections.match_bm25(s.stable_symbol_id, q) AS bm25
                  FROM sections s
                  WHERE fts_main_sections.match_bm25(s.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25 DESC;
                "#,
            )?;
            // Flush WAL → main file so the read-only reopen (which cannot replay
            // a WAL) sees the tables. The real analyst.duckdb is checkpointed by
            // the CLI on exit, so this only matters in-test.
            conn.execute_batch("CHECKPOINT;")?;
        }

        let (resolved, results, graph_hash, ann_rows) =
            search_rows("oauth token refresh", "docs", 10, db.to_str())
                .await
                .expect("semantic search over fixture");
        assert_eq!(resolved, db.display().to_string());
        assert_eq!(graph_hash.as_deref(), Some("deadbeef"));
        assert_eq!(ann_rows, 0);
        assert!(!results.is_empty(), "expected at least one ranked hit");
        assert_eq!(results[0]["title"], "OAuth refresh");
        // run() wraps the same rows into a CallToolResult without error.
        let result = run("oauth token refresh", "docs", 10, db.to_str())
            .await
            .expect("run wraps result");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["ann_rows"],
            json!(0)
        );
        Ok(())
    }

    #[tokio::test]
    async fn search_graph_scope_returns_neighbor_kind_rows() {
        // Build a minimal fixture with FTS + scorecard + edges so search_graph can run.
        // Uses an in-memory DB with the same macro structure as the real analyst.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let conn = duckdb::Connection::open(&db_path).unwrap();

        conn.execute_batch(
            "
            INSTALL fts; LOAD fts; LOAD icu;

            -- Minimal nodes + symbol_text + scorecard + inbound + edges
            CREATE TABLE nodes (stable_symbol_id VARCHAR, node_id BIGINT,
              entity_name VARCHAR, qualified_name VARCHAR,
              file_path VARCHAR, symbol_kind VARCHAR,
              line_start INT, line_end INT);
            CREATE TABLE symbol_text (stable_symbol_id VARCHAR, entity_name VARCHAR,
              qualified_name VARCHAR, file_path VARCHAR, symbol_kind VARCHAR,
              doc_text VARCHAR);

            INSERT INTO nodes VALUES
              ('aa01', 1, 'handle_query', 'handle_query',
               'crates/spur-mcp/src/server.rs', 'function', 1, 20),
              ('aa02', 2, 'run_bm25_search', 'run_bm25_search',
               'crates/spur-mcp/src/search.rs', 'function', 1, 10);
            INSERT INTO symbol_text VALUES
              ('aa01', 'handle_query', 'handle_query',
               'crates/spur-mcp/src/server.rs', 'function',
               'handle_query search bm25 graph query'),
              ('aa02', 'run_bm25_search', 'run_bm25_search',
               'crates/spur-mcp/src/search.rs', 'function',
               'run_bm25_search search fts fulltext');
        ",
        )
        .unwrap();

        conn.execute_batch(
            "PRAGMA create_fts_index('symbol_text','stable_symbol_id','doc_text',overwrite=1);",
        )
        .unwrap();

        conn.execute_batch(
            "
            -- Minimal scorecard view
            CREATE VIEW v_symbol_scorecard AS
            SELECT stable_symbol_id,
                   0.001 AS pagerank, 0 AS churn_90d,
                   'leaf' AS posture, 1 AS component_size
            FROM nodes;

            -- Minimal inbound view (0 callers = safe to expand)
            CREATE VIEW v_symbol_inbound AS
            SELECT stable_symbol_id, 0 AS callers, 0 AS importers,
                   0 AS containers, 0 AS inbound_total
            FROM nodes;

            -- One call edge: aa01 calls aa02
            CREATE TABLE edges (source_stable_id VARCHAR, target_stable_id VARCHAR,
              relation VARCHAR, bind_method VARCHAR, edge_kind VARCHAR,
              src_id BIGINT, dst_id BIGINT, target_label VARCHAR,
              confidence VARCHAR, confidence_score DOUBLE);
            INSERT INTO edges VALUES
              ('aa01','aa02','calls','singleton','calls',1,2,'run_bm25_search','high',0.9);

            -- FTS macro (simplified version of real search_graph)
            CREATE OR REPLACE MACRO search_graph(q) AS TABLE
              SELECT kind, title, file, score, signal, neighbor_kind, edge_bind_method
              FROM (
                WITH base AS (
                  SELECT st.stable_symbol_id, st.entity_name AS symbol, st.symbol_kind,
                         st.file_path,
                         fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
                         sc.pagerank, sc.churn_90d, sc.posture,
                         COALESCE(vi.callers, 0) AS caller_count
                  FROM symbol_text st
                  JOIN v_symbol_scorecard sc USING (stable_symbol_id)
                  LEFT JOIN v_symbol_inbound vi USING (stable_symbol_id)
                  WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25_raw DESC LIMIT 5
                ),
                gated AS (
                  SELECT * FROM base
                  WHERE posture != 'load-bearing wall' OR caller_count <= 30
                ),
                primary_rows AS (
                  SELECT 'code' AS kind, symbol AS title,
                         regexp_replace(file_path,'^crates/','') AS file,
                         round(bm25_raw, 3) AS score,
                         posture AS signal, 'primary' AS neighbor_kind,
                         CAST(NULL AS VARCHAR) AS edge_bind_method
                  FROM base
                ),
                callee_rows AS (
                  SELECT 'code', ndst.entity_name,
                         regexp_replace(ndst.file_path,'^crates/',''),
                         0.0, 'leaf · callee of ' || g.symbol,
                         'callee', e.bind_method
                  FROM gated g
                  JOIN edges e ON e.source_stable_id = g.stable_symbol_id
                    AND e.relation = 'calls'
                  JOIN nodes ndst ON ndst.stable_symbol_id = e.target_stable_id
                  WHERE ndst.file_path NOT LIKE '.%'
                )
                SELECT * FROM primary_rows
                UNION ALL SELECT * FROM callee_rows
              )
              ORDER BY CASE neighbor_kind WHEN 'primary' THEN 0 ELSE 1 END, score DESC
              LIMIT 20;

            CREATE TABLE _meta (graph_content_hash VARCHAR);
            INSERT INTO _meta VALUES ('test-hash');
        ",
        )
        .unwrap();
        drop(conn);

        let (_, rows, _, ann_rows) = search_rows(
            "search bm25 graph",
            "graph",
            20,
            Some(db_path.to_str().unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(ann_rows, 0);

        // Must have at least one primary and one callee row
        let has_primary = rows
            .iter()
            .any(|r| r["neighbor_kind"].as_str() == Some("primary"));
        let has_callee = rows
            .iter()
            .any(|r| r["neighbor_kind"].as_str() == Some("callee"));

        assert!(has_primary, "graph scope must return primary hits");
        assert!(has_callee, "graph scope must return callee neighbors");

        // All rows must have edge_bind_method field (may be null for primary)
        for row in &rows {
            assert!(
                row.get("edge_bind_method").is_some(),
                "every graph row must carry edge_bind_method key"
            );
        }
    }

    // Regression: the `code` scope reads `search_code`, which fuses the
    // `v_symbol_scorecard.posture` signal, which depends on v_symbol_churn_90d's
    // 90-day window — `now() - INTERVAL '90 day'` over a TIMESTAMP WITH TIME ZONE
    // column. That `-(TIMESTAMPTZ, INTERVAL)` overload lives in icu, which the
    // read path does not autoload, so without a `LOAD icu` the query fails to
    // prepare ("No function matches ... -(TIMESTAMP WITH TIME ZONE, INTERVAL)").
    // This fixture mirrors the production view chain with the real timestamptz
    // boundary and proves the `code` scope binds + returns the posture signal,
    // under the bundled libduckdb (fts + the statically-linked icu).
    #[tokio::test]
    async fn search_code_scope_binds_temporal_views_via_icu() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("analyst.duckdb");
        {
            // The build connection mirrors the real build path (the `duckdb` CLI),
            // which has icu — needed to CREATE the aggregating timestamptz view.
            // The read path's icu load is exercised separately by search_rows below.
            let conn = duckdb::Connection::open(&db)?;
            conn.execute_batch("INSTALL fts; LOAD fts; LOAD icu;")?;
            conn.execute_batch(
                r#"
                CREATE TABLE commits(sha VARCHAR, author_ts TIMESTAMP WITH TIME ZONE);
                INSERT INTO commits VALUES ('rec', now()), ('old', to_timestamp(0));
                CREATE TABLE temporal_edges(
                  target_stable_symbol_id VARCHAR, source_commit VARCHAR, change_kind VARCHAR);
                INSERT INTO temporal_edges VALUES
                  ('s1','rec','modified'), ('s1','rec','modified');
                CREATE TABLE symbol_text(
                  stable_symbol_id VARCHAR, entity_name VARCHAR, symbol_kind VARCHAR,
                  file_path VARCHAR, tokens VARCHAR);
                INSERT INTO symbol_text VALUES
                  ('s1','compress_response','function','crates/x.rs',
                   'code graph mcp response metadata compression payload'),
                  ('s2','unrelated','function','crates/y.rs','kernel notebook cells');
                CREATE TABLE _meta(graph_content_hash VARCHAR);
                INSERT INTO _meta VALUES ('cafebabe');
                "#,
            )?;
            // Production-shaped temporal chain — the churn boundary uses the exact
            // TIMESTAMPTZ `now() - INTERVAL` form from init_views.sql.
            conn.execute_batch(
                r#"
                CREATE VIEW v_symbol_churn_90d AS
                  SELECT t.target_stable_symbol_id AS stable_symbol_id, count(*) AS events
                  FROM temporal_edges t JOIN commits c ON c.sha = t.source_commit
                  WHERE c.author_ts > (now() - INTERVAL '90 day')
                  GROUP BY t.target_stable_symbol_id;
                CREATE VIEW v_symbol_scorecard AS
                  SELECT st.stable_symbol_id,
                         CASE
                           WHEN COALESCE(ch.events, 0) = 0 THEN 'load-bearing wall'
                           WHEN ch.events >= 10 THEN 'hot-central'
                           ELSE 'active'
                         END AS posture
                  FROM symbol_text st
                  LEFT JOIN v_symbol_churn_90d ch USING (stable_symbol_id);
                "#,
            )?;
            conn.execute_batch(
                "PRAGMA create_fts_index('symbol_text','stable_symbol_id','tokens', overwrite=1);",
            )?;
            conn.execute_batch(
                r#"
                CREATE OR REPLACE MACRO search_code(q) AS TABLE
                  SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
                         round(fts_main_symbol_text.match_bm25(st.stable_symbol_id, q), 3) AS bm25,
                         sc.posture
                  FROM symbol_text st
                  JOIN v_symbol_scorecard sc USING (stable_symbol_id)
                  WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25 DESC;
                "#,
            )?;
            conn.execute_batch("CHECKPOINT;")?;
        }

        // search_rows reopens read-only and must LOAD icu so the TIMESTAMPTZ churn
        // boundary binds. Without that load this errors with the production
        // `-(TIMESTAMP WITH TIME ZONE, INTERVAL)` binder error.
        let (_path, results, graph_hash, ann_rows) =
            search_rows("response metadata compression", "code", 10, db.to_str())
                .await
                .expect("code-scope search must bind temporal views via icu");
        assert_eq!(graph_hash.as_deref(), Some("cafebabe"));
        assert_eq!(ann_rows, 0);
        assert!(!results.is_empty(), "expected at least one ranked code hit");
        assert_eq!(results[0]["title"], "compress_response");
        // posture flowed through as the `signal` column (active: churn==2, <10).
        assert_eq!(results[0]["signal"], "active");
        Ok(())
    }
}
