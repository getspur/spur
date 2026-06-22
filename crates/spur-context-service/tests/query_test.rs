use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use spur_context_service::query::{
    find_callees, find_callers, read_symbol, resolve_selector, search_symbols, SearchMode,
    SearchOptions, SelectorResolution,
};

const SOURCE: &str = "registry:crates-io";
const PACKAGE: &str = "demo";
const REVISION: &str = "1.0.0";

#[test]
fn search_supports_exact_prefix_substring_filters_and_truncation() -> Result<()> {
    let fixture = QueryFixture::new()?;

    let exact = search_symbols(
        &fixture.conn,
        &search_options("beta", SearchMode::Exact, 20),
    )?;
    assert_eq!(exact.total_matches, 1);
    assert!(!exact.truncated);
    assert_eq!(exact.candidates[0].stable_symbol_id, "bbbbbbbbbbbbbbbb");

    let prefix = search_symbols(
        &fixture.conn,
        &search_options("alp", SearchMode::Prefix, 20),
    )?;
    assert_eq!(
        prefix
            .candidates
            .iter()
            .map(|candidate| candidate.entity_name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "alphabet"]
    );

    let mut substring_opts = search_options("pha", SearchMode::Substring, 1);
    substring_opts.file_glob = Some("src/*.rs".to_owned());
    let substring = search_symbols(&fixture.conn, &substring_opts)?;
    assert_eq!(substring.total_matches, 2);
    assert!(substring.truncated);
    assert_eq!(substring.candidates.len(), 1);
    assert_eq!(substring.candidates[0].entity_name, "alpha");
    Ok(())
}

#[test]
fn read_symbol_returns_exact_source_byte_range() -> Result<()> {
    let fixture = QueryFixture::new()?;

    let source = read_symbol(&fixture.conn, "pkg:demo@1.0.0::demo::beta", 0)?
        .context("expected beta source")?;

    assert_eq!(source.stable_symbol_id, "bbbbbbbbbbbbbbbb");
    assert_eq!(source.file_path, "src/lib.rs");
    assert_eq!(source.line_range, [6, 7]);
    assert_eq!(source.source, "pub fn beta() {\n}\n");
    Ok(())
}

#[test]
fn find_callers_returns_resolved_and_unresolved_edges() -> Result<()> {
    let fixture = QueryFixture::new()?;

    let callers = find_callers(&fixture.conn, "pkg:demo@1.0.0::demo::beta", true)?;

    let caller_ids = callers
        .callers
        .iter()
        .map(|caller| caller.caller.stable_symbol_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        caller_ids,
        ["aaaaaaaaaaaaaaaa", "dddddddddddddddd", "eeeeeeeeeeeeeeee"]
    );
    assert_eq!(callers.counts_by_kind.calls, 1);
    assert_eq!(callers.counts_by_kind.calls_dyn, 1);
    assert_eq!(callers.counts_by_kind.references_hof, 1);
    assert_eq!(callers.counts_by_kind.references_other, 0);
    assert_eq!(callers.counts_by_kind.unresolved, 1);
    assert_eq!(callers.unresolved_sample, ["demo::beta"]);
    Ok(())
}

#[test]
fn find_callees_returns_resolved_and_unresolved_edges() -> Result<()> {
    let fixture = QueryFixture::new()?;

    let callees = find_callees(&fixture.conn, "pkg:demo@1.0.0::demo::alpha", true)?;

    assert_eq!(callees.callees.len(), 2);
    assert_eq!(
        callees.callees[0]
            .callee
            .as_ref()
            .map(|candidate| candidate.stable_symbol_id.as_str()),
        Some("bbbbbbbbbbbbbbbb")
    );
    assert_eq!(callees.callees[1].callee, None);
    assert_eq!(
        callees.callees[1].edge.target_label.as_deref(),
        Some("external::Thing")
    );
    assert_eq!(callees.counts_by_kind.calls, 2);
    assert_eq!(callees.counts_by_kind.unresolved, 1);
    assert_eq!(callees.unresolved_sample, ["external::Thing"]);
    Ok(())
}

#[test]
fn resolve_selector_handles_exact_latest_ambiguous_and_missing() -> Result<()> {
    let fixture = QueryFixture::new()?;

    let exact = resolve_selector(&fixture.conn, "pkg:demo@1.0.0::demo::beta")?;
    assert!(matches!(
        exact,
        SelectorResolution::Resolved(symbol) if symbol.stable_symbol_id == "bbbbbbbbbbbbbbbb"
    ));

    let latest = resolve_selector(&fixture.conn, "pkg:demo::demo::beta")?;
    assert!(matches!(
        latest,
        SelectorResolution::Resolved(symbol) if symbol.stable_symbol_id == "bbbbbbbbbbbbbbbb"
    ));

    let ambiguous = resolve_selector(&fixture.conn, "pkg:demo@1.0.0::dup")?;
    assert!(matches!(
        ambiguous,
        SelectorResolution::Ambiguous { ref candidates } if candidates.len() == 2
    ));

    assert_eq!(
        resolve_selector(&fixture.conn, "pkg:demo@1.0.0::missing")?,
        SelectorResolution::NotFound
    );
    Ok(())
}

struct QueryFixture {
    conn: Connection,
}

impl QueryFixture {
    fn new() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
        create_schema(&conn)?;
        seed_fixture(&conn)?;
        Ok(Self { conn })
    }
}

fn search_options(query: &str, mode: SearchMode, limit: usize) -> SearchOptions {
    SearchOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        query: query.to_owned(),
        mode,
        symbol_kind: Some("function".to_owned()),
        file_glob: None,
        limit,
    }
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            file_path VARCHAR,
            byte_range_start INTEGER,
            byte_range_end INTEGER,
            line_start INTEGER,
            line_end INTEGER,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR,
            anchor_hash VARCHAR,
            enclosing_scope VARCHAR
        );

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            target_package VARCHAR,
            target_label VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            relation VARCHAR,
            edge_kind VARCHAR,
            confidence VARCHAR,
            confidence_score DOUBLE,
            bind_method VARCHAR,
            receiver_text VARCHAR,
            scope_text VARCHAR
        );

        CREATE TABLE edges_unresolved (
            source_stable_id VARCHAR,
            target_label VARCHAR,
            target_package VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            relation VARCHAR,
            edge_kind VARCHAR,
            confidence VARCHAR,
            confidence_score DOUBLE,
            bind_method VARCHAR,
            receiver_text VARCHAR,
            scope_text VARCHAR
        );

        CREATE TABLE files (
            stable_file_id VARCHAR,
            file_path VARCHAR,
            source_text VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER
        );

        CREATE TABLE refs (
            source VARCHAR,
            package VARCHAR,
            ref_name VARCHAR,
            revision VARCHAR,
            updated_at TIMESTAMP
        );
        ",
    )
    .context("create query schema")?;
    Ok(())
}

fn seed_fixture(conn: &Connection) -> Result<()> {
    let source_text = concat!(
        "pub fn alpha() {\n",
        "    beta();\n",
        "    external::Thing::new();\n",
        "}\n",
        "\n",
        "pub fn beta() {\n",
        "}\n",
        "\n",
        "pub fn alphabet() {}\n",
        "\n",
        "pub fn caller() {\n",
        "    beta();\n",
        "}\n",
        "\n",
        "pub fn dynamic_caller() {}\n",
        "\n",
        "pub fn hof_caller() {}\n",
    );
    conn.execute(
        r"
        INSERT INTO files VALUES
            ('file-lib', 'src/lib.rs', $1, 'demo', 'registry:crates-io', '1.0.0',
             'semver', 1, 0, 0)
        ",
        params![source_text],
    )
    .context("insert file")?;

    insert_node(
        conn,
        "aaaaaaaaaaaaaaaa",
        source_text,
        "alpha",
        "demo::alpha",
        1,
        4,
    )?;
    insert_node(
        conn,
        "bbbbbbbbbbbbbbbb",
        source_text,
        "beta",
        "demo::beta",
        6,
        7,
    )?;
    insert_node(
        conn,
        "cccccccccccccccc",
        source_text,
        "alphabet",
        "demo::alphabet",
        9,
        9,
    )?;
    insert_node(
        conn,
        "dddddddddddddddd",
        source_text,
        "caller",
        "demo::caller",
        11,
        13,
    )?;
    insert_node(
        conn,
        "eeeeeeeeeeeeeeee",
        source_text,
        "dynamic_caller",
        "demo::dynamic_caller",
        15,
        15,
    )?;
    insert_node(
        conn,
        "ffffffffffffffff",
        source_text,
        "hof_caller",
        "demo::hof_caller",
        17,
        17,
    )?;

    insert_synthetic_node(conn, "1111111111111111", "src/a.rs", "dup", "a::dup")?;
    insert_synthetic_node(conn, "2222222222222222", "src/b.rs", "dup", "b::dup")?;
    insert_old_revision_node(conn)?;

    insert_edge(
        conn,
        "aaaaaaaaaaaaaaaa",
        Some("bbbbbbbbbbbbbbbb"),
        None,
        None,
        "calls",
        "calls",
    )?;
    insert_edge(
        conn,
        "dddddddddddddddd",
        Some("bbbbbbbbbbbbbbbb"),
        None,
        None,
        "calls",
        "references_hof",
    )?;
    insert_edge(
        conn,
        "ffffffffffffffff",
        Some("bbbbbbbbbbbbbbbb"),
        None,
        None,
        "calls",
        "references_other",
    )?;
    insert_edge(
        conn,
        "aaaaaaaaaaaaaaaa",
        Some("bbbbbbbbbbbbbbbb"),
        None,
        None,
        "contains",
        "calls",
    )?;
    insert_unresolved_edge(
        conn,
        "eeeeeeeeeeeeeeee",
        "demo::beta",
        None,
        "calls",
        "calls_dyn",
    )?;
    insert_unresolved_edge(
        conn,
        "aaaaaaaaaaaaaaaa",
        "external::Thing",
        Some("external"),
        "calls",
        "calls",
    )?;

    conn.execute(
        r"
        INSERT INTO refs VALUES
            ('registry:crates-io', 'demo', 'latest', '1.0.0',
             TIMESTAMP '2026-06-22 00:00:00')
        ",
        [],
    )
    .context("insert latest ref")?;
    Ok(())
}

fn insert_node(
    conn: &Connection,
    id: &str,
    source_text: &str,
    entity_name: &str,
    qualified_name: &str,
    line_start: i64,
    line_end: i64,
) -> Result<()> {
    let marker = format!("pub fn {entity_name}");
    let byte_start = source_text
        .find(&marker)
        .with_context(|| format!("find marker {marker}"))? as i64;
    let byte_end = source_text[byte_start as usize..]
        .find("\n\n")
        .map(|offset| byte_start + offset as i64 + 1)
        .unwrap_or(source_text.len() as i64);
    conn.execute(
        r"
        INSERT INTO nodes VALUES
            ($1, 'demo', 'registry:crates-io', '1.0.0', 'semver', 1, 0, 0,
             'src/lib.rs', $2, $3, $4, $5, $6, $7, 'function', $8, NULL)
        ",
        params![
            id,
            byte_start,
            byte_end,
            line_start,
            line_end,
            entity_name,
            qualified_name,
            format!("anchor-{id}")
        ],
    )
    .with_context(|| format!("insert node {qualified_name}"))?;
    Ok(())
}

fn insert_synthetic_node(
    conn: &Connection,
    id: &str,
    file_path: &str,
    entity_name: &str,
    qualified_name: &str,
) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO nodes VALUES
            ($1, 'demo', 'registry:crates-io', '1.0.0', 'semver', 1, 0, 0,
             $2, 0, 1, 1, 1, $3, $4, 'function', $5, NULL)
        ",
        params![
            id,
            file_path,
            entity_name,
            qualified_name,
            format!("anchor-{id}")
        ],
    )
    .with_context(|| format!("insert synthetic node {qualified_name}"))?;
    Ok(())
}

fn insert_old_revision_node(conn: &Connection) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO nodes VALUES
            ('9999999999999999', 'demo', 'registry:crates-io', '0.9.0', 'semver',
             0, 9, 0, 'src/lib.rs', 0, 1, 1, 1, 'beta', 'demo::beta',
             'function', 'anchor-old', NULL)
        ",
        [],
    )
    .context("insert old revision node")?;
    Ok(())
}

fn insert_edge(
    conn: &Connection,
    source_id: &str,
    target_id: Option<&str>,
    target_package: Option<&str>,
    target_label: Option<&str>,
    relation: &str,
    edge_kind: &str,
) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO edges VALUES
            ($1, $2, $3, $4, 'demo', 'registry:crates-io', '1.0.0',
             'semver', 1, 0, 0, $5, $6, 'syntax_exact', 0.99,
             'singleton', NULL, NULL)
        ",
        params![
            source_id,
            target_id,
            target_package,
            target_label,
            relation,
            edge_kind
        ],
    )
    .context("insert resolved edge")?;
    Ok(())
}

fn insert_unresolved_edge(
    conn: &Connection,
    source_id: &str,
    target_label: &str,
    target_package: Option<&str>,
    relation: &str,
    edge_kind: &str,
) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO edges_unresolved VALUES
            ($1, $2, $3, 'demo', 'registry:crates-io', '1.0.0',
             'semver', 1, 0, 0, $4, $5, 'heuristic', 0.50,
             'label', NULL, NULL)
        ",
        params![source_id, target_label, target_package, relation, edge_kind],
    )
    .context("insert unresolved edge")?;
    Ok(())
}
