//! `spur mcp`, `spur graph mcp`, `spur analyst mcp`, and `spur context mcp` —
//! standalone MCP servers launched directly from the `spur` CLI.
//!
//! Each builds a [`spur_mcp::ToolRegistry`] from one or more domain
//! `ToolModule`s, wraps it in a [`RegistryServerHandler`], and serves it over
//! stdio so an MCP client (Claude Code, OpenCode, …) can launch it via
//! `command`/`args`:
//!
//! ```jsonc
//! { "mcpServers": {
//!     "spur-graph":   { "command": "spur", "args": ["graph", "mcp"] },
//!     "spur-analyst": { "command": "spur", "args": ["analyst", "mcp"] },
//!     "spur-context": { "command": "spur", "args": ["context", "mcp", "--url", "..."] },
//!     "spur":         { "command": "spur", "args": ["mcp"] }
//! }}
//! ```
//!
//! Pass `--root <path>` in any of those `args` arrays to bind every tool call
//! to a specific worktree for the server lifetime. If `--root` is omitted,
//! `SPUR_WORKTREE` is used as a fallback; if neither is set, tools keep the
//! existing behavior of resolving from the MCP client launch directory.
//!
//! The bundled `spur mcp` server keeps repository and index queries read-only.
//! Its local-project add/remove tools mutate only user catalog configuration;
//! orchestration tools (delegation, plan, review) stay bound to the
//! TUI/orchestrator because they need an active brain session and PM service.
//!
//! Logging: these commands run in non-TUI mode, so `init_tracing` directs all
//! log output to **stderr**, keeping stdout free for the JSON-RPC stream.

use std::future::Future;
use std::path::PathBuf;

use anyhow::{Context, Result};

use spur_analyst::mcp::AnalystMcpModule;
use spur_core::mcp::{ContextServiceAuth, ContextServiceClient, LocalProjectMcpComposition};
use spur_graph::mcp::{GraphMcpDeps, GraphMcpModule};
use spur_mcp::{serve_stdio_server, RegistryServerHandler, ToolRegistry};

const SPUR_WORKTREE_ENV: &str = "SPUR_WORKTREE";

const GRAPH_INSTRUCTIONS: &str =
    "Tree-sitter code-graph query tools over the current worktree: resolve/search/read symbols, \
     list callers/callees, map neighborhoods, and trace symbol history. Graph-first code \
     navigation — prefer these over grep/glob. Repository and index query operations are read-only; \
     local_project_add and local_project_remove mutate only user catalog configuration. Register an \
     already-indexed local Git project once and pass its name as project; registration validates but \
     does not index. external_* tools are the separate hosted package/revision surface.";

const ANALYST_INSTRUCTIONS: &str =
    "DuckDB-backed analytics over the .spur/analyst.duckdb index: knowledge_context_pack for \
     one-shot oriented evidence, and doc_navigate for documentation section search. Use these for \
     ranked/aggregated/path-shaped answers over code and docs. Repository and index query operations \
     are read-only; local_project_add and local_project_remove mutate only user catalog configuration. \
     Register an already-indexed local Git project once and pass its name as project; registration \
     validates but does not index. external_* tools are the separate hosted package/revision surface.";

const CONTEXT_INSTRUCTIONS: &str =
    "Cloud-backed external code-context tools (external_*): search/read indexed packages, inspect \
     callers/callees, query external knowledge context, and manage external indexes.";

const BUNDLED_INSTRUCTIONS: &str =
    "SPUR standalone query surface: code-graph tools (code_*) plus DuckDB analyst tools \
     (knowledge_context_pack*, doc_navigate). Repository and index query operations are read-only; \
     local_project_add and local_project_remove mutate only user catalog configuration. For \
     orchestration (delegation, plans, review) run the SPUR TUI. Register an already-indexed local \
     Git project once and pass its name as project; registration validates but does not index. \
     external_* tools remain the separate hosted package/revision surface.";

fn resolve_mcp_worktree_root(root: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let root = root.or_else(|| {
        std::env::var_os(SPUR_WORKTREE_ENV).and_then(|value| {
            if value.is_empty() {
                None
            } else {
                Some(PathBuf::from(value))
            }
        })
    });
    root.map(|root| {
        root.canonicalize()
            .with_context(|| format!("failed to canonicalize root `{}`", root.display()))
    })
    .transpose()
}

async fn with_mcp_worktree_scope<F, T>(root: Option<PathBuf>, future: F) -> Result<T>
where
    F: Future<Output = T>,
{
    match resolve_mcp_worktree_root(root)? {
        Some(root) => Ok(spur_graph::mcp::with_worktree_root_for_request(root, future).await),
        None => Ok(future.await),
    }
}

fn local_project_composition() -> LocalProjectMcpComposition {
    LocalProjectMcpComposition::from_environment()
}

fn graph_server_registry(local_projects: &LocalProjectMcpComposition) -> Result<ToolRegistry> {
    Ok(ToolRegistry::builder()
        .with(local_projects.catalog_module())?
        .with(GraphMcpModule::with_local_projects(
            GraphMcpDeps::default(),
            local_projects.resolver(),
        ))?
        .with_alias("code_search", "code_symbol_search")?
        .build())
}

fn analyst_server_registry(local_projects: &LocalProjectMcpComposition) -> Result<ToolRegistry> {
    Ok(ToolRegistry::builder()
        .with(local_projects.catalog_module())?
        .with(AnalystMcpModule::with_local_projects_for_analyst_server(
            local_projects.resolver(),
        ))?
        .build())
}

fn bundled_server_registry(local_projects: &LocalProjectMcpComposition) -> Result<ToolRegistry> {
    Ok(ToolRegistry::builder()
        .with(local_projects.catalog_module())?
        .with(GraphMcpModule::with_local_projects(
            GraphMcpDeps::default(),
            local_projects.resolver(),
        ))?
        .with(AnalystMcpModule::with_local_projects(
            local_projects.resolver(),
        ))?
        .with_alias("code_search", "code_symbol_search")?
        .build())
}

/// `spur graph mcp` — standalone code-graph MCP server (the 9 `code_*` tools).
///
/// `root` is the optional `--root <path>` override. When absent, `SPUR_WORKTREE`
/// is honored before falling back to the MCP client launch directory.
pub async fn run_graph_server(root: Option<PathBuf>) -> Result<()> {
    let local_projects = local_project_composition();
    let registry = graph_server_registry(&local_projects)?;
    let handler = RegistryServerHandler::new(registry, "spur-graph-mcp", GRAPH_INSTRUCTIONS);
    Box::pin(with_mcp_worktree_scope(root, serve_stdio_server(handler))).await?
}

/// `spur analyst mcp` — standalone analyst MCP server
/// (`knowledge_context_pack`, `knowledge_context_pack_2`, `doc_navigate`).
///
/// `root` is the optional `--root <path>` override. When absent, `SPUR_WORKTREE`
/// is honored before falling back to the MCP client launch directory.
pub async fn run_analyst_server(root: Option<PathBuf>) -> Result<()> {
    spur_analyst::mcp::warm_embed_model();

    let local_projects = local_project_composition();
    let registry = analyst_server_registry(&local_projects)?;
    let handler = RegistryServerHandler::new(registry, "spur-analyst-mcp", ANALYST_INSTRUCTIONS);
    Box::pin(with_mcp_worktree_scope(root, serve_stdio_server(handler))).await?
}

fn context_server_registry(url: String, auth: ContextServiceAuth) -> Result<ToolRegistry> {
    Ok(ToolRegistry::builder()
        .with(ContextServiceClient::new(url, auth)?)?
        .build())
}

fn legacy_context_server_registry(url: String, token: String) -> Result<ToolRegistry> {
    Ok(ToolRegistry::builder()
        .with(ContextServiceClient::with_optional_token(url, Some(token))?)?
        .build())
}

/// `spur context mcp` — standalone external code-context MCP server.
///
/// Exposes the 7 `external_*` tools (external_code_search, external_code_read,
/// external_code_callers, external_code_callees, external_knowledge_context,
/// external_index, external_index_status) as a spec-compliant MCP stdio server.
/// Tools proxy to the cloud spur-context-service Lambda.
pub async fn run_context_server(url: String, auth: ContextServiceAuth) -> Result<()> {
    let registry = context_server_registry(url, auth)?;
    let handler = RegistryServerHandler::new(registry, "spur-context-mcp", CONTEXT_INSTRUCTIONS);
    serve_stdio_server(handler).await
}

/// Runs the context MCP proxy while preserving the configured legacy route.
pub async fn run_legacy_context_server(url: String, token: String) -> Result<()> {
    let registry = legacy_context_server_registry(url, token)?;
    let handler = RegistryServerHandler::new(registry, "spur-context-mcp", CONTEXT_INSTRUCTIONS);
    serve_stdio_server(handler).await
}

/// `spur mcp` — bundled MCP server with read-only repository/index queries and
/// user catalog configuration tools.
///
/// `root` is the optional `--root <path>` override. When absent, `SPUR_WORKTREE`
/// is honored before falling back to the MCP client launch directory.
pub async fn run_bundled_server(root: Option<PathBuf>) -> Result<()> {
    spur_analyst::mcp::warm_embed_model();

    let local_projects = local_project_composition();
    let registry = bundled_server_registry(&local_projects)?;
    let handler = RegistryServerHandler::new(registry, "spur-mcp", BUNDLED_INSTRUCTIONS);
    Box::pin(with_mcp_worktree_scope(root, serve_stdio_server(handler))).await?
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use spur_core::mcp::LocalProjectMcpComposition;
    use spur_graph::{
        artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer,
        WriteOptions,
    };
    use spur_mcp::local_projects::LocalProjectCatalogStore;
    use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext};

    fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("missing function signature: {signature}"));
        let tail = &source[start..];
        let next_fn = tail
            .find("\npub async fn ")
            .filter(|offset| *offset > 0)
            .unwrap_or(tail.len());
        &tail[..next_fn]
    }

    #[test]
    fn standalone_query_servers_pre_warm_kcp_embeddings() {
        let source = include_str!("mcp.rs");

        for signature in [
            "pub async fn run_analyst_server",
            "pub async fn run_bundled_server",
        ] {
            let body = function_body(source, signature);
            assert!(
                body.contains("spur_analyst::mcp::warm_embed_model();"),
                "{signature} must pre-warm knowledge_context_pack embeddings"
            );
        }
    }

    #[tokio::test]
    async fn worktree_scope_wraps_entire_server_future() {
        let root = std::env::current_dir()
            .expect("current dir")
            .canonicalize()
            .expect("canonical root");
        let expected = spur_graph::resolve_worktree_root_from(root.clone());

        let scoped = super::with_mcp_worktree_scope(Some(root), async {
            tokio::task::yield_now().await;
            spur_graph::mcp::scoped_worktree_root()
        })
        .await
        .expect("scoped server future");

        assert_eq!(scoped, Some(expected));
    }

    #[tokio::test]
    async fn omitted_project_dispatches_registry_call_against_outer_root() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        std::fs::create_dir_all(repo.path().join("src")).expect("create source directory");
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn selected_by_outer_root() {}\n",
        )
        .expect("write source");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "SPUR Test"][..],
            &["add", "src/lib.rs"][..],
            &["commit", "-q", "-m", "fixture"][..],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        }
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

        let catalog = tempfile::tempdir().expect("catalog tempdir");
        let local_projects = LocalProjectMcpComposition::new(LocalProjectCatalogStore::new(
            catalog.path().join("projects.toml"),
        ));
        let registry = super::graph_server_registry(&local_projects).expect("graph registry");
        let response = super::with_mcp_worktree_scope(Some(repo.path().to_path_buf()), async {
            registry
                .call_json_tool(
                    ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None),
                    "code_symbol_search",
                    json!({"query": "selected_by_outer_root", "mode": "exact"}),
                )
                .await
        })
        .await
        .expect("outer worktree scope");
        let response = serde_json::to_value(response).expect("serialize response");
        let body = response["result"]["content"][0]["text"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or_else(|| panic!("unexpected registry response: {response:#?}"));
        assert_eq!(
            body["candidates"][0]["entity_name"],
            "selected_by_outer_root"
        );
        assert!(body.get("project").is_none());
    }

    #[test]
    fn context_server_registry_exposes_external_tools() {
        let registry = super::context_server_registry(
            "https://context.example.test".to_owned(),
            super::ContextServiceAuth::None,
        )
        .expect("context registry should build");
        let tools = registry.list_tools();
        let names: std::collections::BTreeSet<_> =
            tools.iter().map(|tool| tool.name.as_str()).collect();

        assert_eq!(tools.len(), 8);
        assert!(names.contains("external_catalog"));
        assert!(names.contains("external_code_search"));
        assert!(names.contains("external_code_read"));
        assert!(names.contains("external_code_callers"));
        assert!(names.contains("external_code_callees"));
        assert!(names.contains("external_knowledge_context"));
        assert!(names.contains("external_index"));
        assert!(names.contains("external_index_status"));
    }

    #[test]
    fn standalone_query_registries_expose_named_local_projects() {
        let catalog = tempfile::tempdir().expect("catalog tempdir");
        let local_projects = LocalProjectMcpComposition::new(LocalProjectCatalogStore::new(
            catalog.path().join("projects.toml"),
        ));
        let graph = super::graph_server_registry(&local_projects).expect("graph registry");
        let analyst = super::analyst_server_registry(&local_projects).expect("analyst registry");
        let bundled = super::bundled_server_registry(&local_projects).expect("bundled registry");

        for (name, registry) in [
            ("graph", &graph),
            ("analyst", &analyst),
            ("bundled", &bundled),
        ] {
            let tools = registry.list_tools();
            for management in [
                "local_project_add",
                "local_project_list",
                "local_project_remove",
            ] {
                assert!(
                    tools.iter().any(|tool| tool.name == management),
                    "{name} registry missing {management}"
                );
            }
        }

        assert_project_schema(&graph, "code_symbol_search");
        assert_project_schema(&analyst, "query");
        assert_project_schema(&bundled, "code_symbol_search");
        assert_project_schema(&bundled, "knowledge_context_pack_2");
        assert_eq!(
            bundled
                .canonical_name_for_call("code_search")
                .expect("bundled alias"),
            "code_symbol_search"
        );
    }

    #[test]
    fn analyst_only_registry_selects_same_surface_followup_policy() {
        let source = include_str!("mcp.rs");
        let analyst_start = source
            .find("fn analyst_server_registry")
            .expect("analyst registry function");
        let bundled_start = source
            .find("fn bundled_server_registry")
            .expect("bundled registry function");
        let analyst_body = &source[analyst_start..bundled_start];

        assert!(
            analyst_body.contains("with_local_projects_for_analyst_server"),
            "standalone analyst composition must suppress graph-only follow-ups"
        );
    }

    #[test]
    fn standalone_instructions_explain_local_registration_boundary() {
        for (name, instructions) in [
            ("graph", super::GRAPH_INSTRUCTIONS),
            ("analyst", super::ANALYST_INSTRUCTIONS),
            ("bundled", super::BUNDLED_INSTRUCTIONS),
        ] {
            assert!(instructions.contains("already-indexed"), "{name}");
            assert!(instructions.contains("does not index"), "{name}");
            assert!(instructions.contains("external_*"), "{name}");
            assert!(
                instructions.contains("Repository and index query operations are read-only"),
                "{name}"
            );
            assert!(instructions.contains("local_project_add"), "{name}");
            assert!(instructions.contains("local_project_remove"), "{name}");
            assert!(
                instructions.contains("user catalog configuration"),
                "{name}"
            );
        }
    }

    fn assert_project_schema(registry: &spur_mcp::ToolRegistry, tool_name: &str) {
        let tool = registry
            .list_tools()
            .into_iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("missing {tool_name}"));
        assert_eq!(tool.input_schema["properties"]["project"]["type"], "string");
    }
}
