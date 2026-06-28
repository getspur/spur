//! Query builders for the external code context MCP tools.

use crate::catalog::readable_table;
use anyhow::{anyhow, bail, Context as _, Result};
use duckdb::{params, Connection, Row, ToSql};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const DEFAULT_SOURCE: &str = "registry:crates-io";
const PKG_SYMBOL_URI_PREFIX: &str = "pkg-symbol://";
const CALL_EDGE_KIND_CALLS: &str = "calls";
const CALL_EDGE_KIND_DYN: &str = "calls_dyn";
const CALL_EDGE_KIND_HOF: &str = "references_hof";

#[derive(Debug)]
struct QueryTables {
    nodes: String,
    files: String,
    edges: String,
    edges_unresolved: String,
    refs: String,
    package_catalog: String,
    uses_gold: bool,
}

impl QueryTables {
    fn load(db: &Connection) -> Result<Self> {
        let nodes = readable_table(db, "nodes")?;
        let uses_gold = nodes.starts_with("gold.");
        Ok(Self {
            nodes,
            files: readable_table(db, "files")?,
            edges: readable_table(db, "edges")?,
            edges_unresolved: readable_table(db, "edges_unresolved")?,
            refs: readable_table(db, "refs")?,
            package_catalog: readable_table(db, "package_catalog")?,
            uses_gold,
        })
    }

    fn published_filter(&self, alias: &str) -> String {
        if self.uses_gold {
            format!(
                r"
                AND {alias}.generation = (
                    SELECT pc.generation
                    FROM {} pc
                    WHERE pc.source = {alias}.source
                      AND pc.package = {alias}.package
                      AND pc.revision = {alias}.revision
                    LIMIT 1
                )
                ",
                self.package_catalog
            )
        } else {
            String::new()
        }
    }

    fn same_generation_join(&self, left: &str, right: &str) -> &'static str {
        if self.uses_gold {
            match (left, right) {
                ("n", "e") => "AND n.generation = e.generation",
                ("f", "n") => "AND f.generation = n.generation",
                _ => "",
            }
        } else {
            ""
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Exact,
    Prefix,
    Substring,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub query: String,
    pub mode: SearchMode,
    pub symbol_kind: Option<String>,
    pub file_glob: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeSearchResult {
    pub candidates: Vec<CodeCandidate>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeCandidate {
    pub selector: String,
    pub uri: String,
    pub id: String,
    pub stable_symbol_id: String,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub entity_name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub line_range: [usize; 2],
    pub symbol_kind: String,
    pub enclosing_scope: Option<String>,
}

pub type CandidateRow = CodeCandidate;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolSource {
    pub selector: String,
    pub stable_symbol_id: String,
    pub package_source: String,
    pub package: String,
    pub revision: String,
    pub file_path: String,
    pub byte_range: [usize; 2],
    pub line_range: [usize; 2],
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeMetadata {
    pub source_stable_id: String,
    pub target_stable_id: Option<String>,
    pub target_label: Option<String>,
    pub target_package: Option<String>,
    pub relation: String,
    pub edge_kind: String,
    pub confidence: Option<String>,
    pub confidence_score: Option<f64>,
    pub bind_method: Option<String>,
    pub receiver_text: Option<String>,
    pub scope_text: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CountsByKind {
    pub calls: usize,
    pub calls_dyn: usize,
    pub references_hof: usize,
    pub references_other: usize,
    pub unresolved: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallerRecord {
    pub caller: CodeCandidate,
    pub edge: EdgeMetadata,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalleeRecord {
    pub callee: Option<CodeCandidate>,
    pub edge: EdgeMetadata,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallerResult {
    pub callers: Vec<CallerRecord>,
    pub counts_by_kind: CountsByKind,
    pub unresolved_sample: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalleeResult {
    pub callees: Vec<CalleeRecord>,
    pub counts_by_kind: CountsByKind,
    pub unresolved_sample: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SelectorResolution {
    Resolved(ResolvedSymbol),
    Ambiguous { candidates: Vec<CandidateRow> },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedSymbol {
    pub stable_symbol_id: String,
}

pub fn search_symbols(db: &Connection, opts: &SearchOptions) -> Result<CodeSearchResult> {
    let tables = QueryTables::load(db)?;
    let (count_where, count_params, _) = search_filter(opts);
    let count_sql = format!(
        "SELECT COUNT(*) FROM {} n {count_where} {}",
        tables.nodes,
        tables.published_filter("n")
    );
    let total_matches = query_count(db, &count_sql, count_params)
        .context("failed to count matching code symbols")?;

    let (select_where, mut select_params, query_param) = search_filter(opts);
    let limit = opts.limit.clamp(1, 200) as i64;
    let limit_param = select_params.push(limit);
    let order_by = search_order_by(opts.mode, &query_param);
    let select_sql = format!(
        r"
        SELECT
            source,
            package,
            revision,
            stable_symbol_id,
            entity_name,
            qualified_name,
            file_path,
            line_start,
            line_end,
            symbol_kind,
            enclosing_scope
        FROM {nodes} n
        {select_where}
        {published_filter}
        {order_by}
        LIMIT {limit_param}
        ",
        nodes = tables.nodes,
        published_filter = tables.published_filter("n")
    );
    let candidates = collect_rows(db, &select_sql, select_params, |row| {
        code_candidate_from_row(row, 0)
    })
    .context("failed to search code symbols")?;

    Ok(CodeSearchResult {
        candidates,
        total_matches,
        truncated: total_matches > limit as usize,
    })
}

pub fn read_symbol(
    db: &Connection,
    selector: &str,
    context_lines: usize,
) -> Result<Option<SymbolSource>> {
    let tables = QueryTables::load(db)?;
    let Some(symbol) = resolve_required(db, selector)? else {
        return Ok(None);
    };

    let sql = format!(
        r"
        SELECT
            n.stable_symbol_id,
            n.source,
            n.package,
            n.revision,
            n.file_path,
            n.byte_range_start,
            n.byte_range_end,
            n.line_start,
            n.line_end,
            n.qualified_name,
            f.source_text
        FROM {nodes} n
        LEFT JOIN {files} f
          ON f.source = n.source
         AND f.package = n.package
         AND f.revision = n.revision
         AND f.file_path = n.file_path
         {files_generation_join}
        WHERE n.source = $1
          AND n.package = $2
          AND n.revision = $3
          AND n.stable_symbol_id = $4
          {published_filter}
        LIMIT 1
        ",
        nodes = tables.nodes,
        files = tables.files,
        files_generation_join = tables.same_generation_join("f", "n"),
        published_filter = tables.published_filter("n")
    );
    let row = optional_no_rows(
        db.query_row(
            &sql,
            params![
                symbol.source,
                symbol.package,
                symbol.revision,
                symbol.stable_symbol_id
            ],
            |row| {
                Ok(SymbolSourceRow {
                    stable_symbol_id: row.get(0)?,
                    package_source: row.get(1)?,
                    package: row.get(2)?,
                    revision: row.get(3)?,
                    file_path: row.get(4)?,
                    byte_start: i64_to_usize(row.get(5)?, "byte_range_start")?,
                    byte_end: i64_to_usize(row.get(6)?, "byte_range_end")?,
                    line_start: i64_to_usize(row.get(7)?, "line_start")?,
                    line_end: i64_to_usize(row.get(8)?, "line_end")?,
                    qualified_name: row.get(9)?,
                    source_text: row.get(10)?,
                })
            },
        ),
        "failed to read symbol source row",
    )?;

    let Some(row) = row else {
        return Ok(None);
    };
    let source_text = row.source_text.ok_or_else(|| {
        anyhow!(
            "source text is not indexed for {} {}@{} `{}` ({})",
            row.package_source,
            row.package,
            row.revision,
            row.file_path,
            row.stable_symbol_id
        )
    })?;
    let (source, line_range) = slice_symbol_source(
        &source_text,
        row.byte_start,
        row.byte_end,
        row.line_start,
        row.line_end,
        context_lines,
    )?;
    let selector = format!(
        "pkg:{}@{}::{}",
        row.package, row.revision, row.qualified_name
    );

    Ok(Some(SymbolSource {
        selector,
        stable_symbol_id: row.stable_symbol_id,
        package_source: row.package_source,
        package: row.package,
        revision: row.revision,
        file_path: row.file_path,
        byte_range: [row.byte_start, row.byte_end],
        line_range,
        source,
    }))
}

pub fn find_callers(
    db: &Connection,
    selector: &str,
    include_unresolved: bool,
) -> Result<CallerResult> {
    let Some(target) = resolve_required(db, selector)? else {
        return Ok(CallerResult {
            callers: Vec::new(),
            counts_by_kind: CountsByKind::default(),
            unresolved_sample: Vec::new(),
        });
    };

    let mut callers = resolved_callers(db, &target)?;
    if include_unresolved {
        callers.extend(unresolved_callers(db, &target)?);
    }
    let edges = callers.iter().map(|caller| &caller.edge);
    let counts_by_kind = counts_by_kind(edges);
    let unresolved_sample = unresolved_sample(
        callers
            .iter()
            .filter(|caller| !caller.resolved)
            .filter_map(|caller| caller.edge.target_label.as_deref()),
    );

    Ok(CallerResult {
        callers,
        counts_by_kind,
        unresolved_sample,
    })
}

pub fn find_callees(
    db: &Connection,
    selector: &str,
    include_unresolved: bool,
) -> Result<CalleeResult> {
    let Some(source) = resolve_required(db, selector)? else {
        return Ok(CalleeResult {
            callees: Vec::new(),
            counts_by_kind: CountsByKind::default(),
            unresolved_sample: Vec::new(),
        });
    };

    let mut callees = resolved_callees(db, &source)?;
    if include_unresolved {
        callees.extend(unresolved_callees(db, &source)?);
    }
    let edges = callees.iter().map(|callee| &callee.edge);
    let counts_by_kind = counts_by_kind(edges);
    let unresolved_sample = unresolved_sample(
        callees
            .iter()
            .filter(|callee| !callee.resolved)
            .filter_map(|callee| callee.edge.target_label.as_deref()),
    );

    Ok(CalleeResult {
        callees,
        counts_by_kind,
        unresolved_sample,
    })
}

pub fn resolve_selector(db: &Connection, selector: &str) -> Result<SelectorResolution> {
    if let Some(parsed) = parse_pkg_symbol_uri(selector) {
        return Ok(match stable_selector_node(db, &parsed)? {
            Some(node) => SelectorResolution::Resolved(ResolvedSymbol {
                stable_symbol_id: node.stable_symbol_id,
            }),
            None => SelectorResolution::NotFound,
        });
    }

    let Some(parsed) = parse_pkg_selector(selector) else {
        return Ok(SelectorResolution::NotFound);
    };

    let revision = match parsed.revision {
        Some(revision) => Some(revision),
        None => latest_revision(db, &parsed.source, &parsed.package)?,
    };
    let candidates = selector_candidates(
        db,
        &parsed.source,
        &parsed.package,
        revision.as_deref(),
        &parsed.name,
    )?;

    Ok(resolution_from_candidates(candidates))
}

fn resolved_callers(db: &Connection, target: &ResolvedNode) -> Result<Vec<CallerRecord>> {
    let tables = QueryTables::load(db)?;
    let sql = format!(
        r"
        SELECT
            n.source,
            n.package,
            n.revision,
            n.stable_symbol_id,
            n.entity_name,
            n.qualified_name,
            n.file_path,
            n.line_start,
            n.line_end,
            n.symbol_kind,
            n.enclosing_scope,
            e.source_stable_id,
            e.target_stable_id,
            e.target_label,
            e.target_package,
            e.relation,
            e.edge_kind,
            e.confidence,
            e.confidence_score,
            e.bind_method,
            e.receiver_text,
            e.scope_text
        FROM {edges} e
        JOIN {nodes} n
          ON n.source = e.source
         AND n.package = e.package
         AND n.revision = e.revision
         AND n.stable_symbol_id = e.source_stable_id
         {node_edge_generation_join}
        WHERE e.source = $1
          AND e.package = $2
          AND e.revision = $3
          AND e.target_stable_id = $4
          AND e.relation = 'calls'
          AND e.edge_kind IN ('calls', 'calls_dyn', 'references_hof')
          {published_filter}
        ORDER BY n.file_path, n.line_start, n.line_end, n.qualified_name, e.edge_kind
        ",
        edges = tables.edges,
        nodes = tables.nodes,
        node_edge_generation_join = tables.same_generation_join("n", "e"),
        published_filter = tables.published_filter("e")
    );
    collect_rows(
        db,
        &sql,
        SqlParams::from_values([
            target.source.clone(),
            target.package.clone(),
            target.revision.clone(),
            target.stable_symbol_id.clone(),
        ]),
        |row| {
            Ok(CallerRecord {
                caller: code_candidate_from_row(row, 0)?,
                edge: edge_metadata_from_row(row, 11)?,
                resolved: true,
            })
        },
    )
    .context("failed to query resolved callers")
}

fn unresolved_callers(db: &Connection, target: &ResolvedNode) -> Result<Vec<CallerRecord>> {
    let tables = QueryTables::load(db)?;
    let sql = format!(
        r"
        SELECT
            n.source,
            n.package,
            n.revision,
            n.stable_symbol_id,
            n.entity_name,
            n.qualified_name,
            n.file_path,
            n.line_start,
            n.line_end,
            n.symbol_kind,
            n.enclosing_scope,
            e.source_stable_id,
            NULL AS target_stable_id,
            e.target_label,
            e.target_package,
            e.relation,
            e.edge_kind,
            e.confidence,
            e.confidence_score,
            e.bind_method,
            e.receiver_text,
            e.scope_text
        FROM {edges_unresolved} e
        JOIN {nodes} n
          ON n.source = e.source
         AND n.package = e.package
         AND n.revision = e.revision
         AND n.stable_symbol_id = e.source_stable_id
         {node_edge_generation_join}
        WHERE e.source = $1
          AND e.package = $2
          AND e.revision = $3
          AND e.target_label IN ($4, $5, $6)
          AND e.relation = 'calls'
          AND e.edge_kind IN ('calls', 'calls_dyn', 'references_hof')
          {published_filter}
        ORDER BY n.file_path, n.line_start, n.line_end, n.qualified_name, e.edge_kind
        ",
        edges_unresolved = tables.edges_unresolved,
        nodes = tables.nodes,
        node_edge_generation_join = tables.same_generation_join("n", "e"),
        published_filter = tables.published_filter("e")
    );
    collect_rows(
        db,
        &sql,
        SqlParams::from_values([
            target.source.clone(),
            target.package.clone(),
            target.revision.clone(),
            target.entity_name.clone(),
            target.qualified_name.clone(),
            target.stable_symbol_id.clone(),
        ]),
        |row| {
            Ok(CallerRecord {
                caller: code_candidate_from_row(row, 0)?,
                edge: edge_metadata_from_row(row, 11)?,
                resolved: false,
            })
        },
    )
    .context("failed to query unresolved callers")
}

fn resolved_callees(db: &Connection, source: &ResolvedNode) -> Result<Vec<CalleeRecord>> {
    let tables = QueryTables::load(db)?;
    let sql = format!(
        r"
        SELECT
            n.source,
            n.package,
            n.revision,
            n.stable_symbol_id,
            n.entity_name,
            n.qualified_name,
            n.file_path,
            n.line_start,
            n.line_end,
            n.symbol_kind,
            n.enclosing_scope,
            e.source_stable_id,
            e.target_stable_id,
            e.target_label,
            e.target_package,
            e.relation,
            e.edge_kind,
            e.confidence,
            e.confidence_score,
            e.bind_method,
            e.receiver_text,
            e.scope_text
        FROM {edges} e
        JOIN {nodes} n
          ON n.source = e.source
         AND n.package = e.package
         AND n.revision = e.revision
         AND n.stable_symbol_id = e.target_stable_id
         {node_edge_generation_join}
        WHERE e.source = $1
          AND e.package = $2
          AND e.revision = $3
          AND e.source_stable_id = $4
          AND e.relation = 'calls'
          AND e.edge_kind IN ('calls', 'calls_dyn', 'references_hof')
          {published_filter}
        ORDER BY n.file_path, n.line_start, n.line_end, n.qualified_name, e.edge_kind
        ",
        edges = tables.edges,
        nodes = tables.nodes,
        node_edge_generation_join = tables.same_generation_join("n", "e"),
        published_filter = tables.published_filter("e")
    );
    collect_rows(
        db,
        &sql,
        SqlParams::from_values([
            source.source.clone(),
            source.package.clone(),
            source.revision.clone(),
            source.stable_symbol_id.clone(),
        ]),
        |row| {
            Ok(CalleeRecord {
                callee: Some(code_candidate_from_row(row, 0)?),
                edge: edge_metadata_from_row(row, 11)?,
                resolved: true,
            })
        },
    )
    .context("failed to query resolved callees")
}

fn unresolved_callees(db: &Connection, source: &ResolvedNode) -> Result<Vec<CalleeRecord>> {
    let tables = QueryTables::load(db)?;
    let sql = format!(
        r"
        SELECT
            e.source_stable_id,
            NULL AS target_stable_id,
            e.target_label,
            e.target_package,
            e.relation,
            e.edge_kind,
            e.confidence,
            e.confidence_score,
            e.bind_method,
            e.receiver_text,
            e.scope_text
        FROM {edges_unresolved} e
        WHERE e.source = $1
          AND e.package = $2
          AND e.revision = $3
          AND e.source_stable_id = $4
          AND e.relation = 'calls'
          AND e.edge_kind IN ('calls', 'calls_dyn', 'references_hof')
          {published_filter}
        ORDER BY e.target_package NULLS LAST, e.target_label, e.edge_kind
        ",
        edges_unresolved = tables.edges_unresolved,
        published_filter = tables.published_filter("e")
    );
    collect_rows(
        db,
        &sql,
        SqlParams::from_values([
            source.source.clone(),
            source.package.clone(),
            source.revision.clone(),
            source.stable_symbol_id.clone(),
        ]),
        |row| {
            Ok(CalleeRecord {
                callee: None,
                edge: edge_metadata_from_row(row, 0)?,
                resolved: false,
            })
        },
    )
    .context("failed to query unresolved callees")
}

fn selector_candidates(
    db: &Connection,
    source: &str,
    package: &str,
    revision: Option<&str>,
    name: &str,
) -> Result<Vec<CodeCandidate>> {
    let tables = QueryTables::load(db)?;
    let (revision_filter, params) = if let Some(revision) = revision {
        (
            "AND revision = $3 AND (qualified_name = $4 OR entity_name = $4)",
            SqlParams::from_values([
                source.to_owned(),
                package.to_owned(),
                revision.to_owned(),
                name.to_owned(),
            ]),
        )
    } else {
        (
            "AND (qualified_name = $3 OR entity_name = $3)",
            SqlParams::from_values([source.to_owned(), package.to_owned(), name.to_owned()]),
        )
    };
    let sql = format!(
        r"
        SELECT
            source,
            package,
            revision,
            stable_symbol_id,
            entity_name,
            qualified_name,
            file_path,
            line_start,
            line_end,
            symbol_kind,
            enclosing_scope
        FROM {nodes} n
        WHERE source = $1
          AND package = $2
          {revision_filter}
          {published_filter}
        ORDER BY file_path, line_start, line_end, qualified_name, stable_symbol_id
        ",
        nodes = tables.nodes,
        published_filter = tables.published_filter("n")
    );
    collect_rows(db, &sql, params, |row| code_candidate_from_row(row, 0))
        .context("failed to resolve external code selector")
}

fn resolution_from_candidates(candidates: Vec<CodeCandidate>) -> SelectorResolution {
    match candidates.as_slice() {
        [] => SelectorResolution::NotFound,
        [candidate] => SelectorResolution::Resolved(ResolvedSymbol {
            stable_symbol_id: candidate.stable_symbol_id.clone(),
        }),
        _ => SelectorResolution::Ambiguous { candidates },
    }
}

fn resolve_required(db: &Connection, selector: &str) -> Result<Option<ResolvedNode>> {
    if let Some(parsed) = parse_pkg_symbol_uri(selector) {
        return stable_selector_node(db, &parsed);
    }

    let Some(parsed) = parse_pkg_selector(selector) else {
        return Ok(None);
    };

    let revision = match parsed.revision {
        Some(revision) => Some(revision),
        None => latest_revision(db, &parsed.source, &parsed.package)?,
    };
    let candidates = selector_candidates(
        db,
        &parsed.source,
        &parsed.package,
        revision.as_deref(),
        &parsed.name,
    )?;

    match candidates.as_slice() {
        [] => Ok(None),
        [candidate] => Ok(Some(ResolvedNode::from(candidate))),
        _ => bail!(
            "ambiguous selector `{selector}` matched {} symbols",
            candidates.len()
        ),
    }
}

fn stable_selector_node(
    db: &Connection,
    parsed: &ParsedStableSelector,
) -> Result<Option<ResolvedNode>> {
    let tables = QueryTables::load(db)?;
    let sql = format!(
        r"
        SELECT stable_symbol_id, source, package, revision, entity_name, qualified_name
        FROM {nodes} n
        WHERE source = $1
          AND package = $2
          AND revision = $3
          AND stable_symbol_id = $4
          {published_filter}
        LIMIT 1
        ",
        nodes = tables.nodes,
        published_filter = tables.published_filter("n")
    );
    optional_no_rows(
        db.query_row(
            &sql,
            params![
                &parsed.source,
                &parsed.package,
                &parsed.revision,
                &parsed.stable_symbol_id
            ],
            |row| {
                Ok(ResolvedNode {
                    stable_symbol_id: row.get(0)?,
                    source: row.get(1)?,
                    package: row.get(2)?,
                    revision: row.get(3)?,
                    entity_name: row.get(4)?,
                    qualified_name: row.get(5)?,
                })
            },
        ),
        "failed to resolve stable external code selector",
    )
}

fn latest_revision(db: &Connection, source: &str, package: &str) -> Result<Option<String>> {
    let tables = QueryTables::load(db)?;
    if tables.refs == "refs" && !table_exists(db, "refs")? {
        return Ok(None);
    }

    let sql = format!(
        r"
        SELECT revision
        FROM {refs}
        WHERE source = $1
          AND package = $2
          AND ref_name = 'latest'
        ORDER BY updated_at DESC NULLS LAST, revision DESC
        LIMIT 1
        ",
        refs = tables.refs
    );
    optional_no_rows(
        db.query_row(
            &sql,
            params![source, package],
            |row| row.get(0),
        ),
        "failed to resolve latest package revision",
    )
}

fn table_exists(db: &Connection, table_name: &str) -> Result<bool> {
    let count: i64 = db
        .query_row(
            r"
            SELECT COUNT(*)
            FROM information_schema.tables
            WHERE table_name = $1
            ",
            params![table_name],
            |row| row.get(0),
        )
        .context("failed to inspect DuckDB tables")?;
    Ok(count > 0)
}

fn parse_pkg_selector(selector: &str) -> Option<ParsedSelector> {
    let selector = selector.trim();
    let selector = selector.strip_prefix("pkg:")?;
    let (package_revision, name) = selector.split_once("::")?;
    if package_revision.is_empty() || name.is_empty() {
        return None;
    }

    let (source, package_revision) = match package_revision.split_once('|') {
        Some((source, package_revision)) if !source.is_empty() && !package_revision.is_empty() => {
            (source.to_owned(), package_revision)
        }
        Some(_) => return None,
        None => (DEFAULT_SOURCE.to_owned(), package_revision),
    };

    let (package, revision) = match package_revision.split_once('@') {
        Some((package, revision)) if !package.is_empty() && !revision.is_empty() => {
            (package.to_owned(), Some(revision.to_owned()))
        }
        Some(_) => return None,
        None => (package_revision.to_owned(), None),
    };

    Some(ParsedSelector {
        source,
        package,
        revision,
        name: name.to_owned(),
    })
}

fn parse_pkg_symbol_uri(selector: &str) -> Option<ParsedStableSelector> {
    let body = selector.trim().strip_prefix(PKG_SYMBOL_URI_PREFIX)?;
    let mut parts = body.rsplitn(4, '/');
    let stable_symbol_id = parts.next()?;
    let revision = parts.next()?;
    let package = parts.next()?;
    let source = parts.next()?;
    if source.is_empty()
        || package.is_empty()
        || revision.is_empty()
        || stable_symbol_id.is_empty()
    {
        return None;
    }
    Some(ParsedStableSelector {
        source: source.to_owned(),
        package: package.to_owned(),
        revision: revision.to_owned(),
        stable_symbol_id: stable_symbol_id.to_owned(),
    })
}

fn search_filter(opts: &SearchOptions) -> (String, SqlParams, String) {
    let mut params = SqlParams::default();
    let source_param = params.push(opts.source.clone());
    let package_param = params.push(opts.package.clone());
    let revision_param = params.push(opts.revision.clone());
    let query_param = params.push(opts.query.clone());

    let query_predicate = match opts.mode {
        SearchMode::Exact => {
            format!("(entity_name = {query_param} OR qualified_name = {query_param})")
        }
        SearchMode::Prefix => format!(
            "(starts_with(entity_name, {query_param}) OR starts_with(qualified_name, {query_param}))"
        ),
        SearchMode::Substring => format!(
            "(contains(entity_name, {query_param}) OR contains(qualified_name, {query_param}))"
        ),
    };

    let mut filters = vec![
        format!("source = {source_param}"),
        format!("package = {package_param}"),
        format!("revision = {revision_param}"),
        query_predicate,
    ];

    if let Some(symbol_kind) = &opts.symbol_kind {
        let symbol_kind_param = params.push(symbol_kind.clone());
        filters.push(format!("symbol_kind = {symbol_kind_param}"));
    }

    if let Some(file_glob) = &opts.file_glob {
        let file_glob_param = params.push(file_glob.clone());
        filters.push(format!("file_path GLOB {file_glob_param}"));
    }

    (
        format!("WHERE {}", filters.join(" AND ")),
        params,
        query_param,
    )
}

fn search_order_by(mode: SearchMode, query_param: &str) -> String {
    match mode {
        SearchMode::Exact => format!(
            r"
            ORDER BY
                CASE WHEN entity_name = {query_param} THEN 0 ELSE 1 END,
                file_path,
                line_start,
                line_end,
                stable_symbol_id
            "
        ),
        SearchMode::Prefix => r"
            ORDER BY
                LENGTH(entity_name),
                file_path,
                line_start,
                line_end,
                stable_symbol_id
            "
        .to_owned(),
        SearchMode::Substring => format!(
            r"
            ORDER BY
                CASE
                    WHEN contains(entity_name, {query_param}) THEN strpos(entity_name, {query_param})
                    ELSE strpos(qualified_name, {query_param})
                END,
                LENGTH(entity_name),
                file_path,
                line_start,
                line_end,
                stable_symbol_id
            "
        ),
    }
}

fn code_candidate_from_row(row: &Row<'_>, offset: usize) -> duckdb::Result<CodeCandidate> {
    let source: String = row.get(offset)?;
    let package: String = row.get(offset + 1)?;
    let revision: String = row.get(offset + 2)?;
    let stable_symbol_id: String = row.get(offset + 3)?;
    let entity_name: String = row.get(offset + 4)?;
    let qualified_name: String = row.get(offset + 5)?;
    let file_path: String = row.get(offset + 6)?;
    let line_start = i64_to_usize(row.get(offset + 7)?, "line_start")?;
    let line_end = i64_to_usize(row.get(offset + 8)?, "line_end")?;
    let symbol_kind: String = row.get(offset + 9)?;
    let enclosing_scope: Option<String> = row.get(offset + 10)?;
    let selector_name = if qualified_name.is_empty() {
        entity_name.as_str()
    } else {
        qualified_name.as_str()
    };
    let selector = format!("pkg:{package}@{revision}::{selector_name}");
    let uri = format!("pkg-symbol://{source}/{package}/{revision}/{stable_symbol_id}");

    Ok(CodeCandidate {
        selector,
        uri,
        id: stable_symbol_id.clone(),
        stable_symbol_id,
        source,
        package,
        revision,
        entity_name,
        qualified_name,
        file_path,
        line_range: [line_start, line_end],
        symbol_kind,
        enclosing_scope,
    })
}

fn edge_metadata_from_row(row: &Row<'_>, offset: usize) -> duckdb::Result<EdgeMetadata> {
    Ok(EdgeMetadata {
        source_stable_id: row.get(offset)?,
        target_stable_id: row.get(offset + 1)?,
        target_label: row.get(offset + 2)?,
        target_package: row.get(offset + 3)?,
        relation: row.get(offset + 4)?,
        edge_kind: row.get(offset + 5)?,
        confidence: row.get(offset + 6)?,
        confidence_score: row.get(offset + 7)?,
        bind_method: row.get(offset + 8)?,
        receiver_text: row.get(offset + 9)?,
        scope_text: row.get(offset + 10)?,
    })
}

fn counts_by_kind<'a>(edges: impl IntoIterator<Item = &'a EdgeMetadata>) -> CountsByKind {
    let mut counts = CountsByKind::default();
    for edge in edges {
        match edge.edge_kind.as_str() {
            CALL_EDGE_KIND_CALLS => counts.calls += 1,
            CALL_EDGE_KIND_DYN => counts.calls_dyn += 1,
            CALL_EDGE_KIND_HOF => counts.references_hof += 1,
            _ => counts.references_other += 1,
        }
        if edge.target_stable_id.is_none() {
            counts.unresolved += 1;
        }
    }
    counts
}

fn unresolved_sample<'a>(labels: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut sample = Vec::new();
    let mut bytes = 0usize;

    for label in labels {
        if sample.len() >= 5 || !seen.insert(label) {
            continue;
        }
        let next_bytes = bytes + label.len();
        if next_bytes > 120 {
            break;
        }
        bytes = next_bytes;
        sample.push(label.to_owned());
    }

    sample
}

fn slice_symbol_source(
    source_text: &str,
    byte_start: usize,
    byte_end: usize,
    line_start: usize,
    line_end: usize,
    context_lines: usize,
) -> Result<(String, [usize; 2])> {
    if byte_start > byte_end || byte_end > source_text.len() {
        bail!(
            "invalid symbol byte range [{byte_start}, {byte_end}) for source text of {} bytes",
            source_text.len()
        );
    }

    if context_lines == 0 {
        let source = source_text
            .get(byte_start..byte_end)
            .ok_or_else(|| anyhow!("symbol byte range is not on UTF-8 character boundaries"))?
            .to_owned();
        return Ok((source, [line_start, line_end]));
    }

    let line_starts = line_starts(source_text);
    if line_starts.is_empty() {
        return Ok((String::new(), [0, 0]));
    }

    let expanded_start = line_start.saturating_sub(context_lines).max(1);
    let expanded_end = (line_end + context_lines).min(line_starts.len());
    let start_byte = line_starts[expanded_start - 1];
    let end_byte = if expanded_end < line_starts.len() {
        line_starts[expanded_end]
    } else {
        source_text.len()
    };
    let source = source_text
        .get(start_byte..end_byte)
        .ok_or_else(|| anyhow!("expanded line range is not on UTF-8 character boundaries"))?
        .to_owned();
    Ok((source, [expanded_start, expanded_end]))
}

fn line_starts(source_text: &str) -> Vec<usize> {
    if source_text.is_empty() {
        return Vec::new();
    }

    let mut starts = vec![0];
    for (index, byte) in source_text.bytes().enumerate() {
        if byte == b'\n' && index + 1 < source_text.len() {
            starts.push(index + 1);
        }
    }
    starts
}

fn query_count(db: &Connection, sql: &str, params: SqlParams) -> Result<usize> {
    let mut stmt = db.prepare(sql).context("failed to prepare count query")?;
    let param_refs = params.refs();
    let count: i64 = stmt
        .query_row(param_refs.as_slice(), |row| row.get(0))
        .context("failed to execute count query")?;
    usize::try_from(count).context("count did not fit usize")
}

fn collect_rows<T, F>(db: &Connection, sql: &str, params: SqlParams, mut map: F) -> Result<Vec<T>>
where
    F: for<'r> FnMut(&Row<'r>) -> duckdb::Result<T>,
{
    let mut stmt = db.prepare(sql).context("failed to prepare query")?;
    let param_refs = params.refs();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| map(row))
        .context("failed to execute query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read query rows")
}

fn optional_no_rows<T>(result: duckdb::Result<T>, context: &'static str) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context(context),
    }
}

fn i64_to_usize(value: i64, field: &'static str) -> duckdb::Result<usize> {
    usize::try_from(value).map_err(|error| {
        duckdb::Error::FromSqlConversionFailure(
            0,
            duckdb::types::Type::BigInt,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{field} value {value} did not fit usize: {error}"),
            )),
        )
    })
}

impl From<&CodeCandidate> for ResolvedNode {
    fn from(candidate: &CodeCandidate) -> Self {
        Self {
            stable_symbol_id: candidate.stable_symbol_id.clone(),
            source: candidate.source.clone(),
            package: candidate.package.clone(),
            revision: candidate.revision.clone(),
            entity_name: candidate.entity_name.clone(),
            qualified_name: candidate.qualified_name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedNode {
    stable_symbol_id: String,
    source: String,
    package: String,
    revision: String,
    entity_name: String,
    qualified_name: String,
}

#[derive(Debug)]
struct ParsedSelector {
    source: String,
    package: String,
    revision: Option<String>,
    name: String,
}

#[derive(Debug)]
struct ParsedStableSelector {
    source: String,
    package: String,
    revision: String,
    stable_symbol_id: String,
}

#[derive(Debug)]
struct SymbolSourceRow {
    stable_symbol_id: String,
    package_source: String,
    package: String,
    revision: String,
    file_path: String,
    byte_start: usize,
    byte_end: usize,
    line_start: usize,
    line_end: usize,
    qualified_name: String,
    source_text: Option<String>,
}

#[derive(Default)]
struct SqlParams {
    values: Vec<Box<dyn ToSql>>,
}

impl SqlParams {
    fn from_values<T, const N: usize>(values: [T; N]) -> Self
    where
        T: ToSql + 'static,
    {
        Self {
            values: values
                .into_iter()
                .map(|value| Box::new(value) as Box<dyn ToSql>)
                .collect(),
        }
    }

    fn push<T>(&mut self, value: T) -> String
    where
        T: ToSql + 'static,
    {
        self.values.push(Box::new(value));
        format!("${}", self.values.len())
    }

    fn refs(&self) -> Vec<&dyn ToSql> {
        self.values
            .iter()
            .map(|value| value.as_ref() as &dyn ToSql)
            .collect()
    }
}
