//! `spur mcp`, `spur graph mcp`, and `spur analyst mcp` — standalone MCP
//! servers launched directly from the `spur` CLI.
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
//!     "spur":         { "command": "spur", "args": ["mcp"] }
//! }}
//! ```
//!
//! Pass `--root <path>` in any of those `args` arrays to bind every tool call
//! to a specific worktree for the server lifetime. If `--root` is omitted,
//! `SPUR_WORKTREE` is used as a fallback; if neither is set, tools keep the
//! existing behavior of resolving from the MCP client launch directory.
//!
//! The bundled `spur mcp` server is a read-only query surface (code graph +
//! analyst); orchestration tools (delegation, plan, review) stay bound to the
//! TUI/orchestrator because they need an active brain session and PM service.
//!
//! Logging: these commands run in non-TUI mode, so `init_tracing` directs all
//! log output to **stderr**, keeping stdout free for the JSON-RPC stream.

use std::future::Future;
use std::path::PathBuf;

use anyhow::{Context, Result};

use spur_analyst::mcp::AnalystMcpModule;
use spur_graph::mcp::{GraphMcpDeps, GraphMcpModule};
use spur_mcp::{serve_stdio_server, RegistryServerHandler, ToolRegistry};

const SPUR_WORKTREE_ENV: &str = "SPUR_WORKTREE";

const GRAPH_INSTRUCTIONS: &str =
    "Tree-sitter code-graph query tools over the current worktree: resolve/search/read symbols, \
     list callers/callees, map neighborhoods, and trace symbol history. Graph-first code \
     navigation — prefer these over grep/glob.";

const ANALYST_INSTRUCTIONS: &str =
    "DuckDB-backed analytics over the .spur/analyst.duckdb index: knowledge_context_pack for \
     one-shot oriented evidence, and doc_navigate for documentation section search. Use these for \
     ranked/aggregated/path-shaped answers over code and docs.";

const BUNDLED_INSTRUCTIONS: &str =
    "SPUR standalone query surface: code-graph tools (code_*) plus DuckDB analyst tools \
     (knowledge_context_pack*, doc_navigate). Read-only. For orchestration (delegation, plans, \
     review) run the SPUR TUI.";

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

/// `spur graph mcp` — standalone code-graph MCP server (the 9 `code_*` tools).
///
/// `root` is the optional `--root <path>` override. When absent, `SPUR_WORKTREE`
/// is honored before falling back to the MCP client launch directory.
pub async fn run_graph_server(root: Option<PathBuf>) -> Result<()> {
    let registry = ToolRegistry::builder()
        .with(GraphMcpModule::new(GraphMcpDeps::default()))?
        .with_alias("code_search", "code_symbol_search")?
        .build();
    let handler = RegistryServerHandler::new(registry, "spur-graph-mcp", GRAPH_INSTRUCTIONS);
    Box::pin(with_mcp_worktree_scope(root, serve_stdio_server(handler))).await?
}

/// `spur analyst mcp` — standalone analyst MCP server
/// (`knowledge_context_pack`, `knowledge_context_pack_2`, `doc_navigate`).
///
/// `root` is the optional `--root <path>` override. When absent, `SPUR_WORKTREE`
/// is honored before falling back to the MCP client launch directory.
pub async fn run_analyst_server(root: Option<PathBuf>) -> Result<()> {
    let registry = ToolRegistry::builder()
        .with(AnalystMcpModule::new())?
        .build();
    let handler = RegistryServerHandler::new(registry, "spur-analyst-mcp", ANALYST_INSTRUCTIONS);
    Box::pin(with_mcp_worktree_scope(root, serve_stdio_server(handler))).await?
}

/// `spur mcp` — bundled read-only MCP server (graph + analyst).
///
/// `root` is the optional `--root <path>` override. When absent, `SPUR_WORKTREE`
/// is honored before falling back to the MCP client launch directory.
pub async fn run_bundled_server(root: Option<PathBuf>) -> Result<()> {
    let registry = ToolRegistry::builder()
        .with(GraphMcpModule::new(GraphMcpDeps::default()))?
        .with(AnalystMcpModule::new())?
        .with_alias("code_search", "code_symbol_search")?
        .build();
    let handler = RegistryServerHandler::new(registry, "spur-mcp", BUNDLED_INSTRUCTIONS);
    Box::pin(with_mcp_worktree_scope(root, serve_stdio_server(handler))).await?
}

#[cfg(test)]
mod tests {
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
}
