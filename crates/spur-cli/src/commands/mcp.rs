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
//! The bundled `spur mcp` server is a read-only query surface (code graph +
//! analyst); orchestration tools (delegation, plan, review) stay bound to the
//! TUI/orchestrator because they need an active brain session and PM service.
//!
//! Logging: these commands run in non-TUI mode, so `init_tracing` directs all
//! log output to **stderr**, keeping stdout free for the JSON-RPC stream.

use anyhow::Result;

use spur_analyst::mcp::AnalystMcpModule;
use spur_graph::mcp::{GraphMcpDeps, GraphMcpModule};
use spur_mcp::{serve_stdio_server, RegistryServerHandler, ToolRegistry};

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

/// `spur graph mcp` — standalone code-graph MCP server (the 9 `code_*` tools).
pub async fn run_graph_server() -> Result<()> {
    let registry = ToolRegistry::builder()
        .with(GraphMcpModule::new(GraphMcpDeps::default()))?
        .with_alias("code_search", "code_symbol_search")?
        .build();
    let handler = RegistryServerHandler::new(registry, "spur-graph-mcp", GRAPH_INSTRUCTIONS);
    serve_stdio_server(handler).await
}

/// `spur analyst mcp` — standalone analyst MCP server
/// (`knowledge_context_pack`, `knowledge_context_pack_2`, `doc_navigate`).
pub async fn run_analyst_server() -> Result<()> {
    let registry = ToolRegistry::builder()
        .with(AnalystMcpModule::new())?
        .build();
    let handler = RegistryServerHandler::new(registry, "spur-analyst-mcp", ANALYST_INSTRUCTIONS);
    serve_stdio_server(handler).await
}

/// `spur mcp` — bundled read-only MCP server (graph + analyst).
pub async fn run_bundled_server() -> Result<()> {
    let registry = ToolRegistry::builder()
        .with(GraphMcpModule::new(GraphMcpDeps::default()))?
        .with(AnalystMcpModule::new())?
        .with_alias("code_search", "code_symbol_search")?
        .build();
    let handler = RegistryServerHandler::new(registry, "spur-mcp", BUNDLED_INSTRUCTIONS);
    serve_stdio_server(handler).await
}
