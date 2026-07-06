use std::path::Path;

use serde_json::{json, Value};

#[cfg(test)]
use crate::query_symbol_risk_community;
use crate::{
    query_symbol_risk_community_with_conn, KnowledgeCandidate, KnowledgeQueryResult,
    SymbolEvidenceStatus, SymbolRiskCommunityResult, SymbolRiskScorecardRow,
};

#[cfg(test)]
use super::graph_paths::collect_graph_paths;
use super::graph_paths::collect_graph_paths_with_conn;
use super::{
    analyst_matches_exact_graph, caveat_value, is_test_file, raw_stable_symbol_id,
    symbol_caveat_value, ExactGraphContext, KnowledgeContextPackRequest,
    KnowledgeContextPackV2Request, PackStaleness,
};

#[derive(Default)]
pub(crate) struct GraphReasoningSections {
    pub(crate) graph_paths: Vec<Value>,
    pub(crate) risk_scorecard: Vec<Value>,
    pub(crate) community_context: Vec<Value>,
    pub(crate) temporal_context: Vec<Value>,
    pub(crate) caveats: Vec<Value>,
}

impl GraphReasoningSections {
    pub(crate) fn with_caveat(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            caveats: vec![caveat_value(code, message, None)],
            ..Self::default()
        }
    }
}

pub(crate) fn insert_v2_sections(pack: &mut Value, sections: GraphReasoningSections) {
    if let Some(object) = pack.as_object_mut() {
        object.insert("graph_paths".into(), Value::Array(sections.graph_paths));
        object.insert(
            "risk_scorecard".into(),
            Value::Array(sections.risk_scorecard),
        );
        object.insert(
            "community_context".into(),
            Value::Array(sections.community_context),
        );
        object.insert(
            "temporal_context".into(),
            Value::Array(sections.temporal_context),
        );
        object.insert("caveats".into(), Value::Array(sections.caveats));
        object.entry("candidates").or_insert_with(|| {
            json!({
                "total": 0,
                "returned_primary": 0,
                "returned_supporting_docs": 0,
                "total_code": 0,
                "total_docs": 0,
            })
        });
    }
}

#[cfg(test)]
pub(crate) fn graph_reasoning_sections_for_pack(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
    db_path: &Path,
) -> GraphReasoningSections {
    match analyst_matches_exact_graph(result, exact_context) {
        Some(false) if request.graph_reasoning.any_enabled() => {
            stale_graph_reasoning_sections(result, exact_context)
        }
        _ => graph_reasoning_sections(request, result, db_path),
    }
}

pub(crate) fn graph_reasoning_sections_for_pack_with_conn(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
    db_path: &Path,
    conn: &duckdb::Connection,
    staleness: &PackStaleness,
) -> GraphReasoningSections {
    match analyst_matches_exact_graph(result, exact_context) {
        Some(false) if request.graph_reasoning.any_enabled() && !staleness.delta_applied => {
            stale_graph_reasoning_sections(result, exact_context)
        }
        _ => graph_reasoning_sections_with_conn(request, result, db_path, conn),
    }
}

fn stale_graph_reasoning_sections(
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
) -> GraphReasoningSections {
    let analyst_hash = result.graph_content_hash.as_deref().unwrap_or("<missing>");
    let exact_hash = exact_context
        .graph_content_hash
        .as_deref()
        .unwrap_or("<missing>");
    GraphReasoningSections::with_caveat(
        "analyst_graph_stale",
        format!(
            "analyst DB graph hash {analyst_hash} differs from exact graph hash {exact_hash}; graph reasoning skipped until analyst DB is rebuilt"
        ),
    )
}

#[cfg(test)]
pub(crate) fn graph_reasoning_sections(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    db_path: &Path,
) -> GraphReasoningSections {
    let code_symbol_ids = graph_reasoning_code_symbol_ids(&result.candidates, &request.base);
    let mut sections = GraphReasoningSections::default();
    let wants_communities = request
        .graph_reasoning
        .should_query_communities(code_symbol_ids.len());
    let wants_symbol_enrichment = request.graph_reasoning.risk || wants_communities;

    if code_symbol_ids.is_empty() {
        if request.graph_reasoning.paths || wants_symbol_enrichment {
            push_no_code_candidates_caveat(&mut sections);
        }
        return sections;
    }

    if request.graph_reasoning.paths {
        collect_graph_paths(db_path, request, &code_symbol_ids, &mut sections);
    }

    if wants_symbol_enrichment {
        match query_symbol_risk_community(db_path, &code_symbol_ids) {
            Ok(result) => {
                apply_symbol_enrichment_result(request, wants_communities, &mut sections, result);
            }
            Err(error) => sections.caveats.push(caveat_value(
                "symbol_enrichment_unavailable",
                format!("symbol graph enrichment unavailable: {error:#}"),
                None,
            )),
        }
    }

    sections
}

fn graph_reasoning_sections_with_conn(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    db_path: &Path,
    conn: &duckdb::Connection,
) -> GraphReasoningSections {
    let code_symbol_ids = graph_reasoning_code_symbol_ids(&result.candidates, &request.base);
    let mut sections = GraphReasoningSections::default();
    let wants_communities = request
        .graph_reasoning
        .should_query_communities(code_symbol_ids.len());
    let wants_symbol_enrichment = request.graph_reasoning.risk || wants_communities;

    if code_symbol_ids.is_empty() {
        if request.graph_reasoning.paths || wants_symbol_enrichment {
            push_no_code_candidates_caveat(&mut sections);
        }
        return sections;
    }

    if request.graph_reasoning.paths {
        collect_graph_paths_with_conn(conn, db_path, request, &code_symbol_ids, &mut sections);
    }

    if wants_symbol_enrichment {
        apply_symbol_enrichment_with_conn(
            conn,
            db_path,
            request,
            wants_communities,
            &code_symbol_ids,
            &mut sections,
        );
    }

    sections
}

fn push_no_code_candidates_caveat(sections: &mut GraphReasoningSections) {
    sections.caveats.push(caveat_value(
        "graph_reasoning_no_code_candidates",
        "graph reasoning sections require grounded code candidates",
        None,
    ));
}

fn apply_symbol_enrichment_with_conn(
    conn: &duckdb::Connection,
    db_path: &Path,
    request: &KnowledgeContextPackV2Request,
    wants_communities: bool,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
) {
    match query_symbol_risk_community_with_conn(conn, db_path, code_symbol_ids) {
        Ok(result) => {
            apply_symbol_enrichment_result(request, wants_communities, sections, result);
        }
        Err(error) => sections.caveats.push(caveat_value(
            "symbol_enrichment_unavailable",
            format!("symbol graph enrichment unavailable: {error:#}"),
            None,
        )),
    }
}

fn apply_symbol_enrichment_result(
    request: &KnowledgeContextPackV2Request,
    wants_communities: bool,
    sections: &mut GraphReasoningSections,
    result: SymbolRiskCommunityResult,
) {
    let risk_rows = result.risk_scorecard;
    let community_rows = result.community_context;
    if request.graph_reasoning.risk {
        sections.temporal_context = temporal_context_from_risk_rows(&risk_rows);
        sections.risk_scorecard = risk_rows
            .iter()
            .filter_map(to_json_value)
            .collect::<Vec<_>>();
    } else {
        sections.temporal_context = Vec::new();
    }
    if wants_communities {
        sections.community_context = community_rows
            .iter()
            .filter_map(to_json_value)
            .collect::<Vec<_>>();
    }
    sections
        .caveats
        .extend(result.caveats.iter().map(symbol_caveat_value));
    sections.caveats.extend(risk_rows.iter().flat_map(|row| {
        row.caveats
            .iter()
            .map(symbol_caveat_value)
            .collect::<Vec<_>>()
    }));
    sections
        .caveats
        .extend(community_rows.iter().flat_map(|row| {
            row.caveats
                .iter()
                .map(symbol_caveat_value)
                .collect::<Vec<_>>()
        }));
}

fn graph_reasoning_code_symbol_ids(
    candidates: &[KnowledgeCandidate],
    request: &KnowledgeContextPackRequest,
) -> Vec<String> {
    let mut ids = Vec::new();
    for candidate in candidates {
        if ids.len() >= request.limit as usize {
            break;
        }
        if !request.include_tests && is_test_file(&candidate.file_path) {
            continue;
        }
        if candidate.kind != "code" && candidate.kind != "symbol" {
            continue;
        }
        let Some(stable_symbol_id) = candidate.stable_symbol_id.as_deref() else {
            continue;
        };
        let id = raw_stable_symbol_id(stable_symbol_id).to_owned();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn temporal_context_from_risk_rows(rows: &[SymbolRiskScorecardRow]) -> Vec<Value> {
    rows.iter()
        .filter(|row| row.status == SymbolEvidenceStatus::Available)
        .filter(|row| row.churn_90d.is_some() || row.last_touched.is_some())
        .map(|row| {
            json!({
                "input_index": row.input_index,
                "stable_symbol_id": row.stable_symbol_id,
                "file_path": row.file_path,
                "churn_90d": row.churn_90d,
                "last_touched": row.last_touched,
                "posture": row.posture,
            })
        })
        .collect()
}

fn to_json_value<T: serde::Serialize>(value: &T) -> Option<Value> {
    serde_json::to_value(value).ok()
}
