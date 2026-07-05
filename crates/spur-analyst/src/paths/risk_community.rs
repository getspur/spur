use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{
    api::{
        SymbolCommunityContextRow, SymbolEvidenceCaveat, SymbolEvidenceStatus, SymbolGraphMetrics,
        SymbolInput, SymbolRiskCommunityResult, SymbolRiskScorecardRow,
    },
    db::{
        connection::open_analyst_connection_read_only, extensions::load_analyst_icu_extension,
        sql::sql_string_literal,
    },
};

use super::{graph_content_hash, i64_to_usize};

pub const MAX_SYMBOL_RISK_COMMUNITY_IDS: usize = 40;

pub fn query_symbol_risk_community<S: AsRef<str>>(
    db_path: &Path,
    stable_symbol_ids: &[S],
) -> Result<SymbolRiskCommunityResult> {
    let conn = open_analyst_connection_read_only(db_path)?;
    load_analyst_icu_extension(&conn);
    query_symbol_risk_community_with_conn(&conn, db_path, stable_symbol_ids)
}

pub fn query_symbol_risk_community_with_conn<S: AsRef<str>>(
    conn: &duckdb::Connection,
    db_path: &Path,
    stable_symbol_ids: &[S],
) -> Result<SymbolRiskCommunityResult> {
    let (inputs, mut caveats, truncated) = bounded_symbol_inputs(stable_symbol_ids);
    let mut result = SymbolRiskCommunityResult {
        db_path: db_path.display().to_string(),
        graph_content_hash: graph_content_hash(conn),
        max_symbols: MAX_SYMBOL_RISK_COMMUNITY_IDS,
        truncated,
        risk_scorecard: Vec::new(),
        community_context: Vec::new(),
        graph_metrics: None,
        caveats: Vec::new(),
    };
    result.caveats.append(&mut caveats);

    if inputs.is_empty() {
        return Ok(result);
    }

    match query_symbol_risk_scorecard_rows(conn, &inputs) {
        Ok(rows) => result.risk_scorecard = rows,
        Err(error) => {
            let caveat = SymbolEvidenceCaveat {
                stable_symbol_id: None,
                code: "scorecard_unavailable".to_owned(),
                message: format!("v_symbol_scorecard unavailable: {error:#}"),
            };
            result
                .risk_scorecard
                .extend(unavailable_risk_rows(&inputs, &caveat));
            result.caveats.push(caveat);
        }
    }

    match query_symbol_community_context_rows(conn, &inputs) {
        Ok(rows) => result.community_context = rows,
        Err(error) => {
            let caveat = SymbolEvidenceCaveat {
                stable_symbol_id: None,
                code: "community_unavailable".to_owned(),
                message: format!("v_symbol_component/v_symbol_community unavailable: {error:#}"),
            };
            result
                .community_context
                .extend(unavailable_community_rows(&inputs, &caveat));
            result.caveats.push(caveat);
        }
    }

    match query_symbol_graph_metrics(conn) {
        Ok(metrics) => result.graph_metrics = metrics,
        Err(error) => result.caveats.push(SymbolEvidenceCaveat {
            stable_symbol_id: None,
            code: "graph_metrics_unavailable".to_owned(),
            message: format!("v_graph_metrics unavailable: {error:#}"),
        }),
    }

    Ok(result)
}

fn bounded_symbol_inputs<S: AsRef<str>>(
    stable_symbol_ids: &[S],
) -> (Vec<SymbolInput>, Vec<SymbolEvidenceCaveat>, bool) {
    let mut inputs = Vec::new();
    let mut caveats = Vec::new();
    let mut truncated = false;

    for (input_index, stable_symbol_id) in stable_symbol_ids.iter().enumerate() {
        let stable_symbol_id = stable_symbol_id.as_ref().trim();
        if stable_symbol_id.is_empty() {
            caveats.push(SymbolEvidenceCaveat {
                stable_symbol_id: None,
                code: "empty_stable_symbol_id".to_owned(),
                message: format!("ignored empty stable symbol ID at input index {input_index}"),
            });
            continue;
        }
        if inputs.len() >= MAX_SYMBOL_RISK_COMMUNITY_IDS {
            truncated = true;
            continue;
        }
        inputs.push(SymbolInput {
            input_index,
            stable_symbol_id: stable_symbol_id.to_owned(),
        });
    }

    if truncated {
        caveats.push(SymbolEvidenceCaveat {
            stable_symbol_id: None,
            code: "input_truncated".to_owned(),
            message: format!(
                "symbol enrichment input was capped at {MAX_SYMBOL_RISK_COMMUNITY_IDS} stable IDs"
            ),
        });
    }

    (inputs, caveats, truncated)
}

fn query_symbol_risk_scorecard_rows(
    conn: &duckdb::Connection,
    inputs: &[SymbolInput],
) -> Result<Vec<SymbolRiskScorecardRow>> {
    let input_values = symbol_input_values_sql(inputs);
    let sql = format!(
        "WITH input(input_index, stable_symbol_id) AS (VALUES {input_values}) \
         SELECT i.input_index, i.stable_symbol_id, sc.stable_symbol_id, \
                sc.entity_name, sc.qualified_name, sc.symbol_kind, sc.file_path, \
                sc.pagerank, sc.in_degree, sc.out_degree, sc.callers, sc.importers, \
                sc.inbound_total, sc.churn_90d, CAST(sc.last_touched AS VARCHAR), \
                sc.blast_radius_score, sc.posture \
         FROM input i \
         LEFT JOIN v_symbol_scorecard sc \
           ON sc.stable_symbol_id = i.stable_symbol_id \
         ORDER BY i.input_index"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare symbol scorecard enrichment query")?;
    let rows = stmt
        .query_map([], |row| {
            let input_index = i64_to_usize(row.get(0)?);
            let stable_symbol_id: String = row.get(1)?;
            let matched_symbol_id: Option<String> = row.get(2)?;
            let status = if matched_symbol_id.is_some() {
                SymbolEvidenceStatus::Available
            } else {
                SymbolEvidenceStatus::MissingSymbol
            };
            let caveats = if status == SymbolEvidenceStatus::MissingSymbol {
                vec![SymbolEvidenceCaveat {
                    stable_symbol_id: Some(stable_symbol_id.clone()),
                    code: "scorecard_missing_symbol".to_owned(),
                    message: format!(
                        "stable symbol ID {stable_symbol_id:?} has no row in v_symbol_scorecard"
                    ),
                }]
            } else {
                Vec::new()
            };
            Ok(SymbolRiskScorecardRow {
                input_index,
                stable_symbol_id,
                status,
                entity_name: row.get(3)?,
                qualified_name: row.get(4)?,
                symbol_kind: row.get(5)?,
                file_path: row.get(6)?,
                pagerank: row.get(7)?,
                in_degree: row.get(8)?,
                out_degree: row.get(9)?,
                callers: row.get(10)?,
                importers: row.get(11)?,
                inbound_total: row.get(12)?,
                churn_90d: row.get(13)?,
                last_touched: row.get(14)?,
                blast_radius_score: row.get(15)?,
                posture: row.get(16)?,
                caveats,
            })
        })
        .context("failed to run symbol scorecard enrichment query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read symbol scorecard enrichment rows")
}

fn query_symbol_community_context_rows(
    conn: &duckdb::Connection,
    inputs: &[SymbolInput],
) -> Result<Vec<SymbolCommunityContextRow>> {
    let input_values = symbol_input_values_sql(inputs);
    let sql = format!(
        "WITH input(input_index, stable_symbol_id) AS (VALUES {input_values}) \
         SELECT i.input_index, i.stable_symbol_id, \
                cmp.stable_symbol_id, comm.stable_symbol_id, \
                cmp.component_id, cmp.component_size, comm.community_id \
         FROM input i \
         LEFT JOIN v_symbol_component cmp \
           ON cmp.stable_symbol_id = i.stable_symbol_id \
         LEFT JOIN v_symbol_community comm \
           ON comm.stable_symbol_id = i.stable_symbol_id \
         ORDER BY i.input_index"
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare symbol community enrichment query")?;
    let rows = stmt
        .query_map([], |row| {
            let input_index = i64_to_usize(row.get(0)?);
            let stable_symbol_id: String = row.get(1)?;
            let component_symbol_id: Option<String> = row.get(2)?;
            let community_symbol_id: Option<String> = row.get(3)?;
            let status = if component_symbol_id.is_some() || community_symbol_id.is_some() {
                SymbolEvidenceStatus::Available
            } else {
                SymbolEvidenceStatus::MissingSymbol
            };
            let caveats = if status == SymbolEvidenceStatus::MissingSymbol {
                vec![SymbolEvidenceCaveat {
                    stable_symbol_id: Some(stable_symbol_id.clone()),
                    code: "community_missing_symbol".to_owned(),
                    message: format!(
                        "stable symbol ID {stable_symbol_id:?} has no row in v_symbol_component or v_symbol_community"
                    ),
                }]
            } else {
                Vec::new()
            };
            Ok(SymbolCommunityContextRow {
                input_index,
                stable_symbol_id,
                status,
                component_id: row.get(4)?,
                component_size: row.get(5)?,
                community_id: row.get(6)?,
                caveats,
            })
        })
        .context("failed to run symbol community enrichment query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read symbol community enrichment rows")
}

fn query_symbol_graph_metrics(conn: &duckdb::Connection) -> Result<Option<SymbolGraphMetrics>> {
    let mut stmt = conn
        .prepare(
            "SELECT calls_edges, connected_nodes, components, largest_component, communities, density \
             FROM v_graph_metrics \
             LIMIT 1",
        )
        .context("failed to prepare graph metrics query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SymbolGraphMetrics {
                calls_edges: row.get(0)?,
                connected_nodes: row.get(1)?,
                components: row.get(2)?,
                largest_component: row.get(3)?,
                communities: row.get(4)?,
                density: row.get(5)?,
            })
        })
        .context("failed to run graph metrics query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read graph metrics rows")
        .map(|rows| rows.into_iter().next())
}

fn unavailable_risk_rows(
    inputs: &[SymbolInput],
    caveat: &SymbolEvidenceCaveat,
) -> Vec<SymbolRiskScorecardRow> {
    inputs
        .iter()
        .map(|input| SymbolRiskScorecardRow {
            input_index: input.input_index,
            stable_symbol_id: input.stable_symbol_id.clone(),
            status: SymbolEvidenceStatus::Unavailable,
            entity_name: None,
            qualified_name: None,
            symbol_kind: None,
            file_path: None,
            pagerank: None,
            in_degree: None,
            out_degree: None,
            callers: None,
            importers: None,
            inbound_total: None,
            churn_90d: None,
            last_touched: None,
            blast_radius_score: None,
            posture: None,
            caveats: vec![SymbolEvidenceCaveat {
                stable_symbol_id: Some(input.stable_symbol_id.clone()),
                code: caveat.code.clone(),
                message: caveat.message.clone(),
            }],
        })
        .collect()
}

fn unavailable_community_rows(
    inputs: &[SymbolInput],
    caveat: &SymbolEvidenceCaveat,
) -> Vec<SymbolCommunityContextRow> {
    inputs
        .iter()
        .map(|input| SymbolCommunityContextRow {
            input_index: input.input_index,
            stable_symbol_id: input.stable_symbol_id.clone(),
            status: SymbolEvidenceStatus::Unavailable,
            component_id: None,
            component_size: None,
            community_id: None,
            caveats: vec![SymbolEvidenceCaveat {
                stable_symbol_id: Some(input.stable_symbol_id.clone()),
                code: caveat.code.clone(),
                message: caveat.message.clone(),
            }],
        })
        .collect()
}

fn symbol_input_values_sql(inputs: &[SymbolInput]) -> String {
    inputs
        .iter()
        .map(|input| {
            format!(
                "({}, {})",
                input.input_index,
                sql_string_literal(&input.stable_symbol_id)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
