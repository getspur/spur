use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    Array as _, Float32Array, LargeStringArray, RecordBatch, StringArray, UInt32Array, UInt64Array,
    UInt8Array,
};
use futures::TryStreamExt as _;
use globset::Glob;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery as _, QueryBase as _};
use serde_json::{json, Value};
use spur_graph::store::lance_sections::{SECTIONS_DATASET_DIR, SECTIONS_TABLE};
use spur_graph::temporal::{resolve_symbol_at_indexed, symbol_history, Resolution, TemporalIndex};
use spur_graph::{
    load_artifact, resolve_artifact_location, resolve_worktree_root_from, CommitIndexArtifact,
    GraphIndexArtifact, CODE_SYMBOL_URI_PREFIX,
};

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

const DEFAULT_K: usize = 20;
const MAX_K: usize = 100;
const LEDE_CHARS: usize = 200;

impl McpCallbackServer {
    pub(crate) async fn handle_doc_navigate(&self, id: Value, args: Value) -> JsonRpcResponse {
        match doc_navigate(&args).await {
            Ok(result) => {
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(McpHandlerError::InvalidParams(error)) => {
                JsonRpcResponse::invalid_params(id, error)
            }
            Err(McpHandlerError::NotFound(error)) => JsonRpcResponse::error(id, -32004, error),
            Err(error) => JsonRpcResponse::internal_error(id, error.to_string()),
        }
    }
}

pub(crate) async fn doc_navigate(args: &Value) -> Result<Value, McpHandlerError> {
    let request = DocNavigateRequest::parse(args)?;
    let worktree = current_worktree()?;
    let resolved = resolve_artifact_location(&worktree, None)
        .map_err(|_| graph_artifact_missing(&worktree))?;
    let table = open_sections_table(&resolved.path).await?;

    let mut hits = if let Some(root) = request.root.as_deref() {
        let root =
            resolve_root_for_as_of(&resolved.path, &worktree, root, request.as_of.as_deref())?;
        child_hits(&table, &root).await?
    } else {
        fts_hits(&table, &request).await?
    };

    if let Some(glob) = &request.file_glob {
        hits.retain(|hit| glob.is_match(&hit.file_path));
    }
    if hits.len() > request.k {
        hits.truncate(request.k);
    }

    Ok(json!({
        "hits": hits
            .into_iter()
            .map(|hit| hit.into_value(request.include_lede))
            .collect::<Vec<_>>()
    }))
}

struct DocNavigateRequest {
    query: Option<String>,
    root: Option<String>,
    k: usize,
    file_glob: Option<globset::GlobMatcher>,
    as_of: Option<String>,
    include_lede: bool,
}

impl DocNavigateRequest {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let root = optional_string(args, "root")?.map(strip_symbol_uri);
        let query = optional_string(args, "query")?;
        if root.is_none() && query.as_deref().is_none_or(str::is_empty) {
            return Err(McpHandlerError::InvalidParams(
                "field 'query' is required when 'root' is not set".to_string(),
            ));
        }

        let requested_k = args
            .get("k")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_K as u64);
        let k = requested_k.clamp(1, MAX_K as u64) as usize;
        let file_glob = optional_string(args, "file_glob")?
            .map(|pattern| {
                Glob::new(&pattern)
                    .map(|glob| glob.compile_matcher())
                    .map_err(|error| {
                        McpHandlerError::InvalidParams(format!(
                            "invalid file_glob `{pattern}`: {error}"
                        ))
                    })
            })
            .transpose()?;
        let as_of = optional_string(args, "as_of")?;
        if as_of.as_deref().is_some_and(str::is_empty) {
            return Err(McpHandlerError::InvalidParams(
                "field 'as_of' must not be empty".to_string(),
            ));
        }
        let include_lede = args
            .get("include_lede")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        Ok(Self {
            query,
            root,
            k,
            file_glob,
            as_of,
            include_lede,
        })
    }
}

#[derive(Debug)]
struct DocHit {
    stable_symbol_id: String,
    qualified_name: String,
    file_path: String,
    heading_level: u8,
    child_count: u32,
    score: Option<f32>,
    lede: Option<String>,
    body_byte_start: u64,
}

impl DocHit {
    fn into_value(self, include_lede: bool) -> Value {
        let mut value = json!({
            "stable_symbol_id": self.stable_symbol_id,
            "qualified_name": self.qualified_name,
            "file_path": self.file_path,
            "heading_level": self.heading_level,
            "child_count": self.child_count,
        });
        if let Some(score) = self.score {
            value["score"] = json!(score);
        }
        if include_lede {
            if let Some(lede) = self.lede {
                value["lede"] = json!(lede);
            }
        }
        value
    }
}

async fn open_sections_table(artifact_dir: &Path) -> Result<lancedb::Table, McpHandlerError> {
    let dataset_dir = artifact_dir.join(SECTIONS_DATASET_DIR);
    let db = lancedb::connect(dataset_dir.to_string_lossy().as_ref())
        .execute()
        .await
        .map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to connect to sections.lancedb at `{}`: {error}",
                dataset_dir.display()
            ))
        })?;
    db.open_table(SECTIONS_TABLE)
        .execute()
        .await
        .map_err(|error| {
            McpHandlerError::NotFound(format!(
                "failed to open Lance sections table `{SECTIONS_TABLE}`: {error}"
            ))
        })
}

async fn fts_hits(
    table: &lancedb::Table,
    request: &DocNavigateRequest,
) -> Result<Vec<DocHit>, McpHandlerError> {
    let query = request.query.as_deref().unwrap_or_default().to_string();
    let fts = FullTextSearchQuery::new(query)
        .with_column("body_text".to_string())
        .map_err(|error| McpHandlerError::InvalidParams(format!("invalid FTS query: {error}")))?;
    let batches = table
        .query()
        .full_text_search(fts)
        .limit(request.k)
        .execute()
        .await
        .map_err(|error| McpHandlerError::Internal(format!("doc_navigate FTS failed: {error}")))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|error| {
            McpHandlerError::Internal(format!("failed to collect doc_navigate FTS rows: {error}"))
        })?;
    let mut hits = Vec::new();
    for batch in &batches {
        hits.extend(project_batch(batch, true)?);
    }
    Ok(hits)
}

async fn child_hits(table: &lancedb::Table, root: &str) -> Result<Vec<DocHit>, McpHandlerError> {
    let filter = format!("parent_stable_id = '{}'", sql_string_literal(root));
    let batches = table
        .query()
        .only_if(filter)
        .execute()
        .await
        .map_err(|error| {
            McpHandlerError::Internal(format!("doc_navigate root expansion failed: {error}"))
        })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to collect doc_navigate child rows: {error}"
            ))
        })?;
    let mut hits = Vec::new();
    for batch in &batches {
        hits.extend(project_batch(batch, false)?);
    }
    hits.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then(left.body_byte_start.cmp(&right.body_byte_start))
            .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
    });
    Ok(hits)
}

fn project_batch(batch: &RecordBatch, include_score: bool) -> Result<Vec<DocHit>, McpHandlerError> {
    let stable_symbol_id = string_column(batch, "stable_symbol_id")?;
    let qualified_name = string_column(batch, "qualified_name")?;
    let file_path = string_column(batch, "file_path")?;
    let heading_level = u8_column(batch, "heading_level")?;
    let body_text = large_string_column(batch, "body_text")?;
    let body_byte_start = u64_column(batch, "body_byte_start")?;
    let child_count = u32_column(batch, "child_count")?;
    let score = if include_score {
        batch
            .column_by_name("_score")
            .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
    } else {
        None
    };

    let mut hits = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        hits.push(DocHit {
            stable_symbol_id: stable_symbol_id.value(row).to_string(),
            qualified_name: qualified_name.value(row).to_string(),
            file_path: file_path.value(row).to_string(),
            heading_level: heading_level.value(row),
            child_count: child_count.value(row),
            score: score.and_then(|scores| (!scores.is_null(row)).then(|| scores.value(row))),
            lede: Some(lede(body_text.value(row))),
            body_byte_start: body_byte_start.value(row),
        });
    }
    Ok(hits)
}

fn resolve_root_for_as_of(
    artifact_dir: &Path,
    worktree: &Path,
    root: &str,
    as_of: Option<&str>,
) -> Result<String, McpHandlerError> {
    let Some(as_of) = as_of else {
        return Ok(root.to_string());
    };
    let artifact = Arc::new(load_artifact(artifact_dir).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load graph artifact `{}`: {error}",
            artifact_dir.display()
        ))
    })?);
    let commits = load_commit_index(worktree)?;
    resolve_symbol_as_of(&artifact, &commits, root, as_of)
}

fn resolve_symbol_as_of(
    artifact: &Arc<GraphIndexArtifact>,
    commits: &CommitIndexArtifact,
    symbol_id: &str,
    as_of: &str,
) -> Result<String, McpHandlerError> {
    if !commits.commits.iter().any(|commit| commit.sha == as_of) {
        return Err(McpHandlerError::InvalidParams(format!(
            "as_of commit `{as_of}` is not indexed"
        )));
    }

    let temporal_index = TemporalIndex::new(Arc::clone(artifact));
    let history = symbol_history(&temporal_index, commits, symbol_id);
    if history.is_empty() {
        return Err(McpHandlerError::NotFound(format!(
            "symbol {symbol_id} has no temporal history in graph artifact"
        )));
    }

    for (_, _, key) in history {
        match resolve_symbol_at_indexed(
            &temporal_index,
            commits,
            &key.stable_symbol_id,
            &key.commit,
            as_of,
        ) {
            Resolution::Found { value, .. } => return Ok(value),
            Resolution::Deleted { last_seen } => {
                return Err(McpHandlerError::NotFound(format!(
                    "symbol {symbol_id} was deleted before `{as_of}`; last seen at {}",
                    last_seen.commit
                )));
            }
            Resolution::Ambiguous { candidates } => {
                let candidates = candidates.join(", ");
                return Err(McpHandlerError::InvalidParams(format!(
                    "symbol {symbol_id} is ambiguous at `{as_of}`; candidates: {candidates}"
                )));
            }
            Resolution::Unknown { .. } => {}
        }
    }

    Err(McpHandlerError::NotFound(format!(
        "symbol {symbol_id} not present at commit `{as_of}`"
    )))
}

fn load_commit_index(worktree: &Path) -> Result<CommitIndexArtifact, McpHandlerError> {
    let pointer = spur_graph::store::commit_index::load_pointer(worktree).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load commit index pointer in {}: {error}",
            worktree.display()
        ))
    })?;
    let pointer = pointer.ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "commit index not found; run `spur graph build --history` in {}",
            worktree.display()
        ))
    })?;
    spur_graph::store::commit_index::load_artifact(worktree, &pointer).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load commit index artifact in {}: {error}",
            worktree.display()
        ))
    })
}

fn current_worktree() -> Result<PathBuf, McpHandlerError> {
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
    })?;
    Ok(resolve_worktree_root_from(current_dir))
}

fn graph_artifact_missing(worktree: &Path) -> McpHandlerError {
    McpHandlerError::NotFound(format!(
        "graph artifact not found; run `spur graph build` in {}",
        worktree.display()
    ))
}

fn optional_string(args: &Value, field: &str) -> Result<Option<String>, McpHandlerError> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be a string"
        ))),
    }
}

fn strip_symbol_uri(value: String) -> String {
    value
        .strip_prefix(CODE_SYMBOL_URI_PREFIX)
        .unwrap_or(value.as_str())
        .to_string()
}

fn string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn large_string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a LargeStringArray, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<LargeStringArray>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn u8_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt8Array, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt8Array>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn u32_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt32Array, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt32Array>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn u64_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn lede(body_text: &str) -> String {
    body_text.chars().take(LEDE_CHARS).collect()
}

fn sql_string_literal(value: &str) -> String {
    value.replace('\'', "''")
}
