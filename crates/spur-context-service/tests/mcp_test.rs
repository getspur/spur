use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use duckdb::{params, Connection};
use serde_json::{json, Value};
use spur_context_service::catalog::{
    compact_gold_and_export_snapshot, connect_frozen_snapshot, CatalogResolver,
    FrozenSnapshotManifest, SnapshotCleanupOptions,
};
use spur_context_service::jobs::{
    CreateJobOutcome, CreateJobRequest, JobKey, JobRecord, JobStatus, JobStore, JobsError,
};
use spur_context_service::mcp::{
    handle_tool, handle_tool_without_catalog, route_index, route_index_status,
    route_index_status_for_caller, route_index_without_catalog, tool_definitions, ExecutionOutcome,
    ExecutionOutcomeStatus, ExecutionStatusChecker, IndexExecutionRequest, IndexExecutionStarter,
    McpHandlerError,
};

const PACKAGE: &str = "demo";
const REVISION: &str = "1.0.0";
const SOURCE_URL: &str = "https://1.1.1.1/example/demo";
const GIT_SOURCE: &str = "git:github.com/example/demo";
const DIMENSIONS: usize = 768;
const EMBEDDING_MODEL: &str = "EmbeddingGemma300M";
const EMBED_TEXT_VERSION: &str = "v4-embeddinggemma-300m-titled";

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
            "external_index",
            "external_index_status",
        ]
    );

    let search_schema = schema_for(&definitions, "external_code_search");
    assert_eq!(required(search_schema), ["query", "package"]);
    assert!(search_schema["properties"]["source"]["default"].is_string());
    assert_eq!(search_schema["properties"]["limit"]["maximum"], 200);

    let read_schema = schema_for(&definitions, "external_code_read");
    assert_eq!(required(read_schema), ["selector"]);
    assert!(read_schema["properties"]["source"]["default"].is_string());

    let callers_schema = schema_for(&definitions, "external_code_callers");
    assert_eq!(required(callers_schema), ["selector"]);
    assert!(callers_schema["properties"]["source"]["default"].is_string());
    assert_eq!(
        callers_schema["properties"]["include_unresolved"]["default"],
        false
    );

    let callees_schema = schema_for(&definitions, "external_code_callees");
    assert_eq!(required(callees_schema), ["selector"]);
    assert!(callees_schema["properties"]["source"]["default"].is_string());

    let knowledge_schema = schema_for(&definitions, "external_knowledge_context");
    assert_eq!(required(knowledge_schema), ["query", "package"]);
    assert_eq!(
        knowledge_schema["properties"]["scope"]["enum"],
        json!(["code", "docs", "all"])
    );
    assert_eq!(knowledge_schema["properties"]["limit"]["default"], 8);

    let index_schema = schema_for(&definitions, "external_index");
    assert_eq!(
        required(index_schema),
        ["package", "revision", "source_url"]
    );
    assert_eq!(
        index_schema["properties"]["source"]["default"],
        "git:custom"
    );
    assert_eq!(
        index_schema["properties"]["source_kind"]["enum"],
        json!(["git", "tarball"])
    );
    assert_eq!(index_schema["properties"]["force"]["default"], false);

    let status_schema = schema_for(&definitions, "external_index_status");
    assert_eq!(required(status_schema), ["job_id"]);
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
async fn serving_uses_frozen_snapshot_when_postgres_catalog_is_unreachable() -> Result<()> {
    let fixture = FrozenServingFixture::new("serving-frozen-snapshot")?;
    let snapshot_conn = connect_frozen_snapshot(&fixture.snapshot_path, &fixture.data_path)
        .context("serving should attach the frozen snapshot without a live catalog backend")?;
    let catalog = CatalogResolver::from_connection(snapshot_conn);

    let response = handle_tool(
        "external_code_search",
        &json!({
            "query": "bet",
            "package": PACKAGE,
            "limit": 20
        }),
        catalog.connection(),
        &catalog,
    )
    .await?;

    assert_eq!(response["total_matches"], 1);
    assert_eq!(
        response["candidates"][0]["stable_symbol_id"],
        "bbbbbbbbbbbbbbbb"
    );
    assert_eq!(response["candidates"][0]["revision"], REVISION);

    let source = handle_tool(
        "external_code_read",
        &json!({
            "selector": "pkg:demo@latest::demo::beta",
            "context_lines": 0
        }),
        catalog.connection(),
        &catalog,
    )
    .await?;

    assert_eq!(source["file_path"], "src/lib.rs");
    assert_eq!(source["source"], "pub fn beta() {\n}\n");

    let write_error = catalog
        .connection()
        .execute(
            "INSERT INTO gold.refs VALUES ('registry:crates-io', 'demo', 'mutable', '1.0.0', CURRENT_TIMESTAMP)",
            [],
        )
        .expect_err("serving must attach the frozen snapshot read-only");
    assert!(
        write_error.to_string().contains("read-only")
            || write_error.to_string().contains("read only"),
        "unexpected write error: {write_error}"
    );

    let _postgres_catalog_that_must_not_be_used =
        "ducklake:postgresql://127.0.0.1:1/spur_context?connect_timeout=1";
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
async fn external_code_read_resolves_non_default_source() -> Result<()> {
    let fixture = McpFixture::new("read-git-source")?;
    move_fixture_to_source(&fixture, GIT_SOURCE)?;

    let response = handle_tool(
        "external_code_read",
        &json!({
            "source": GIT_SOURCE,
            "selector": "pkg:demo@latest::demo::beta",
            "context_lines": 0
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(response["stable_symbol_id"], "bbbbbbbbbbbbbbbb");
    assert_eq!(response["package_source"], GIT_SOURCE);
    assert_eq!(response["source"], "pub fn beta() {\n}\n");
    Ok(())
}

#[tokio::test]
async fn external_code_read_reports_missing_indexed_source_text() -> Result<()> {
    let fixture = McpFixture::new("read-missing-source-text")?;
    fixture
        .conn
        .execute("DELETE FROM files WHERE file_path = 'src/lib.rs'", [])?;

    let search = handle_tool(
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

    assert_eq!(search["total_matches"], 1);
    assert_eq!(
        search["candidates"][0]["stable_symbol_id"],
        "bbbbbbbbbbbbbbbb"
    );

    let error = handle_tool(
        "external_code_read",
        &json!({
            "selector": "pkg:demo@latest::demo::beta",
            "context_lines": 0
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await
    .expect_err("missing source text should not look like a missing symbol");

    assert!(matches!(error, McpHandlerError::Internal(_)));
    assert!(
        error.to_string().contains("source text is not indexed"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[tokio::test]
async fn external_code_read_accepts_search_uri_for_ambiguous_symbol_name() -> Result<()> {
    let fixture = McpFixture::new("read-search-uri")?;
    move_fixture_to_source(&fixture, GIT_SOURCE)?;

    let search = handle_tool(
        "external_code_search",
        &json!({
            "source": GIT_SOURCE,
            "query": "Buffer",
            "package": PACKAGE,
            "symbol_kind": "struct",
            "limit": 10
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(search["total_matches"], 1);
    assert_eq!(
        search["candidates"][0]["selector"],
        "pkg:demo@1.0.0::Buffer"
    );
    let uri = search["candidates"][0]["uri"]
        .as_str()
        .context("search candidate URI")?;

    let response = handle_tool(
        "external_code_read",
        &json!({
            "selector": uri,
            "context_lines": 0
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(response["stable_symbol_id"], "bufferstruct0001");
    assert_eq!(response["package_source"], GIT_SOURCE);
    assert_eq!(
        response["source"],
        "pub struct Buffer {\n    bytes: [u8; 20],\n}\n"
    );
    Ok(())
}

#[tokio::test]
async fn external_call_graph_accepts_search_uri_selectors() -> Result<()> {
    let fixture = McpFixture::new("call-graph-search-uri")?;

    let beta_uri = search_uri(&fixture, "bet", Some("function")).await?;
    let callers = handle_tool(
        "external_code_callers",
        &json!({
            "selector": beta_uri,
            "include_unresolved": true
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert_eq!(
        callers["callers"]
            .as_array()
            .context("callers array")?
            .len(),
        3
    );
    assert_eq!(callers["counts_by_kind"]["unresolved"], 1);

    let alpha_uri = search_uri(&fixture, "alpha", Some("function")).await?;
    let callees = handle_tool(
        "external_code_callees",
        &json!({
            "selector": alpha_uri,
            "include_unresolved": true
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert_eq!(
        callees["callees"]
            .as_array()
            .context("callees array")?
            .len(),
        2
    );
    assert_eq!(
        callees["callees"][0]["callee"]["stable_symbol_id"],
        "bbbbbbbbbbbbbbbb"
    );
    assert_eq!(
        callees["callees"][1]["edge"]["target_label"],
        "external::Thing"
    );
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
async fn external_tools_support_multi_round_agent_eval_flow() -> Result<()> {
    let fixture = McpFixture::new("multi-round-eval")?;

    let pack = handle_tool(
        "external_knowledge_context",
        &json!({
            "query": "alpha beta external thing",
            "package": PACKAGE,
            "ref": "latest",
            "scope": "all",
            "limit": 8
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    let alpha = pack["primary_evidence"]
        .as_array()
        .context("primary_evidence array")?
        .iter()
        .find(|evidence| evidence["stable_symbol_id"] == "pkg:demo@1.0.0::demo::alpha")
        .context("alpha evidence")?;
    let alpha_selector = alpha["stable_symbol_id"]
        .as_str()
        .context("alpha selector")?;
    assert_next_tools(
        alpha,
        alpha_selector,
        [
            "external_code_read",
            "external_code_callers",
            "external_code_callees",
        ],
    )?;

    let alpha_source = handle_tool(
        "external_code_read",
        &json!({
            "selector": alpha_selector,
            "context_lines": 0
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert!(alpha_source["source"]
        .as_str()
        .context("alpha source")?
        .contains("external::Thing::new();"));

    let alpha_callees = handle_tool(
        "external_code_callees",
        &json!({
            "selector": alpha_selector,
            "include_unresolved": false
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert_eq!(alpha_callees["counts_by_kind"]["calls"], 1);
    assert_eq!(alpha_callees["counts_by_kind"]["unresolved"], 0);
    assert_eq!(alpha_callees["unresolved_sample"], json!([]));
    assert_eq!(
        alpha_callees["callees"]
            .as_array()
            .context("alpha callees")?
            .len(),
        1
    );

    let alpha_callees_with_unresolved = handle_tool(
        "external_code_callees",
        &json!({
            "selector": alpha_selector,
            "include_unresolved": true
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert_eq!(alpha_callees_with_unresolved["counts_by_kind"]["calls"], 2);
    assert_eq!(
        alpha_callees_with_unresolved["unresolved_sample"],
        json!(["external::Thing"])
    );
    assert_eq!(
        alpha_callees_with_unresolved["callees"]
            .as_array()
            .context("alpha callees with unresolved")?
            .len(),
        2
    );

    let beta_uri = search_uri(&fixture, "bet", Some("function")).await?;
    let beta_source = handle_tool(
        "external_code_read",
        &json!({
            "selector": beta_uri,
            "context_lines": 0
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert_eq!(beta_source["stable_symbol_id"], "bbbbbbbbbbbbbbbb");

    let beta_callers = handle_tool(
        "external_code_callers",
        &json!({
            "selector": beta_source["selector"],
            "include_unresolved": true
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    assert_eq!(beta_callers["counts_by_kind"]["calls"], 1);
    assert_eq!(beta_callers["counts_by_kind"]["calls_dyn"], 1);
    assert_eq!(beta_callers["counts_by_kind"]["references_hof"], 1);
    assert_eq!(beta_callers["counts_by_kind"]["unresolved"], 1);
    assert_eq!(
        beta_callers["callers"]
            .as_array()
            .context("beta callers")?
            .len(),
        3
    );
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

#[tokio::test]
async fn missing_serving_catalog_returns_empty_read_tool_results() -> Result<()> {
    let search = handle_tool_without_catalog(
        "external_code_search",
        &json!({
            "query": "alpha",
            "package": PACKAGE,
            "limit": 20
        }),
    )?;
    assert_eq!(search["total_matches"], 0);
    assert_eq!(search["truncated"], false);
    assert_eq!(search["candidates"], json!([]));

    let source = handle_tool_without_catalog(
        "external_code_read",
        &json!({
            "selector": "pkg:demo@latest::demo::alpha",
            "context_lines": 0
        }),
    )?;
    assert!(source.is_null());

    let callers = handle_tool_without_catalog(
        "external_code_callers",
        &json!({
            "selector": "pkg:demo@latest::demo::alpha",
            "include_unresolved": true
        }),
    )?;
    assert_eq!(callers["callers"], json!([]));
    assert_eq!(callers["counts_by_kind"]["calls"], 0);
    assert_eq!(callers["unresolved_sample"], json!([]));

    let callees = handle_tool_without_catalog(
        "external_code_callees",
        &json!({
            "selector": "pkg:demo@latest::demo::alpha",
            "include_unresolved": true
        }),
    )?;
    assert_eq!(callees["callees"], json!([]));
    assert_eq!(callees["counts_by_kind"]["calls"], 0);
    assert_eq!(callees["unresolved_sample"], json!([]));

    let knowledge = handle_tool_without_catalog(
        "external_knowledge_context",
        &json!({
            "query": "how does alpha work",
            "package": PACKAGE,
            "limit": 8
        }),
    )?;
    assert_eq!(knowledge["answerable"], false);
    assert_eq!(knowledge["confidence"], "low");
    assert_eq!(knowledge["primary_evidence"], json!([]));
    assert_eq!(knowledge["supporting_docs"], json!([]));
    assert_eq!(knowledge["candidates"]["total"], 0);

    Ok(())
}

#[tokio::test]
async fn external_index_rejects_missing_source_url() -> Result<()> {
    let fixture = McpFixture::new("index-missing-source-url")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let error = route_index(
        &json!({
            "package": PACKAGE,
            "revision": REVISION,
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-missing-source-url",
    )
    .await
    .expect_err("missing source_url should fail validation");

    assert!(matches!(error, McpHandlerError::InvalidParams(_)));
    assert_eq!(sfn.started_count(), 0);
    Ok(())
}

#[tokio::test]
async fn external_index_without_serving_catalog_starts_execution() -> Result<()> {
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index_without_catalog(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": SOURCE_URL,
            "source_kind": "git",
        }),
        &jobs,
        &sfn,
        "caller-bootstrap",
    )
    .await?;

    assert_eq!(response["status"], "queued");
    assert_eq!(response["job_id"], "job-1");
    assert_eq!(response["execution_arn"], "arn:stub:job-1");
    assert_eq!(response["revision"], "main");
    assert_eq!(sfn.started_count(), 1);
    let stored = jobs.lookup_job_sync("job-1").context("created job")?;
    assert_eq!(stored.caller_id, "caller-bootstrap");
    Ok(())
}

#[tokio::test]
async fn external_index_payload_prefetches_github_git_sources() -> Result<()> {
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index_without_catalog(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": "https://github.com/getspur/spur",
        }),
        &jobs,
        &sfn,
        "caller-prefetch-github",
    )
    .await?;

    assert_eq!(response["status"], "queued");
    let started = sfn.started_requests();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].input["source_kind"], "git");
    assert_eq!(started[0].input["prefetch_source"], json!(true));
    Ok(())
}

#[tokio::test]
async fn external_index_payload_prefetches_non_s3_https_tarballs() -> Result<()> {
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index_without_catalog(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": "https://example.com/x.tar.gz",
        }),
        &jobs,
        &sfn,
        "caller-prefetch-tarball",
    )
    .await?;

    assert_eq!(response["status"], "queued");
    let started = sfn.started_requests();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].input["source_kind"], "tarball");
    assert_eq!(started[0].input["prefetch_source"], json!(true));
    Ok(())
}

#[tokio::test]
async fn external_index_payload_skips_prefetch_for_presigned_s3_https_tarballs() -> Result<()> {
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index_without_catalog(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": "https://s3.amazonaws.com/spur-context-test/source.tar.gz?X-Amz-Expires=3600&X-Amz-Signature=test",
        }),
        &jobs,
        &sfn,
        "caller-prefetch-s3",
    )
    .await?;

    assert_eq!(response["status"], "queued");
    let started = sfn.started_requests();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].input["source_kind"], "tarball");
    assert_eq!(started[0].input["prefetch_source"], json!(false));
    Ok(())
}

#[tokio::test]
async fn external_index_status_returns_not_found_for_unknown_job() -> Result<()> {
    let jobs = FakeJobStore::default();

    let response = route_index_status(&json!({ "job_id": "missing-job" }), &jobs, None).await?;

    assert_eq!(response, json!({ "status": "not_found" }));
    Ok(())
}

#[tokio::test]
async fn external_index_status_for_caller_hides_jobs_owned_by_other_callers() -> Result<()> {
    let jobs = FakeJobStore::default();
    let job = jobs.seed_queued_job("arn:other-caller", |record| {
        record.caller_id = "caller-a".to_owned();
    });
    let checker = StubExecutionStatusChecker::new(Some(ExecutionOutcome {
        status: ExecutionOutcomeStatus::Succeeded,
        output: Some(json!({
            "snapshot_id": 777,
        })),
        error: None,
    }));

    let response = route_index_status_for_caller(
        &json!({ "job_id": job.job_id }),
        &jobs,
        Some(&checker),
        "caller-b",
    )
    .await?;

    assert_eq!(response, json!({ "status": "not_found" }));
    assert_eq!(checker.described_arns(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn lambda_index_status_route_does_not_require_catalog_initialization() -> Result<()> {
    let jobs = FakeJobStore::default();
    let job = jobs.seed_queued_job("arn:lambda-status", |record| {
        record.caller_id = "caller-lambda-status".to_owned();
    });
    let job_id = job.job_id.clone();
    let checker = StubExecutionStatusChecker::new(None);

    let response = spur_context_service::lambda::route_index_status_control_plane(
        &json!({ "job_id": job_id.clone() }),
        &jobs,
        &checker,
        "caller-lambda-status",
    )
    .await?;

    assert_eq!(response["job_id"], job_id);
    assert_eq!(response["status"], "queued");
    assert_eq!(checker.described_arns(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn external_index_status_repairs_stale_succeeded_execution() -> Result<()> {
    let jobs = FakeJobStore::default();
    let job = jobs.seed_queued_job("arn:success", |record| {
        record.updated_at = "0".to_owned();
    });
    let checker = StubExecutionStatusChecker::new(Some(ExecutionOutcome {
        status: ExecutionOutcomeStatus::Succeeded,
        output: Some(json!({
            "snapshot_id": 777,
            "rows_inserted": {
                "nodes": 5,
                "edges": 4
            }
        })),
        error: None,
    }));

    let response =
        route_index_status(&json!({ "job_id": job.job_id }), &jobs, Some(&checker)).await?;

    assert_eq!(response["status"], "complete");
    assert_eq!(response["snapshot_id"], 777);
    assert_eq!(response["row_counts"], json!({ "nodes": 5, "edges": 4 }));
    assert_eq!(response["execution_arn"], "arn:success");
    assert_eq!(checker.described_arns(), ["arn:success"]);
    Ok(())
}

#[tokio::test]
async fn external_index_status_reconciles_failed_execution() -> Result<()> {
    let jobs = FakeJobStore::default();
    let job = jobs.seed_queued_job("arn:failed", |record| {
        record.updated_at = "0".to_owned();
    });
    let checker = StubExecutionStatusChecker::new(Some(ExecutionOutcome {
        status: ExecutionOutcomeStatus::Failed,
        output: None,
        error: Some("fetch: clone failed".to_owned()),
    }));

    let response =
        route_index_status(&json!({ "job_id": job.job_id }), &jobs, Some(&checker)).await?;

    assert_eq!(response["status"], "failed");
    assert_eq!(response["error"]["code"], "fetch");
    assert_eq!(response["error"]["detail"], "clone failed");
    assert_eq!(checker.described_arns(), ["arn:failed"]);
    Ok(())
}

#[tokio::test]
async fn external_index_status_without_checker_returns_stale_job() -> Result<()> {
    let jobs = FakeJobStore::default();
    let job = jobs.seed_queued_job("arn:stale", |record| {
        record.updated_at = "0".to_owned();
    });

    let response = route_index_status(&json!({ "job_id": job.job_id }), &jobs, None).await?;

    assert_eq!(response["status"], "queued");
    assert!(response.get("snapshot_id").is_none());
    assert!(response.get("error").is_none());
    Ok(())
}

#[tokio::test]
async fn external_index_status_returns_dynamodb_state_when_describe_execution_fails() -> Result<()>
{
    let jobs = FakeJobStore::default();
    let job = jobs.seed_queued_job("arn:transient", |record| {
        record.status = JobStatus::Running;
        record.stage = Some("building_graph".to_owned());
        record.updated_at = "0".to_owned();
    });
    let checker = StubExecutionStatusChecker::fail("temporary sfn outage");

    let response =
        route_index_status(&json!({ "job_id": job.job_id }), &jobs, Some(&checker)).await?;

    assert_eq!(response["status"], "running");
    assert_eq!(response["stage"], "building_graph");
    assert_eq!(response["execution_arn"], "arn:transient");
    assert_eq!(checker.described_arns(), ["arn:transient"]);
    Ok(())
}

#[tokio::test]
async fn external_index_returns_complete_for_warm_catalog_hit() -> Result<()> {
    let fixture = McpFixture::new("index-warm")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index(
        &json!({
            "package": PACKAGE,
            "revision": REVISION,
            "source_url": SOURCE_URL,
            "source": "registry:crates-io"
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-warm",
    )
    .await?;

    assert_eq!(response["status"], "complete");
    assert_eq!(response["snapshot_id"], 100);
    assert_eq!(response["revision"], REVISION);
    assert_eq!(sfn.started_count(), 0);
    assert_eq!(jobs.job_count(), 0);
    Ok(())
}

#[tokio::test]
async fn external_index_creates_job_starts_execution_and_records_arn() -> Result<()> {
    let fixture = McpFixture::new("index-create")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": SOURCE_URL,
            "source_kind": "git",
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-create",
    )
    .await?;

    assert_eq!(response["status"], "queued");
    assert_eq!(response["job_id"], "job-1");
    assert_eq!(response["execution_arn"], "arn:stub:job-1");
    assert_eq!(response["revision"], "main");
    assert_eq!(sfn.started_count(), 1);
    let stored = jobs.lookup_job_sync("job-1").context("created job")?;
    assert_eq!(stored.execution_arn.as_deref(), Some("arn:stub:job-1"));
    assert_eq!(stored.caller_id, "caller-create");
    assert_eq!(stored.source_kind, "git");
    let started = sfn.started_requests();
    assert_eq!(
        started[0].input["limits"],
        json!({
            "max_source_bytes": 2147483648_u64,
            "max_build_seconds": 1800_u64
        })
    );
    Ok(())
}

#[tokio::test]
async fn external_index_rejects_when_caller_concurrent_cap_is_full() -> Result<()> {
    let fixture = McpFixture::new("index-concurrent-cap")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();
    jobs.seed_queued_job("arn:busy-a", |record| {
        record.caller_id = "caller-full".to_owned();
        record.revision = "busy-a".to_owned();
    });
    jobs.seed_queued_job("arn:busy-b", |record| {
        record.caller_id = "caller-full".to_owned();
        record.revision = "busy-b".to_owned();
    });

    let response = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "new-work",
            "source_url": SOURCE_URL,
            "source_kind": "git",
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-full",
    )
    .await?;

    assert_eq!(response["status"], "rejected");
    assert_eq!(response["reason"], "concurrent_job_limit");
    assert_eq!(sfn.started_count(), 0);
    Ok(())
}

#[tokio::test]
async fn external_index_rejects_when_caller_rate_limit_is_full() -> Result<()> {
    let fixture = McpFixture::new("index-rate-limit")?;
    let jobs = FakeJobStore::default();
    jobs.set_rate_limit_per_minute(1);
    let sfn = StubIndexExecutionStarter::default();

    let first = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "rate-a",
            "source_url": SOURCE_URL,
            "source_kind": "git",
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-rate-limited",
    )
    .await?;

    assert_eq!(first["status"], "queued");

    let second = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "rate-b",
            "source_url": SOURCE_URL,
            "source_kind": "git",
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-rate-limited",
    )
    .await?;

    assert_eq!(second["status"], "rejected");
    assert_eq!(second["reason"], "rate_limit");
    assert_eq!(sfn.started_count(), 1);
    Ok(())
}

#[tokio::test]
async fn external_index_returns_existing_deduped_job_without_starting_execution() -> Result<()> {
    let fixture = McpFixture::new("index-dedup")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();
    let existing = jobs.seed_queued_job("arn:existing", |_| {});

    let response = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": SOURCE_URL,
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-dedup",
    )
    .await?;

    assert_eq!(response["job_id"], existing.job_id);
    assert_eq!(response["status"], "queued");
    assert_eq!(response["execution_arn"], "arn:existing");
    assert_eq!(response["revision"], "main");
    assert_eq!(sfn.started_count(), 0);
    Ok(())
}

#[tokio::test]
async fn external_index_status_supports_cold_index_then_retry_eval_flow() -> Result<()> {
    let fixture = McpFixture::new("index-multi-round")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let queued = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source": "registry:crates-io",
            "source_url": SOURCE_URL,
            "source_kind": "git",
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-multi-round",
    )
    .await?;

    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["job_id"], "job-1");
    assert_eq!(sfn.started_count(), 1);

    let retry_before_catalog = handle_tool(
        "external_code_search",
        &json!({
            "query": "alpha",
            "package": PACKAGE,
            "revision": "main"
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await
    .expect_err("cold revision should not be queryable before catalog is populated");
    assert!(matches!(retry_before_catalog, McpHandlerError::NotFound(_)));

    jobs.update_job("job-1", |record| {
        record.updated_at = "0".to_owned();
    })?;
    let checker = StubExecutionStatusChecker::new(Some(ExecutionOutcome {
        status: ExecutionOutcomeStatus::Succeeded,
        output: Some(json!({
            "snapshot_id": 101,
            "rows_inserted": {
                "nodes": 9,
                "edges": 4
            }
        })),
        error: None,
    }));

    let complete = route_index_status(&json!({ "job_id": "job-1" }), &jobs, Some(&checker)).await?;

    assert_eq!(complete["status"], "complete");
    assert_eq!(complete["snapshot_id"], 101);
    assert_eq!(checker.described_arns(), ["arn:stub:job-1"]);

    move_fixture_to_revision(&fixture, "main", "git")?;
    let retry_after_catalog = handle_tool(
        "external_code_search",
        &json!({
            "query": "alpha",
            "package": PACKAGE,
            "revision": "main"
        }),
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;

    assert_eq!(retry_after_catalog["total_matches"], 1);
    assert_eq!(
        retry_after_catalog["candidates"][0]["selector"],
        "pkg:demo@main::demo::alpha"
    );
    Ok(())
}

#[tokio::test]
async fn external_index_rejects_localhost_source_url_before_starting_job() -> Result<()> {
    let fixture = McpFixture::new("index-abuse")?;
    let jobs = FakeJobStore::default();
    let sfn = StubIndexExecutionStarter::default();

    let response = route_index(
        &json!({
            "package": PACKAGE,
            "revision": "main",
            "source_url": "https://localhost/example/demo",
        }),
        &fixture.conn,
        &fixture.catalog,
        &jobs,
        &sfn,
        "caller-abuse",
    )
    .await?;

    assert_eq!(response["status"], "rejected");
    assert!(response["reason"]
        .as_str()
        .context("reason string")?
        .contains("source_url targets localhost"));
    assert_eq!(sfn.started_count(), 0);
    Ok(())
}

fn assert_next_tools<const N: usize>(
    evidence: &Value,
    selector: &str,
    expected_tools: [&str; N],
) -> Result<()> {
    let next = evidence["next"].as_array().context("next array")?;
    for tool in expected_tools {
        assert!(
            next.iter().any(|entry| {
                entry["tool"] == tool && entry["selector"].as_str() == Some(selector)
            }),
            "missing {tool} next entry for {selector}: {next:?}"
        );
    }
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

async fn search_uri(
    fixture: &McpFixture,
    query: &str,
    symbol_kind: Option<&str>,
) -> Result<String> {
    let mut args = json!({
        "query": query,
        "package": PACKAGE,
        "limit": 20
    });
    if let Some(symbol_kind) = symbol_kind {
        args["symbol_kind"] = json!(symbol_kind);
    }
    let response = handle_tool(
        "external_code_search",
        &args,
        &fixture.conn,
        &fixture.catalog,
    )
    .await?;
    response["candidates"][0]["uri"]
        .as_str()
        .map(str::to_owned)
        .context("search candidate URI")
}

fn required(schema: &Value) -> Vec<&str> {
    schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

fn move_fixture_to_source(fixture: &McpFixture, source: &str) -> Result<()> {
    for table in [
        "nodes",
        "edges",
        "edges_unresolved",
        "files",
        "section_bodies",
        "symbol_embeddings",
        "refs",
    ] {
        fixture
            .conn
            .execute(
                &format!("UPDATE {table} SET source = ? WHERE source = 'registry:crates-io'"),
                params![source],
            )
            .with_context(|| format!("move query fixture table {table} to source {source}"))?;
    }

    for table in ["package_catalog", "refs"] {
        fixture
            .catalog
            .connection()
            .execute(
                &format!("UPDATE {table} SET source = ? WHERE source = 'registry:crates-io'"),
                params![source],
            )
            .with_context(|| format!("move catalog fixture table {table} to source {source}"))?;
    }
    Ok(())
}

fn move_fixture_to_revision(
    fixture: &McpFixture,
    revision: &str,
    revision_kind: &str,
) -> Result<()> {
    for table in [
        "nodes",
        "edges",
        "edges_unresolved",
        "files",
        "section_bodies",
        "symbol_embeddings",
    ] {
        fixture
            .conn
            .execute(
                &format!(
                    "UPDATE {table} SET revision = ?, revision_kind = ? WHERE revision = '1.0.0'"
                ),
                params![revision, revision_kind],
            )
            .with_context(|| format!("move query fixture table {table} to revision {revision}"))?;
    }

    fixture
        .conn
        .execute(
            "UPDATE refs SET revision = ? WHERE revision = '1.0.0'",
            params![revision],
        )
        .with_context(|| format!("move query refs to revision {revision}"))?;

    fixture
        .catalog
        .connection()
        .execute(
            "UPDATE package_catalog SET revision = ?, revision_kind = ?, snapshot_id = 101 WHERE revision = '1.0.0'",
            params![revision, revision_kind],
        )
        .with_context(|| format!("move catalog fixture to revision {revision}"))?;
    fixture
        .catalog
        .connection()
        .execute(
            "UPDATE refs SET revision = ? WHERE revision = '1.0.0'",
            params![revision],
        )
        .with_context(|| format!("move catalog refs to revision {revision}"))?;
    Ok(())
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

struct FrozenServingFixture {
    snapshot_path: PathBuf,
    data_path: String,
    _root: PathBuf,
}

impl FrozenServingFixture {
    fn new(name: &str) -> Result<Self> {
        let root = unique_temp_dir(name)?;
        let data_path = root.join("data");
        fs::create_dir_all(&data_path).context("create frozen serving data dir")?;

        let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
        let data_path = data_path.display().to_string();
        initialize_gold_serving_catalog(&catalog_dsn, &data_path)?;
        compact_gold_and_export_snapshot(
            &catalog_dsn,
            &data_path,
            SnapshotCleanupOptions {
                older_than: std::time::Duration::from_secs(3600),
                republish_lag: std::time::Duration::from_secs(300),
            },
        )?;

        let snapshot_path = catalog_snapshot_path(&data_path)?;
        assert!(
            snapshot_path.is_file(),
            "expected frozen serving snapshot at {}",
            snapshot_path.display()
        );

        Ok(Self {
            snapshot_path,
            data_path,
            _root: root,
        })
    }
}

fn initialize_gold_serving_catalog(catalog_dsn: &str, data_path: &str) -> Result<()> {
    let conn = Connection::open_in_memory().context("open frozen serving setup DuckDB")?;
    attach_ducklake(&conn, catalog_dsn, data_path)?;
    conn.execute_batch(include_str!("../sql/catalog_tables.sql"))
        .context("create medallion catalog tables")?;

    let source_text = "pub fn beta() {\n}\n";
    conn.execute(
        r"
        INSERT INTO gold.files (
            stable_file_id, file_path, source_text,
            package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch, generation
        )
        VALUES (
            'file-demo-lib', 'src/lib.rs', ?,
            'demo', 'registry:crates-io', '1.0.0', 'semver',
            1, 0, 0, 7
        )
        ",
        params![source_text],
    )
    .context("insert frozen serving file")?;
    conn.execute(
        r"
        INSERT INTO gold.nodes (
            stable_symbol_id, package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch, file_path,
            byte_range_start, byte_range_end, line_start, line_end,
            entity_name, qualified_name, symbol_kind, anchor_hash,
            enclosing_scope, generation
        )
        VALUES (
            'bbbbbbbbbbbbbbbb', 'demo', 'registry:crates-io', '1.0.0', 'semver',
            1, 0, 0, 'src/lib.rs',
            0, ?, 1, 2,
            'beta', 'demo::beta', 'function', 'anchor-beta',
            NULL, 7
        )
        ",
        params![source_text.len() as i64],
    )
    .context("insert frozen serving node")?;
    conn.execute_batch(
        r#"
        INSERT INTO gold.package_catalog (
            source, package, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            snapshot_id, indexed_at, index_status, embeddings_status, row_counts,
            generation, bronze_content_sha256, silver_graph_content_hash,
            builder_version, translate_schema_version
        )
        VALUES (
            'registry:crates-io', 'demo', '1.0.0', 'semver',
            1, 0, 0,
            777, TIMESTAMP '2026-06-28 00:00:00',
            'complete', 'skipped', '{"nodes":1}',
            7, 'bronze-fixture', 'graph-fixture', 'builder-fixture', 'translate-fixture'
        );

        INSERT INTO gold.refs (source, package, ref_name, revision, updated_at)
        VALUES (
            'registry:crates-io', 'demo', 'latest', '1.0.0',
            TIMESTAMP '2026-06-28 00:01:00'
        );
        "#,
    )
    .context("insert frozen serving catalog metadata")?;
    flush_inlined_ducklake_data(&conn)?;
    conn.execute("FORCE CHECKPOINT", [])
        .context("checkpoint frozen serving fixture")?;
    Ok(())
}

fn flush_inlined_ducklake_data(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("CALL ducklake_flush_inlined_data('spur_context')")
        .context("prepare ducklake_flush_inlined_data")?;
    let mut rows = stmt.query([]).context("execute ducklake flush")?;
    while rows.next()?.is_some() {}
    Ok(())
}

fn catalog_snapshot_path(data_path: &str) -> Result<PathBuf> {
    let pointer_path = PathBuf::from(data_path)
        .join("gold")
        .join("catalog-snapshot")
        .join("current.json");
    let pointer: FrozenSnapshotManifest = serde_json::from_slice(
        &fs::read(&pointer_path)
            .with_context(|| format!("read snapshot pointer {}", pointer_path.display()))?,
    )
    .context("parse snapshot pointer")?;
    Ok(PathBuf::from(pointer.snapshot_uri))
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
        "\n",
        "pub struct Buffer {\n",
        "    bytes: [u8; 20],\n",
        "}\n",
        "\n",
        "impl Buffer {\n",
        "    pub fn new() -> Self { Self { bytes: [0; 20] } }\n",
        "}\n",
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
    insert_node_with_kind(
        conn,
        "bufferstruct0001",
        source_text,
        "Buffer",
        "Buffer",
        "struct",
        "pub struct Buffer",
        21,
        23,
    )?;
    insert_node_with_kind(
        conn,
        "bufferimpl000001",
        source_text,
        "Buffer",
        "impl Buffer",
        "impl",
        "impl Buffer",
        25,
        27,
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
    insert_node_with_kind(
        conn,
        id,
        source_text,
        entity_name,
        qualified_name,
        "function",
        &format!("pub fn {entity_name}"),
        line_start,
        line_end,
    )
}

fn insert_node_with_kind(
    conn: &Connection,
    id: &str,
    source_text: &str,
    entity_name: &str,
    qualified_name: &str,
    symbol_kind: &str,
    marker: &str,
    line_start: i64,
    line_end: i64,
) -> Result<()> {
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
             'src/lib.rs', $2, $3, $4, $5, $6, $7, $8, $9, NULL)
        ",
        params![
            id,
            byte_start,
            byte_end,
            line_start,
            line_end,
            entity_name,
            qualified_name,
            symbol_kind,
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
             {}, '{EMBEDDING_MODEL}', 'hash-{id}', '{EMBED_TEXT_VERSION}')
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
    format!("[{values}]::FLOAT[]")
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

fn source_url_hash(source_url: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source_url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[derive(Default)]
struct StubIndexExecutionStarter {
    started: Mutex<Vec<IndexExecutionRequest>>,
}

impl StubIndexExecutionStarter {
    fn started_count(&self) -> usize {
        self.started.lock().unwrap().len()
    }

    fn started_requests(&self) -> Vec<IndexExecutionRequest> {
        self.started.lock().unwrap().clone()
    }
}

impl IndexExecutionStarter for StubIndexExecutionStarter {
    fn start_execution<'a>(
        &'a self,
        request: IndexExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, McpHandlerError>> + Send + 'a>>
    {
        Box::pin(async move {
            let job_id = request.name.clone();
            self.started.lock().unwrap().push(request);
            Ok(format!("arn:stub:{job_id}"))
        })
    }
}

struct StubExecutionStatusChecker {
    result: StubExecutionResult,
    described_arns: Mutex<Vec<String>>,
}

enum StubExecutionResult {
    Ok(Option<ExecutionOutcome>),
    Err(String),
}

impl StubExecutionStatusChecker {
    fn new(outcome: Option<ExecutionOutcome>) -> Self {
        Self {
            result: StubExecutionResult::Ok(outcome),
            described_arns: Mutex::new(Vec::new()),
        }
    }

    fn fail(message: &str) -> Self {
        Self {
            result: StubExecutionResult::Err(message.to_owned()),
            described_arns: Mutex::new(Vec::new()),
        }
    }

    fn described_arns(&self) -> Vec<String> {
        self.described_arns.lock().unwrap().clone()
    }
}

impl ExecutionStatusChecker for StubExecutionStatusChecker {
    fn describe_execution(
        &self,
        arn: &str,
    ) -> std::result::Result<Option<ExecutionOutcome>, McpHandlerError> {
        self.described_arns.lock().unwrap().push(arn.to_owned());
        match &self.result {
            StubExecutionResult::Ok(outcome) => Ok(outcome.clone()),
            StubExecutionResult::Err(message) => Err(McpHandlerError::Internal(message.clone())),
        }
    }
}

#[derive(Default)]
struct FakeJobStore {
    next_id: AtomicU64,
    state: Mutex<FakeJobState>,
}

#[derive(Default)]
struct FakeJobState {
    jobs: HashMap<String, JobRecord>,
    dedupe: HashMap<JobKey, String>,
    rate_limit_max: Option<u32>,
    rate_counts: HashMap<String, u32>,
}

#[async_trait]
impl JobStore for FakeJobStore {
    async fn check_index_rate_limit(
        &self,
        caller_id: &str,
        max_requests_per_minute: u32,
    ) -> spur_context_service::jobs::Result<()> {
        let mut state = self.state.lock().expect("fake store lock");
        let max = state.rate_limit_max.unwrap_or(max_requests_per_minute);
        if max == 0 {
            return Err(JobsError::RateLimited);
        }
        let count = state.rate_counts.entry(caller_id.to_owned()).or_default();
        if *count >= max {
            return Err(JobsError::RateLimited);
        }
        *count += 1;
        Ok(())
    }

    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> spur_context_service::jobs::Result<CreateJobOutcome> {
        self.create_or_get_active_job_with_limit(request, u32::MAX)
            .await
    }

    async fn create_or_get_active_job_with_limit(
        &self,
        request: CreateJobRequest,
        max_active_jobs_per_caller: u32,
    ) -> spur_context_service::jobs::Result<CreateJobOutcome> {
        let key = request.key();
        let mut state = self.state.lock().expect("fake store lock");
        if let Some(job_id) = state.dedupe.get(&key) {
            if let Some(record) = state.jobs.get(job_id) {
                return Ok(CreateJobOutcome::Existing(record.clone()));
            }
        }

        let active_jobs = state
            .jobs
            .values()
            .filter(|record| {
                record.caller_id == request.caller_id
                    && matches!(record.status, JobStatus::Queued | JobStatus::Running)
            })
            .count();
        if active_jobs >= max_active_jobs_per_caller as usize {
            return Err(JobsError::ConcurrentLimit);
        }

        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let record = JobRecord {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            source: request.source,
            package: request.package,
            revision: request.revision,
            source_url: request.source_url,
            source_url_hash: request.source_url_hash,
            source_kind: request.source_kind,
            caller_id: request.caller_id,
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: "now".to_owned(),
            updated_at: "now".to_owned(),
        };
        state.dedupe.insert(key, job_id.clone());
        state.jobs.insert(job_id, record.clone());
        Ok(CreateJobOutcome::Created(record))
    }

    async fn record_execution_started(
        &self,
        job_id: &str,
        execution_arn: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        self.update_job(job_id, |record| {
            record.execution_arn = Some(execution_arn.to_owned());
            record.updated_at = "started".to_owned();
        })
    }

    async fn update_stage(
        &self,
        job_id: &str,
        status: JobStatus,
        stage: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        self.update_job(job_id, |record| {
            record.status = status;
            record.stage = Some(stage.to_owned());
            record.updated_at = "stage".to_owned();
        })
    }

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let record = self.update_job(job_id, |record| {
            record.status = JobStatus::Complete;
            record.snapshot_id = Some(snapshot_id);
            record.row_counts = Some(row_counts);
            record.error_code = None;
            record.error_detail = None;
            record.updated_at = "complete".to_owned();
        })?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn mark_failed(
        &self,
        job_id: &str,
        code: &str,
        detail: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let record = self.update_job(job_id, |record| {
            record.status = JobStatus::Failed;
            record.error_code = Some(code.to_owned());
            record.error_detail = Some(detail.to_owned());
            record.updated_at = "failed".to_owned();
        })?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn lookup_job(
        &self,
        job_id: &str,
    ) -> spur_context_service::jobs::Result<Option<JobRecord>> {
        Ok(self.lookup_job_sync(job_id))
    }

    async fn release_dedupe_if_owner(
        &self,
        record: &JobRecord,
    ) -> spur_context_service::jobs::Result<()> {
        let mut state = self.state.lock().expect("fake store lock");
        let key = record.key();
        if state
            .dedupe
            .get(&key)
            .is_some_and(|job_id| job_id == &record.job_id)
        {
            state.dedupe.remove(&key);
        }
        Ok(())
    }
}

impl FakeJobStore {
    fn set_rate_limit_per_minute(&self, max: u32) {
        self.state.lock().expect("fake store lock").rate_limit_max = Some(max);
    }

    fn seed_queued_job(
        &self,
        execution_arn: &str,
        update: impl FnOnce(&mut JobRecord),
    ) -> JobRecord {
        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let mut record = JobRecord {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            source: "git:custom".to_owned(),
            package: PACKAGE.to_owned(),
            revision: "main".to_owned(),
            source_url: SOURCE_URL.to_owned(),
            source_url_hash: source_url_hash(SOURCE_URL),
            source_kind: "git".to_owned(),
            caller_id: "seed".to_owned(),
            execution_arn: Some(execution_arn.to_owned()),
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: "now".to_owned(),
            updated_at: "now".to_owned(),
        };
        update(&mut record);
        let mut state = self.state.lock().expect("fake store lock");
        state.dedupe.insert(record.key(), job_id.clone());
        state.jobs.insert(job_id, record.clone());
        record
    }

    fn job_count(&self) -> usize {
        self.state.lock().expect("fake store lock").jobs.len()
    }

    fn lookup_job_sync(&self, job_id: &str) -> Option<JobRecord> {
        self.state
            .lock()
            .expect("fake store lock")
            .jobs
            .get(job_id)
            .cloned()
    }

    fn update_job(
        &self,
        job_id: &str,
        update: impl FnOnce(&mut JobRecord),
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let mut state = self.state.lock().expect("fake store lock");
        let record = state
            .jobs
            .get_mut(job_id)
            .ok_or(spur_context_service::jobs::JobsError::NotFound)?;
        update(record);
        Ok(record.clone())
    }
}
