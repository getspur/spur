//! Bounded, read-only probes of configured MCP servers.
//!
//! A probe connects to one server, completes the MCP initialization handshake,
//! and requests its full tool catalog. It never invokes a tool or changes the
//! supplied configuration.

use std::time::Duration;

use anyhow::{anyhow, Context as _};
use rmcp::{
    model::Tool,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, ConfigureCommandExt,
        StreamableHttpClientTransport, TokioChildProcess,
    },
    ServiceExt as _,
};
use spur_acp::config::{McpServerEntry, McpServerTransport};

/// Maximum duration of the default connect, initialize, and `tools/list`
/// sequence.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Tool metadata returned by an MCP server probe.
#[derive(Debug, Clone)]
pub struct ProbedTool {
    /// Tool name advertised by the server.
    pub name: String,
    /// Optional human-readable tool description.
    pub description: Option<String>,
    /// The advertised input schema serialized as a JSON object string.
    pub input_schema_json: String,
}

/// Terminal result of probing one MCP server.
#[derive(Debug, Clone)]
pub enum ProbeOutcome {
    /// Initialization and the complete paginated `tools/list` request succeeded.
    ToolsListed(Vec<ProbedTool>),
    /// Spawning, connecting, initializing, or listing tools failed.
    ConnectError(String),
    /// The probe exceeded its single end-to-end time bound.
    Timeout,
}

/// Probe result paired with the configured server name.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    /// Name copied from the probed configuration entry.
    pub server_name: String,
    /// Terminal probe outcome.
    pub outcome: ProbeOutcome,
}

/// Probe an MCP server with [`DEFAULT_PROBE_TIMEOUT`].
///
/// # Example
///
/// ```no_run
/// # async fn example(entry: &spur_acp::config::McpServerEntry) {
/// let report = spur_mcp::probe_server(entry).await;
/// println!("{}: {:?}", report.server_name, report.outcome);
/// # }
/// ```
pub async fn probe_server(entry: &McpServerEntry) -> ProbeReport {
    probe_server_with_timeout(entry, DEFAULT_PROBE_TIMEOUT).await
}

/// Probe an MCP server within one end-to-end `timeout`.
///
/// The bound includes child spawn or HTTP connection, MCP initialization, and
/// the complete paginated `tools/list` sequence. Dropping a timed-out stdio
/// probe drops rmcp's child-process guard, which kills the spawned child.
pub async fn probe_server_with_timeout(entry: &McpServerEntry, timeout: Duration) -> ProbeReport {
    let outcome = match tokio::time::timeout(timeout, probe_inner(entry)).await {
        Ok(Ok(tools)) => ProbeOutcome::ToolsListed(tools),
        Ok(Err(error)) => ProbeOutcome::ConnectError(format!("{error:#}")),
        Err(_) => ProbeOutcome::Timeout,
    };

    ProbeReport {
        server_name: entry.name.clone(),
        outcome,
    }
}

async fn probe_inner(entry: &McpServerEntry) -> anyhow::Result<Vec<ProbedTool>> {
    match &entry.transport {
        McpServerTransport::Stdio { command, args, env } => probe_stdio(command, args, env).await,
        McpServerTransport::Http { url, headers } => probe_http(url, headers).await,
    }
}

async fn probe_stdio(
    command: &str,
    args: &[String],
    env: &std::collections::HashMap<String, String>,
) -> anyhow::Result<Vec<ProbedTool>> {
    let transport =
        TokioChildProcess::new(tokio::process::Command::new(command).configure(|child| {
            child.args(args).envs(env).kill_on_drop(true);
        }))
        .with_context(|| format!("failed to spawn MCP server command '{command}'"))?;
    let client = ()
        .serve(transport)
        .await
        .context("failed to initialize MCP client over the configured stdio transport")?;
    let tools = client
        .list_all_tools()
        .await
        .context("failed to list tools from MCP server")?;
    convert_tools(tools)
}

async fn probe_http(
    url: &str,
    headers: &std::collections::HashMap<String, String>,
) -> anyhow::Result<Vec<ProbedTool>> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    let mut header_entries = headers.iter().collect::<Vec<_>>();
    header_entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (name, value) in header_entries {
        let name = name
            .parse()
            .map_err(|error| anyhow!("invalid MCP HTTP header name '{name}': {error}"))?;
        let value = value
            .parse()
            .map_err(|error| anyhow!("invalid MCP HTTP header value: {error}"))?;
        config.custom_headers.insert(name, value);
    }

    let transport = StreamableHttpClientTransport::from_config(config);
    let client = ()
        .serve(transport)
        .await
        .context("failed to initialize MCP client over the configured HTTP transport")?;
    let tools = client
        .list_all_tools()
        .await
        .context("failed to list tools from MCP server")?;
    convert_tools(tools)
}

fn convert_tools(tools: Vec<Tool>) -> anyhow::Result<Vec<ProbedTool>> {
    tools
        .into_iter()
        .map(|tool| {
            Ok(ProbedTool {
                name: tool.name.into_owned(),
                description: tool.description.map(|description| description.into_owned()),
                input_schema_json: serde_json::to_string(&tool.input_schema)
                    .context("failed to serialize MCP tool input schema")?,
            })
        })
        .collect()
}
