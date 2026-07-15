mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use spur_graph::mcp::{
    ensure_graph_artifact_ready, local_project_tool_definitions, tool_definitions,
    with_worktree_root_for_request, GraphMcpDeps, GraphMcpModule,
};
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer, WriteOptions,
};
use spur_mcp::local_projects::{
    LocalProjectCatalogStore, LocalProjectError, LocalProjectHealth, LocalProjectResolver,
    LocalProjectValidator, ValidatedLocalProject,
};
use support::git_repo::GitRepo;

#[derive(Clone, Default)]
struct FixtureValidator;

impl LocalProjectValidator for FixtureValidator {
    fn validate(&self, requested_path: &Path) -> Result<ValidatedLocalProject, LocalProjectError> {
        let canonical_root =
            requested_path
                .canonicalize()
                .map_err(|error| LocalProjectError::InvalidPath {
                    path: requested_path.to_path_buf(),
                    reason: error.to_string(),
                })?;
        let health = if canonical_root.join(".unavailable").exists() {
            LocalProjectHealth::unavailable("graph artifact is unavailable")
        } else {
            LocalProjectHealth::ready()
        };
        Ok(ValidatedLocalProject {
            canonical_root,
            health,
        })
    }
}

fn indexed_repo(symbol: &str) -> GitRepo {
    indexed_repo_with_source(&format!("pub fn {symbol}() {{}}\n"))
}

fn indexed_repo_with_source(source: &str) -> GitRepo {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", source);
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "fixture"]);
    let (facts, _) = build_facts(repo.path(), None).expect("build graph facts");
    let artifact = artifact_from_facts(&facts, repo.path()).expect("build graph artifact");
    let artifact_dir = write_artifact_parquet(
        &artifact,
        &repo.path().join(".spur/graph"),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write graph artifact");
    write_current_pointer(repo.path(), &artifact_dir).expect("write graph pointer");
    repo
}

fn resolver(catalog_path: PathBuf, projects: &[(&str, &Path)]) -> LocalProjectResolver {
    let store = LocalProjectCatalogStore::new(catalog_path);
    for (name, root) in projects {
        store.add(name, root, false).expect("register fixture");
    }
    LocalProjectResolver::new(store, Arc::new(FixtureValidator))
}

fn project_property(definition: &spur_graph::mcp::ToolDefinition) -> Option<&Value> {
    definition
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("project"))
}

#[test]
fn graph_project_routing_is_opt_in_at_the_schema_boundary() {
    let default = GraphMcpModule::new(GraphMcpDeps::default()).tools();
    assert_eq!(
        serde_json::to_value(&default).expect("serialize module tools"),
        serde_json::to_value(tool_definitions()).expect("serialize default tools")
    );
    assert_eq!(default.len(), 9);
    assert!(default
        .iter()
        .all(|definition| project_property(definition).is_none()));

    let catalog = tempfile::tempdir().expect("catalog tempdir");
    let enabled = GraphMcpModule::with_local_projects(
        GraphMcpDeps::default(),
        resolver(catalog.path().join("projects.toml"), &[]),
    )
    .tools();
    assert_eq!(
        serde_json::to_value(&enabled).expect("serialize enabled tools"),
        serde_json::to_value(local_project_tool_definitions()).expect("serialize routed tools")
    );
    assert_eq!(
        enabled
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>(),
        default
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(enabled.iter().all(|definition| project_property(definition)
        .and_then(|property| property.get("type"))
        == Some(&json!("string"))));
}

#[tokio::test]
async fn enabled_module_preserves_default_scope_and_routes_canonical_and_alias_calls() {
    let current = indexed_repo("current_symbol");
    let alpha = indexed_repo("alpha_symbol");
    let catalog = tempfile::tempdir().expect("catalog tempdir");
    let module = GraphMcpModule::with_local_projects(
        GraphMcpDeps::default(),
        resolver(
            catalog.path().join("projects.toml"),
            &[("alpha", alpha.path())],
        ),
    );

    let current_response = with_worktree_root_for_request(
        current.path().to_path_buf(),
        module.dispatch(
            "code_symbol_search",
            json!({"query": "current_symbol", "mode": "exact"}),
        ),
    )
    .await
    .expect("query current worktree");
    assert_eq!(
        current_response["candidates"][0]["entity_name"],
        "current_symbol"
    );
    assert!(current_response.get("project").is_none());

    for tool_name in ["code_symbol_search", "code_search"] {
        let response = module
            .dispatch(
                tool_name,
                json!({"query": "alpha_symbol", "mode": "exact", "project": "alpha"}),
            )
            .await
            .expect("query alpha");
        assert_eq!(response["candidates"][0]["entity_name"], "alpha_symbol");
        assert_eq!(response["project"]["name"], "alpha");
        assert_eq!(
            response["project"]["root"],
            json!(alpha.path().canonicalize().expect("root"))
        );
        assert_eq!(response["project"]["catalog_generation"], 1);
    }
}

#[tokio::test]
async fn named_project_routes_symbol_reads_and_call_edges() {
    let alpha = indexed_repo_with_source(
        "pub fn alpha_caller() { alpha_callee(); }\npub fn alpha_callee() {}\n",
    );
    let catalog = tempfile::tempdir().expect("catalog tempdir");
    let module = GraphMcpModule::with_local_projects(
        GraphMcpDeps::default(),
        resolver(
            catalog.path().join("projects.toml"),
            &[("alpha", alpha.path())],
        ),
    );

    let read = module
        .dispatch(
            "code_read_symbol",
            json!({
                "path": "src/lib.rs",
                "name": "alpha_callee",
                "response_format": "source",
                "project": "alpha",
            }),
        )
        .await
        .expect("read symbol from alpha");
    assert!(
        read["source"]
            .as_str()
            .is_some_and(|source| source.contains("pub fn alpha_callee()")),
        "unexpected source response: {read:#?}"
    );
    assert_eq!(read["project"]["name"], "alpha");

    let callers = module
        .dispatch(
            "code_callers",
            json!({"selector": "alpha_callee", "project": "alpha"}),
        )
        .await
        .expect("find callers in alpha");
    assert!(
        callers["callers"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["entity_name"] == "alpha_caller")),
        "unexpected callers response: {callers:#?}"
    );
    assert_eq!(callers["project"]["name"], "alpha");
}

#[tokio::test]
async fn named_project_preserves_dirty_worktree_overlay_behavior() {
    let alpha = indexed_repo("indexed_symbol");
    alpha.write(
        "src/new_module.rs",
        "pub fn brand_new_dirty_project_symbol() -> u64 { 7 }\n",
    );
    let catalog = tempfile::tempdir().expect("catalog tempdir");
    let module = GraphMcpModule::with_local_projects(
        GraphMcpDeps::default(),
        resolver(
            catalog.path().join("projects.toml"),
            &[("alpha", alpha.path())],
        ),
    );

    let response = module
        .dispatch(
            "code_read_symbol",
            json!({
                "path": "src/new_module.rs",
                "name": "brand_new_dirty_project_symbol",
                "response_format": "source",
                "project": "alpha",
            }),
        )
        .await
        .expect("dirty-worktree overlay should run in the named project");
    assert!(
        response["source"]
            .as_str()
            .is_some_and(|source| source.contains("brand_new_dirty_project_symbol")),
        "unexpected dirty-project response: {response:#?}"
    );
    assert_eq!(response["project"]["name"], "alpha");
}

#[tokio::test]
async fn routing_errors_precede_graph_dispatch_and_blind_modules_reject_project() {
    let unavailable = indexed_repo("unavailable_symbol");
    unavailable.write(".unavailable", "marker\n");
    let catalog = tempfile::tempdir().expect("catalog tempdir");
    let module = GraphMcpModule::with_local_projects(
        GraphMcpDeps::default(),
        resolver(
            catalog.path().join("projects.toml"),
            &[("unavailable", unavailable.path())],
        ),
    );

    let unknown = module
        .dispatch(
            "not_a_graph_tool",
            json!({"project": "missing", "query": "anything"}),
        )
        .await
        .expect_err("unknown project must fail before tool dispatch")
        .into_error_response()
        .await;
    assert_eq!(unknown.code, -32004);
    assert!(unknown.message.contains("unknown local project `missing`"));

    let unavailable = module
        .dispatch(
            "not_a_graph_tool",
            json!({"project": "unavailable", "query": "anything"}),
        )
        .await
        .expect_err("unavailable project must fail before tool dispatch")
        .into_error_response()
        .await;
    assert_eq!(unavailable.code, -32004);
    assert!(unavailable
        .message
        .contains("graph artifact is unavailable"));

    let blind = GraphMcpModule::new(GraphMcpDeps::default())
        .dispatch(
            "code_symbol_search",
            json!({"project": "alpha", "query": "anything"}),
        )
        .await
        .expect_err("project-blind module must reject project")
        .into_error_response()
        .await;
    assert_eq!(blind.code, -32602);
    assert!(blind.message.contains("not available on this MCP server"));
}

#[tokio::test]
async fn concurrent_named_project_queries_do_not_leak_task_local_roots() {
    let alpha = indexed_repo("alpha_symbol");
    let beta = indexed_repo("beta_symbol");
    let catalog = tempfile::tempdir().expect("catalog tempdir");
    let module = GraphMcpModule::with_local_projects(
        GraphMcpDeps::default(),
        resolver(
            catalog.path().join("projects.toml"),
            &[("alpha", alpha.path()), ("beta", beta.path())],
        ),
    );

    let alpha_task = {
        let module = module.clone();
        tokio::spawn(async move {
            module
                .dispatch(
                    "code_symbol_search",
                    json!({"query": "alpha_symbol", "mode": "exact", "project": "alpha"}),
                )
                .await
        })
    };
    let beta_task = {
        let module = module.clone();
        tokio::spawn(async move {
            module
                .dispatch(
                    "code_symbol_search",
                    json!({"query": "beta_symbol", "mode": "exact", "project": "beta"}),
                )
                .await
        })
    };
    let (alpha_response, beta_response) = tokio::join!(alpha_task, beta_task);
    let alpha_response = alpha_response.expect("alpha task").expect("alpha response");
    let beta_response = beta_response.expect("beta task").expect("beta response");
    assert_eq!(
        alpha_response["candidates"][0]["entity_name"],
        "alpha_symbol"
    );
    assert_eq!(alpha_response["project"]["name"], "alpha");
    assert_eq!(beta_response["candidates"][0]["entity_name"], "beta_symbol");
    assert_eq!(beta_response["project"]["name"], "beta");
}

#[test]
fn graph_readiness_probe_requires_an_existing_usable_artifact() {
    let indexed = indexed_repo("ready_symbol");
    ensure_graph_artifact_ready(indexed.path()).expect("indexed graph is ready");

    let unindexed = GitRepo::new();
    let graph_dir = unindexed.path().join(".spur/graph");
    assert!(!graph_dir.exists());
    let error = ensure_graph_artifact_ready(unindexed.path()).expect_err("graph must be indexed");
    assert!(error.to_string().contains("graph artifact"), "{error:#}");
    assert!(
        !graph_dir.exists(),
        "readiness probing must not create graph state"
    );

    let corrupt = GitRepo::new();
    let corrupt_artifact = corrupt.path().join(".spur/graph/corrupt");
    std::fs::create_dir_all(&corrupt_artifact).expect("create corrupt artifact directory");
    write_current_pointer(corrupt.path(), &corrupt_artifact).expect("write corrupt pointer");
    let error =
        ensure_graph_artifact_ready(corrupt.path()).expect_err("corrupt graph must not be ready");
    assert!(error.to_string().contains("graph artifact"), "{error:#}");
    assert!(corrupt_artifact.exists());
}
