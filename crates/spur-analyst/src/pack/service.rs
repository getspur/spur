use std::{path::Path, sync::Arc};

use crate::embedding::EmbeddingRuntime;
use crate::search::graph_candidates::merge_graph_candidates;
#[cfg(test)]
use crate::search::hybrid::confidence_score_thresholds;
use crate::{
    db::{
        connection::open_analyst_connection_read_only,
        extensions::{load_analyst_icu_extension, load_analyst_lance_extension},
        paths::{analyst_db_path, current_repo_root},
    },
    query_context_candidates_with_conn, query_graph_candidates_with_conn, KnowledgeQueryOptions,
    KnowledgeQueryResult,
};
use serde_json::{json, Value};

use crate::pack::{
    base_pack, caveat_value, exact_graph_context_for_result,
    graph_reasoning_sections_for_pack_with_conn, insert_v2_sections,
    pack_query_result_v2_with_graph_sections_and_staleness, pack_query_result_with_exact_context,
    GraphReasoningSections, KnowledgeContextPackRequest, KnowledgeContextPackV2Request,
    PackErrorExt as _, PackStaleness,
};
#[cfg(test)]
use crate::pack::{
    code_next_tools, collect_graph_paths, graph_reasoning_sections_for_pack, pack_query_result,
    pack_query_result_v2_with_graph_sections, path_budget_plan, recommended_next_tools,
    ExactGraphContext, KnowledgeIntent, SymbolImpactSummary, POPULAR_SINK_CALLERS_THRESHOLD,
};

use crate::mcp::McpHandlerError;
use crate::overlay::{
    open_worktree_overlay, overlay_rebuild_key_for_dirty_worktree,
    shared_overlay_session_coordinator, write_delta_for_session, OverlayMergeSession,
};

pub(crate) async fn knowledge_context_pack(
    request: KnowledgeContextPackRequest,
) -> Result<Value, McpHandlerError> {
    let db_path = analyst_db_path()?;
    if !db_path.exists() {
        return Ok(unavailable_pack(&request, &db_path));
    }

    let query_vec = EmbeddingRuntime::global()
        .embed_query(&request.query)
        .await
        .map(Vec::from);
    let query_result = {
        let conn = open_pack_connection(&db_path, "knowledge_context_pack")?;
        query_candidates_for_request_with_conn(
            &request,
            &db_path,
            &conn,
            "knowledge_context_pack",
            query_vec,
        )?
    };

    let exact_context = exact_graph_context_for_result(&request, &query_result).await;
    Ok(pack_query_result_with_exact_context(&request, query_result, exact_context).await)
}

pub(crate) async fn knowledge_context_pack_2(
    request: KnowledgeContextPackV2Request,
) -> Result<Value, McpHandlerError> {
    let db_path = analyst_db_path()?;
    if !db_path.exists() {
        return Ok(unavailable_pack_v2(&request, &db_path));
    }

    let query_vec = EmbeddingRuntime::global()
        .embed_query(&request.base.query)
        .await
        .map(Vec::from);
    let pack_connection =
        open_pack_connection_with_overlay(&db_path, "knowledge_context_pack_2").await?;
    let query_result = query_candidates_for_request_with_conn(
        &request.base,
        &db_path,
        pack_connection.candidate_conn(),
        "knowledge_context_pack_2",
        query_vec,
    )?;

    let exact_context = exact_graph_context_for_result(&request.base, &query_result).await;
    let staleness = pack_connection.staleness.clone();
    let graph_sections = graph_reasoning_sections_for_pack_with_conn(
        &request,
        &query_result,
        &exact_context,
        &db_path,
        pack_connection.graph_conn(),
        &staleness,
    );
    drop(pack_connection);
    Ok(pack_query_result_v2_with_graph_sections_and_staleness(
        &request,
        query_result,
        exact_context,
        graph_sections,
        staleness,
    )
    .await)
}

fn open_pack_connection(
    db_path: &Path,
    tool_name: &str,
) -> Result<duckdb::Connection, McpHandlerError> {
    let conn = open_analyst_connection_read_only(db_path).map_err(|error| {
        McpHandlerError::Internal(format!(
            "{tool_name} failed to query analyst DB at {}: {error}",
            db_path.display()
        ))
    })?;
    load_analyst_icu_extension(&conn);
    load_analyst_lance_extension(&conn);
    Ok(conn)
}

struct PackConnection {
    base_conn: duckdb::Connection,
    overlay_conn: Option<duckdb::Connection>,
    staleness: PackStaleness,
}

impl PackConnection {
    fn candidate_conn(&self) -> &duckdb::Connection {
        &self.base_conn
    }

    fn graph_conn(&self) -> &duckdb::Connection {
        self.overlay_conn.as_ref().unwrap_or(&self.base_conn)
    }
}

async fn open_pack_connection_with_overlay(
    db_path: &Path,
    tool_name: &str,
) -> Result<PackConnection, McpHandlerError> {
    let base_conn = open_pack_connection(db_path, tool_name)?;
    let base_graph_hash = graph_content_hash_from_conn(&base_conn);
    let mut staleness = PackStaleness::base_only(base_graph_hash.clone());

    let overlay_session = overlay_session_for_current_worktree(db_path, base_graph_hash).await;
    let overlay_conn = match overlay_session.as_ref().and_then(|session| {
        session
            .delta_dir()
            .map(|delta_dir| (session.base_db_path(), delta_dir))
    }) {
        Some((base_path, delta_dir)) => match open_worktree_overlay(base_path, delta_dir) {
            Ok(conn) => {
                load_analyst_icu_extension(&conn);
                load_analyst_lance_extension(&conn);
                staleness = overlay_session
                    .as_ref()
                    .map(|session| PackStaleness {
                        delta_applied: session.delta_applied(),
                        algo_as_of: session.algo_as_of().map(str::to_owned),
                    })
                    .unwrap_or_else(|| PackStaleness::base_only(staleness.algo_as_of.clone()));
                Some(conn)
            }
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    delta_dir = %delta_dir.display(),
                    "failed to open analyst worktree overlay; serving base analyst DB"
                );
                None
            }
        },
        None => None,
    };

    if overlay_conn.is_none() {
        if let Some(session) = overlay_session.as_ref() {
            staleness = PackStaleness {
                delta_applied: false,
                algo_as_of: session.algo_as_of().map(str::to_owned),
            };
        }
    }

    Ok(PackConnection {
        base_conn,
        overlay_conn,
        staleness,
    })
}

async fn overlay_session_for_current_worktree(
    db_path: &Path,
    base_graph_hash: Option<String>,
) -> Option<Arc<OverlayMergeSession>> {
    let worktree = current_repo_root().ok()?;
    let seed = spur_graph::cache::load_base_seed_for_worktree(&worktree)?;
    let rebuild_key = overlay_rebuild_key_for_dirty_worktree(&worktree)?;
    let coordinator = shared_overlay_session_coordinator();
    let artifact = Arc::clone(&seed.artifact);
    let build_worktree = worktree.clone();
    let build_key = rebuild_key.clone();

    Some(
        coordinator
            .get_or_build_session(
                worktree,
                rebuild_key,
                db_path.to_path_buf(),
                base_graph_hash,
                move |mode| {
                    let artifact = Arc::clone(&artifact);
                    let worktree = build_worktree.clone();
                    let key = build_key.clone();
                    async move {
                        tokio::task::spawn_blocking(move || {
                            write_delta_for_session(&worktree, &key, &artifact, mode)
                        })
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("analyst overlay delta task failed: {error}")
                        })?
                    }
                },
            )
            .await,
    )
}

fn graph_content_hash_from_conn(conn: &duckdb::Connection) -> Option<String> {
    conn.query_row("SELECT graph_content_hash FROM _meta", [], |row| row.get(0))
        .ok()
}

fn query_candidates_for_request_with_conn(
    request: &KnowledgeContextPackRequest,
    db_path: &Path,
    conn: &duckdb::Connection,
    tool_name: &str,
    query_vec: Option<Vec<f32>>,
) -> Result<KnowledgeQueryResult, McpHandlerError> {
    let analyst_intent = request.intent.as_analyst_intent();
    let mut query_result = query_context_candidates_with_conn(
        conn,
        db_path,
        &request.query,
        request.scope.as_analyst_scope(),
        KnowledgeQueryOptions {
            limit: request.limit as usize,
            intent: analyst_intent,
            query_vec,
        },
    )
    .map_err(|error| {
        McpHandlerError::Internal(format!(
            "{tool_name} failed to query analyst DB at {}: {error}",
            db_path.display()
        ))
    })?;

    if request.should_query_graph_candidates() {
        match query_graph_candidates_with_conn(
            conn,
            db_path,
            &request.query,
            KnowledgeQueryOptions {
                limit: request.limit as usize,
                intent: analyst_intent,
                query_vec: None,
            },
        ) {
            Ok(graph_result) => merge_graph_candidates(&mut query_result, graph_result),
            Err(error) => tracing::warn!(
                db_path = %db_path.display(),
                error = %error,
                tool = tool_name,
                "knowledge context pack failed to query graph candidates; continuing with context candidates"
            ),
        }
    }

    Ok(query_result)
}

fn unavailable_pack(request: &KnowledgeContextPackRequest, db_path: &Path) -> Value {
    base_pack(
        request,
        None,
        json!({ "available": false, "reason": "analyst_db_missing" }),
    )
    .with_error(json!({
        "code": "analyst_unavailable",
        "message": format!("analyst DB not found at {}", db_path.display()),
        "db_path": db_path.display().to_string()
    }))
}

fn unavailable_pack_v2(request: &KnowledgeContextPackV2Request, db_path: &Path) -> Value {
    let mut pack = unavailable_pack(&request.base, db_path);
    insert_v2_sections(
        &mut pack,
        GraphReasoningSections {
            caveats: vec![caveat_value(
                "analyst_unavailable",
                format!("analyst DB not found at {}", db_path.display()),
                None,
            )],
            ..GraphReasoningSections::default()
        },
    );
    pack
}

#[cfg(test)]
async fn pack_query_result_v2_with_graph_reasoning(
    request: &KnowledgeContextPackV2Request,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
    db_path: &Path,
) -> Value {
    let graph_sections =
        graph_reasoning_sections_for_pack(request, &result, &exact_context, db_path);
    pack_query_result_v2_with_graph_sections(request, result, exact_context, graph_sections).await
}

#[cfg(test)]
#[path = "../../tests/pack/service_unit.rs"]
mod tests;
