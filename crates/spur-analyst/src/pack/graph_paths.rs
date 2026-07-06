use std::path::Path;

use serde_json::json;

#[cfg(test)]
use crate::query_context_paths;
use crate::{query_context_paths_with_conn, KnowledgePathOptions, KnowledgePathResult};

use super::{
    caveat_value, graph_reasoning::GraphReasoningSections, push_graph_path_caveat,
    KnowledgeContextPackV2Request,
};

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

pub(crate) fn collect_graph_paths_with_conn(
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
                push_graph_path_result(sections, source, &target, path_result);
            }
            Err(error) => {
                push_unavailable_graph_path(sections, request, budget, source, &target, error);
            }
        }
    }
}

fn push_graph_path_result(
    sections: &mut GraphReasoningSections,
    source: &str,
    target: &str,
    path_result: KnowledgePathResult,
) {
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

fn push_unavailable_graph_path(
    sections: &mut GraphReasoningSections,
    request: &KnowledgeContextPackV2Request,
    budget: GraphPathBudgetPlan,
    source: &str,
    target: &str,
    error: anyhow::Error,
) {
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
