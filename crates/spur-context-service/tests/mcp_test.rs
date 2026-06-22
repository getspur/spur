use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use serde_json::{json, Value};
use spur_context_service::catalog::CatalogResolver;
use spur_context_service::mcp::{handle_tool, tool_definitions, McpHandlerError};

const PACKAGE: &str = "demo";
const REVISION: &str = "1.0.0";
const DIMENSIONS: usize = 768;

#[test]
fn tool_definitions_match_external_context_surface() {
    let definitions = tool_definitions();
    let names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        [
            "external_code_search",
            "external_code_read",
            "external_code_callers",
            "external_code_callees",
            "external_knowledge_context",
        ]
    );

    let search_schema = schema_for(&definitions, "external_code_search");
    assert_eq!(required(search_schema), ["query", "package"]);
    assert!(search_schema["properties"]["source"]["default"].is_string());
    assert_eq!(search_schema["properties"]["limit"]["maximum"], 200);

    let read_schema = schema_for(&definitions, "external_code_read");
    assert_eq!(required(read_schema), ["selector"]);

    let callers_schema = schema_for(&definitions, "external_code_callers");
    assert_eq!(required(callers_schema), ["selector"]);
    assert_eq!(
        callers_schema["properties"]["include_unresolved"]["default"],
        false
    );

    let callees_schema = schema_for(&definitions, "external_code_callees");
    assert_eq!(required(callees_schema), ["selector"]);

    let knowledge_schema = schema_for(&definitions, "external_knowledge_context");
    assert_eq!(required(knowledge_schema), ["query", "package"]);
    assert_eq!(
        knowledge_schema["properties"]["scope"]["enum"],
        json!(["code", "docs", "all"])
    );
    assert_eq!(knowledge_schema["properties"]["limit"]["default"], 8);
}

#[tokio::test]
async fn external_code_search_resolves_latest_and_returns_candidates() -> Result<()> {
    let fixture = McpFixture::new("search")?;

    let response = handle_tool(
        "external_code_search",
        &json!({
            "query": "bet",
            "package": PACKAGE,
            "symbol_kind": "function",
            "limit": 20
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(response["total_matches"], 1);
    assert_eq!(response["truncated"], false);
    assert_eq!(
        response["candidates"][0]["stable_symbol_id"],
        "bbbbbbbbbbbbbbbb"
    );
    assert_eq!(response["candidates"][0]["revision"], REVISION);
    Ok(())
}

#[tokio::test]
async fn external_code_read_resolves_selector_ref_and_returns_source() -> Result<()> {
    let fixture = McpFixture::new("read")?;

    let response = handle_tool(
        "external_code_read",
        &json!({
            "selector": "pkg:demo@latest::demo::beta",
            "context_lines": 0
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(response["stable_symbol_id"], "bbbbbbbbbbbbbbbb");
    assert_eq!(response["file_path"], "src/lib.rs");
    assert_eq!(response["line_range"], json!([6, 7]));
    assert_eq!(response["source"], "pub fn beta() {\n}\n");
    Ok(())
}

#[tokio::test]
async fn external_code_callers_returns_resolved_and_unresolved_edges() -> Result<()> {
    let fixture = McpFixture::new("callers")?;

    let response = handle_tool(
        "external_code_callers",
        &json!({
            "selector": "pkg:demo@latest::demo::beta",
            "include_unresolved": true
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(
        response["callers"]
            .as_array()
            .context("callers array")?
            .len(),
        3
    );
    assert_eq!(response["counts_by_kind"]["calls"], 1);
    assert_eq!(response["counts_by_kind"]["calls_dyn"], 1);
    assert_eq!(response["counts_by_kind"]["references_hof"], 1);
    assert_eq!(response["counts_by_kind"]["unresolved"], 1);
    assert_eq!(response["unresolved_sample"], json!(["demo::beta"]));
    Ok(())
}

#[tokio::test]
async fn external_code_callees_returns_resolved_and_unresolved_edges() -> Result<()> {
    let fixture = McpFixture::new("callees")?;

    let response = handle_tool(
        "external_code_callees",
        &json!({
            "selector": "pkg:demo@latest::demo::alpha",
            "include_unresolved": true
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(
        response["callees"]
            .as_array()
            .context("callees array")?
            .len(),
        2
    );
    assert_eq!(
        response["callees"][0]["callee"]["stable_symbol_id"],
        "bbbbbbbbbbbbbbbb"
    );
    assert_eq!(
        response["callees"][1]["edge"]["target_label"],
        "external::Thing"
    );
    assert_eq!(response["counts_by_kind"]["calls"], 2);
    assert_eq!(response["counts_by_kind"]["unresolved"], 1);
    Ok(())
}

#[tokio::test]
async fn external_knowledge_context_resolves_ref_and_returns_evidence_pack() -> Result<()> {
    let fixture = McpFixture::new("knowledge")?;

    let response = handle_tool(
        "external_knowledge_context",
        &json!({
            "query": "parse config loader",
            "package": PACKAGE,
            "ref": "latest",
            "scope": "all",
            "limit": 8
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(response["answerable"], true);
    assert_eq!(response["graph_content_hash"], "fixture-hash");
    assert!(response["primary_evidence"]
        .as_array()
        .context("primary_evidence array")?
        .iter()
        .any(|evidence| {
            evidence["stable_symbol_id"] == "pkg:demo@1.0.0::demo::parse_config_loader"
        }));
    assert!(response["supporting_docs"]
        .as_array()
        .context("supporting_docs array")?
        .iter()
        .any(|evidence| evidence["stable_symbol_id"] == "doc-parse"));
    assert!(response["confidence"].is_string());
    Ok(())
}

#[tokio::test]
async fn external_knowledge_context_uses_precomputed_query_vector_for_hybrid_hits() -> Result<()> {
    let fixture = McpFixture::new("knowledge-vector")?;

    let response = handle_tool(
        "external_knowledge_context",
        &json!({
            "query": "unmatched lexical query",
            "package": PACKAGE,
            "ref": "latest",
            "scope": "code",
            "limit": 3,
            "query_vec": unit_vector(0)
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    let top = response["primary_evidence"][0]
        .as_object()
        .context("expected vector primary evidence")?;
    assert_eq!(
        top["stable_symbol_id"],
        "pkg:demo@1.0.0::demo::runtime::task_spawner"
    );
    assert_eq!(top["grounding"], "hybrid-code");
    assert!(response["supporting_docs"]
        .as_array()
        .context("supporting_docs array")?
        .is_empty());
    Ok(())
}

#[tokio::test]
async fn handler_reports_unknown_tool_missing_args_and_missing_package() -> Result<()> {
    let fixture = McpFixture::new("errors")?;

    let unknown = handle_tool("missing_tool", &json!({}), &fixture.conn, &fixture.catalog)
        .await
        .expect_err("unknown tool should fail");
    assert!(matches!(unknown, McpHandlerError::InvalidParams(_)));

    let missing_args = handle_tool(
        "external_code_search",
        &json!({ "package": PACKAGE }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await
    .expect_err("missing query should fail");
    assert!(matches!(missing_args, McpHandlerError::InvalidParams(_)));

    let missing_package = handle_tool(
        "external_code_search",
        &json!({ "query": "beta", "package": "missing" }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await
    .expect_err("missing package should fail");
    assert!(matches!(missing_package, McpHandlerError::NotFound(_)));
    Ok(())
}

fn schema_for<'a>(
    definitions: &'a [spur_context_service::mcp::ToolDefinition],
    name: &str,
) -> &'a Value {
    &definitions
        .iter()
        .find(|definition| definition.name == name)
        .with_context(|| format!("missing tool definition {name}"))
        .unwrap()
        .input_schema
}

fn required(schema: &Value) -> Vec<&str> {
    schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

struct McpFixture {
    conn: Connection,
    catalog: CatalogResolver,
    _root: PathBuf,
}

impl McpFixture {
    fn new(name: &str) -> Result<Self> {
        let conn = Connection::open_in_memory().context("open query duckdb")?;
        create_query_schema(&conn)?;
        seed_query_fixture(&conn)?;

        let root = unique_temp_dir(name)?;
        fs::create_dir_all(root.join("data")).context("create catalog data dir")?;
        let catalog_path = root.join("catalog.sqlite");
        let data_path = root.join("data");
        let catalog_dsn = format!("sqlite:{}", catalog_path.display());
        let data_path = data_path.display().to_string();
        initialize_catalog(&catalog_dsn, &data_path)?;
        let catalog = CatalogResolver::new_with_data_path(&catalog_dsn, &data_path)?;

        Ok(Self {
            conn,
            catalog,
            _root: root,
        })
    }
}

fn create_query_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('fixture-hash');

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

        CREATE TABLE section_bodies (
            section_id VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            file_path VARCHAR,
            title VARCHAR,
            body_text VARCHAR,
            body_hash VARCHAR,
            token_count INTEGER
        );

        CREATE TABLE symbol_embeddings (
            stable_symbol_id VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR,
            embedding FLOAT[],
            embedding_model VARCHAR,
            embedding_input_hash VARCHAR,
            embed_text_version VARCHAR
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

fn seed_query_fixture(conn: &Connection) -> Result<()> {
    let source_text = concat!(
        "pub fn alpha() {\n",
        "    beta();\n",
        "    external::Thing::new();\n",
        "}\n",
        "\n",
        "pub fn beta() {\n",
        "}\n",
        "\n",
        "pub fn parse_config_loader() {}\n",
        "\n",
        "pub fn caller() {\n",
        "    beta();\n",
        "}\n",
        "\n",
        "pub fn dynamic_caller() {}\n",
        "\n",
        "pub fn hof_caller() {}\n",
        "\n",
        "pub fn task_spawner() {}\n",
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
        "parse_config_loader",
        "demo::parse_config_loader",
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
    insert_node(
        conn,
        "9999999999999999",
        source_text,
        "task_spawner",
        "demo::runtime::task_spawner",
        19,
        19,
    )?;

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
        INSERT INTO section_bodies VALUES
            ('doc-parse', 'demo', 'registry:crates-io', '1.0.0', 'semver',
             1, 0, 0, 'docs/parser.md', 'Parser Guide',
             'The parse config loader reads config documents and validates loader inputs.',
             'hash-doc-parse', 9)
        ",
        [],
    )
    .context("insert section body")?;

    conn.execute(
        r"
        INSERT INTO refs VALUES
            ('registry:crates-io', 'demo', 'latest', '1.0.0',
             TIMESTAMP '2026-06-22 00:00:00')
        ",
        [],
    )
    .context("insert latest ref")?;

    insert_embedding(
        conn,
        "9999999999999999",
        "task_spawner",
        "demo::runtime::task_spawner",
        unit_vector(0),
    )?;
    insert_embedding(
        conn,
        "cccccccccccccccc",
        "parse_config_loader",
        "demo::parse_config_loader",
        unit_vector(1),
    )?;
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

fn insert_embedding(
    conn: &Connection,
    id: &str,
    entity_name: &str,
    qualified_name: &str,
    vector: Vec<f32>,
) -> Result<()> {
    let sql = format!(
        r"
        INSERT INTO symbol_embeddings VALUES
            ('{id}', 'demo', 'registry:crates-io', '1.0.0', 'semver', 1, 0, 0,
             'src/lib.rs', '{entity_name}', '{qualified_name}', 'function',
             {}, 'JinaEmbeddingsV2BaseCode', 'hash-{id}', 'v2-jina-code')
        ",
        vector_sql(&vector)
    );
    conn.execute_batch(&sql)
        .with_context(|| format!("insert embedding {id}"))?;
    Ok(())
}

fn unit_vector(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS];
    vector[index] = 1.0;
    vector
}

fn vector_sql(vector: &[f32]) -> String {
    let values = vector
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{values}]::FLOAT[768]")
}

fn initialize_catalog(catalog_dsn: &str, data_path: &str) -> Result<()> {
    let conn = Connection::open_in_memory().context("open catalog duckdb")?;
    attach_ducklake(&conn, catalog_dsn, data_path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE package_catalog (
            source VARCHAR,
            package VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            snapshot_id BIGINT,
            indexed_at TIMESTAMP,
            index_status VARCHAR,
            embeddings_status VARCHAR,
            row_counts JSON
        );
        ALTER TABLE package_catalog SET PARTITIONED BY (source, package);

        CREATE TABLE refs (
            source VARCHAR,
            package VARCHAR,
            ref_name VARCHAR,
            revision VARCHAR,
            updated_at TIMESTAMP
        );
        ALTER TABLE refs SET PARTITIONED BY (source, package);

        INSERT INTO package_catalog VALUES
            ('registry:crates-io', 'demo', '1.0.0', 'semver',
             1, 0, 0, 100, TIMESTAMP '2026-06-22 00:00:00',
             'complete', 'complete', '{"nodes": 6}');

        INSERT INTO refs VALUES
            ('registry:crates-io', 'demo', 'latest', '1.0.0',
             TIMESTAMP '2026-06-22 00:05:00');
        "#,
    )
    .context("seed catalog fixture")?;
    Ok(())
}

fn attach_ducklake(conn: &Connection, catalog_dsn: &str, data_path: &str) -> Result<()> {
    let catalog_dsn = escape_sql_literal(catalog_dsn);
    let data_path = escape_sql_literal(data_path);
    conn.execute_batch("INSTALL ducklake; INSTALL sqlite; LOAD ducklake; LOAD sqlite;")
        .context("load ducklake/sqlite extensions")?;
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{catalog_dsn}' AS spur_context (DATA_PATH '{data_path}'); USE spur_context;"
    ))
    .context("attach ducklake")
}

fn unique_temp_dir(name: &str) -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_nanos();
    path.push(format!(
        "spur-context-service-mcp-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).with_context(|| format!("create temp dir {}", path.display()))?;
    Ok(path)
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}
