use std::path::Path;

use futures::TryStreamExt as _;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery as _, QueryBase as _};
use spur_graph::store::lance_sections::{SECTIONS_DATASET_DIR, SECTIONS_TABLE};

use crate::mcp::McpHandlerError;

use super::projection::{project_batch, DocHit};
use super::DocNavigateRequest;

pub(super) async fn open_sections_table(
    artifact_dir: &Path,
) -> Result<lancedb::Table, McpHandlerError> {
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

pub(super) async fn fts_hits(
    table: &lancedb::Table,
    request: &DocNavigateRequest,
) -> Result<Vec<DocHit>, McpHandlerError> {
    let query = request.query.as_deref().unwrap_or_default().to_owned();
    let fts = FullTextSearchQuery::new(query)
        .with_column("body_text".to_owned())
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

pub(super) async fn child_hits(
    table: &lancedb::Table,
    root: &str,
) -> Result<Vec<DocHit>, McpHandlerError> {
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

fn sql_string_literal(value: &str) -> String {
    value.replace('\'', "''")
}
