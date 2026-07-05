use std::path::Path;

use serde_json::{json, Value};

#[cfg(test)]
use crate::{query_context_paths, query_symbol_risk_community};
use crate::{
    query_context_paths_with_conn, query_symbol_risk_community_with_conn, KnowledgeCandidate,
    KnowledgePathOptions, KnowledgePathResult, KnowledgeQueryResult, SymbolEvidenceStatus,
    SymbolRiskCommunityResult, SymbolRiskScorecardRow,
};

use super::{
    analyst_matches_exact_graph, caveat_value, is_test_file, push_graph_path_caveat,
    raw_stable_symbol_id, symbol_caveat_value, ExactGraphContext, KnowledgeContextPackRequest,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GraphPathBudgetPlan {
    pub(crate) target_cap: usize,
    pub(crate) per_target_max_paths: usize,
}

pub(crate) fn path_budget_plan(num_targets: usize, max_paths: usize) -> GraphPathBudgetPlan {
    GraphPathBudgetPlan {
        target_cap: num_targets.min(max_paths),
        per_target_max_paths: max_paths,
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
            sections.caveats.push(caveat_value(
                "graph_reasoning_no_code_candidates",
                "graph reasoning sections require grounded code candidates",
                None,
            ));
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
            sections.caveats.push(caveat_value(
                "graph_reasoning_no_code_candidates",
                "graph reasoning sections require grounded code candidates",
                None,
            ));
        }
        return sections;
    }

    if request.graph_reasoning.paths {
        collect_graph_paths_with_conn(conn, db_path, request, &code_symbol_ids, &mut sections);
    }

    if wants_symbol_enrichment {
        let symbol_enrichment_error =
            match query_symbol_risk_community_with_conn(conn, db_path, &code_symbol_ids) {
                Ok(result) => {
                    apply_symbol_enrichment_result(
                        request,
                        wants_communities,
                        &mut sections,
                        result,
                    );
                    None
                }
                Err(error) => Some(error),
            };
        if let Some(error) = symbol_enrichment_error {
            sections.caveats.push(caveat_value(
                "symbol_enrichment_unavailable",
                format!("symbol graph enrichment unavailable: {error:#}"),
                None,
            ));
        }
    }

    sections
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

#[cfg(test)]
pub(crate) fn collect_graph_paths(
    db_path: &Path,
    request: &KnowledgeContextPackV2Request,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
) {
    collect_graph_paths_with_query(
        request,
        code_symbol_ids,
        sections,
        |source, target, options| query_context_paths(db_path, source, target, options),
    );
}

fn collect_graph_paths_with_conn(
    conn: &duckdb::Connection,
    db_path: &Path,
    request: &KnowledgeContextPackV2Request,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
) {
    collect_graph_paths_with_query(
        request,
        code_symbol_ids,
        sections,
        |source, target, options| {
            query_context_paths_with_conn(conn, db_path, source, target, options)
        },
    );
}

fn collect_graph_paths_with_query<F>(
    request: &KnowledgeContextPackV2Request,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
    mut query_paths: F,
) where
    F: FnMut(&str, &str, KnowledgePathOptions) -> anyhow::Result<KnowledgePathResult>,
{
    let Some(source) = code_symbol_ids.first() else {
        return;
    };
    let mut targets = code_symbol_ids.iter().skip(1).cloned().collect::<Vec<_>>();
    targets.extend(resolve_anchor_targets(
        source,
        &request.graph_reasoning.anchors,
        sections,
    ));
    dedupe_preserving_order(&mut targets);

    if targets.is_empty() {
        sections.caveats.push(caveat_value(
            "graph_paths_insufficient_targets",
            "graph path reasoning requires at least two grounded code candidates or a graph://symbol anchor",
            Some(source.clone()),
        ));
        return;
    }

    let budget = path_budget_plan(targets.len(), request.graph_reasoning.max_paths);
    for target in targets.into_iter().take(budget.target_cap) {
        match query_paths(
            source,
            &target,
            KnowledgePathOptions {
                max_hops: request.graph_reasoning.max_path_hops,
                max_paths: budget.per_target_max_paths,
                undirected: true,
            },
        ) {
            Ok(path_result) => {
                if let Some(caveat) = path_result.caveat.as_deref() {
                    push_graph_path_caveat(&mut sections.caveats, caveat, source);
                }
                sections.graph_paths.push(json!({
                    "source_stable_id": source,
                    "target_stable_id": target,
                    "graph_content_hash": path_result.graph_content_hash,
                    "max_hops": path_result.max_hops,
                    "max_paths": path_result.max_paths,
                    "engine": path_result.engine,
                    "status": path_result.status,
                    "caveat": path_result.caveat,
                    "rows": path_result.rows,
                }));
            }
            Err(error) => {
                let caveat = format!("context path search unavailable: {error:#}");
                push_graph_path_caveat(&mut sections.caveats, caveat.clone(), source);
                sections.graph_paths.push(json!({
                    "source_stable_id": source,
                    "target_stable_id": target,
                    "graph_content_hash": null,
                    "max_hops": request.graph_reasoning.max_path_hops,
                    "max_paths": budget.per_target_max_paths,
                    "engine": "unavailable",
                    "status": "unavailable",
                    "caveat": caveat,
                    "rows": [],
                }));
            }
        }
    }
}

fn resolve_anchor_targets(
    source: &str,
    anchors: &[String],
    sections: &mut GraphReasoningSections,
) -> Vec<String> {
    anchors
        .iter()
        .filter_map(|anchor| {
            let trimmed = anchor.trim();
            let Some(target) = stable_symbol_anchor(trimmed) else {
                sections.caveats.push(caveat_value(
                    "graph_anchor_unresolved",
                    format!(
                        "anchor {trimmed:?} is not a graph://symbol selector or bare stable symbol id"
                    ),
                    None,
                ));
                return None;
            };
            if target == source {
                sections.caveats.push(caveat_value(
                    "graph_anchor_same_as_source",
                    format!("anchor {trimmed:?} resolves to the source symbol"),
                    Some(source.to_owned()),
                ));
                return None;
            }
            Some(target.to_owned())
        })
        .collect()
}

fn stable_symbol_anchor(anchor: &str) -> Option<&str> {
    if let Some(id) = anchor.strip_prefix("graph://symbol/") {
        return (!id.is_empty()).then_some(id);
    }
    let looks_like_stable_id = anchor.len() >= 8
        && anchor
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    looks_like_stable_id.then_some(anchor)
}

fn dedupe_preserving_order(values: &mut Vec<String>) {
    let mut deduped = Vec::with_capacity(values.len());
    for value in values.drain(..) {
        if !deduped.contains(&value) {
            deduped.push(value);
        }
    }
    *values = deduped;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_budget_plan_caps_targets_without_shrinking_per_target_limit() {
        const MAX_PATHS: usize = 4;
        let plan = path_budget_plan(6, MAX_PATHS);

        assert_eq!(plan.target_cap, MAX_PATHS);
        assert_eq!(plan.per_target_max_paths, MAX_PATHS);

        let smaller_target_set = path_budget_plan(2, MAX_PATHS);
        assert_eq!(smaller_target_set.target_cap, 2);
        assert_eq!(smaller_target_set.per_target_max_paths, MAX_PATHS);
    }
}
