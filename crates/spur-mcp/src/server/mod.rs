//! Generic MCP server transport helpers.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use rmcp::{
    self,
    service::{serve_server, RoleServer, Service},
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::debug;

pub mod registry_server;
pub use registry_server::RegistryServerHandler;

/// Idle-session watchdog for SPUR's streamable-HTTP MCP transports.
///
/// rmcp's `SessionConfig::DEFAULT_KEEP_ALIVE` is 5 min, which is far too short
/// for brain↔spur sessions where a brain agent commonly idles between user
/// turns (lunch, overnight, parallel work in another window). When the watchdog
/// fires, the worker quits and rmcp's tower layer logs a cascading
/// `Failed to close session ... Session service terminated` ERROR. 4 hours
/// preserves cleanup of truly-orphaned sessions while accommodating realistic
/// idle gaps. Override via `SPUR_MCP_SESSION_KEEPALIVE_SECS` (env var, secs;
/// `0` disables the watchdog entirely).
pub const MCP_SESSION_KEEPALIVE_DEFAULT: Duration = Duration::from_secs(4 * 60 * 60);

pub fn mcp_session_keepalive() -> Option<Duration> {
    match std::env::var("SPUR_MCP_SESSION_KEEPALIVE_SECS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(_) => Some(MCP_SESSION_KEEPALIVE_DEFAULT),
        },
        Err(_) => Some(MCP_SESSION_KEEPALIVE_DEFAULT),
    }
}

#[derive(Clone, Debug)]
pub struct StreamableHttpTransportConfig {
    pub bind_addr: SocketAddr,
    pub path: String,
    pub stateful_mode: bool,
    pub keep_alive: Option<Duration>,
}

impl Default for StreamableHttpTransportConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            path: "/mcp".to_owned(),
            stateful_mode: true,
            keep_alive: mcp_session_keepalive(),
        }
    }
}

pub struct StreamableHttpServerTask {
    pub url: String,
    pub shutdown_tx: oneshot::Sender<()>,
    pub root_handle: JoinHandle<()>,
    pub done_rx: oneshot::Receiver<()>,
}

pub struct BoundStreamableHttpServer {
    pub url: String,
    listener: TcpListener,
    router: Router,
}

pub async fn bind_streamable_http_server<S, F>(
    service_factory: F,
    config: StreamableHttpTransportConfig,
) -> Result<BoundStreamableHttpServer>
where
    S: Service<RoleServer> + Send + 'static,
    F: Fn() -> Result<S, std::io::Error> + Send + Sync + 'static,
{
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .context("Failed to bind TCP listener")?;
    let addr = listener.local_addr()?;
    let path = normalize_transport_path(config.path);
    let url = format!("http://{addr}{path}");

    let mut rmcp_config = StreamableHttpServerConfig::default();
    rmcp_config.stateful_mode = config.stateful_mode;
    let mut session_manager_inner = LocalSessionManager::default();
    session_manager_inner.session_config.keep_alive = config.keep_alive;
    let session_manager = Arc::new(session_manager_inner);
    let service = StreamableHttpService::new(service_factory, session_manager, rmcp_config);
    let router = Router::new().nest_service(&path, service);

    Ok(BoundStreamableHttpServer {
        url,
        listener,
        router,
    })
}

pub fn serve_streamable_http_server<C>(
    bound: BoundStreamableHttpServer,
    on_server_stopped: C,
) -> StreamableHttpServerTask
where
    C: Future<Output = ()> + Send + 'static,
{
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let root_handle = tokio::spawn(async move {
        if let Err(error) = axum::serve(bound.listener, bound.router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
        {
            debug!(%error, "RMCP streamable HTTP server exited");
        }
        on_server_stopped.await;
        let _ = done_tx.send(());
    });

    StreamableHttpServerTask {
        url: bound.url,
        shutdown_tx,
        root_handle,
        done_rx,
    }
}

pub async fn start_streamable_http_server<S, F, C>(
    service_factory: F,
    config: StreamableHttpTransportConfig,
    on_server_stopped: C,
) -> Result<StreamableHttpServerTask>
where
    S: Service<RoleServer> + Send + 'static,
    F: Fn() -> Result<S, std::io::Error> + Send + Sync + 'static,
    C: Future<Output = ()> + Send + 'static,
{
    let bound = bind_streamable_http_server(service_factory, config).await?;
    Ok(serve_streamable_http_server(bound, on_server_stopped))
}

fn normalize_transport_path(path: String) -> String {
    if path.is_empty() {
        return "/".to_owned();
    }
    if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    }
}

/// Serve an rmcp server over the process's stdin/stdout.
///
/// This is the transport standalone MCP servers use when launched directly by
/// an MCP client (Claude Code, `OpenCode`, etc.) via `command`/`args`. The future
/// resolves when the client disconnects or the stdio streams close.
///
/// `service` is typically a [`RegistryServerHandler`] wrapping a composed
/// [`crate::ToolRegistry`], but any rmcp `ServerHandler` (which blanket-impls
/// `Service<RoleServer>`) is accepted.
pub async fn serve_stdio_server<S>(service: S) -> Result<()>
where
    S: Service<RoleServer>,
{
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let running = serve_server(service, (stdin, stdout))
        .await
        .context("failed to start stdio MCP server")?;
    running
        .waiting()
        .await
        .context("stdio MCP server exited unexpectedly")?;
    Ok(())
}
