//! Generic MCP server transport helpers.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{body::Body, Router};
use hyper::body::Incoming;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
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
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tower::ServiceExt as _;
use tracing::debug;

pub mod registry_server;
pub use registry_server::{json_rpc_to_call_tool_result, RegistryServerHandler};

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
    /// Cancels every admitted HTTP connection without waiting for protocol
    /// graceful shutdown.
    pub force_shutdown: CancellationToken,
    /// Owns every accepted connection task. Once the root task has stopped
    /// accepting, `close` + `wait` is a transitive shutdown barrier.
    pub connection_tasks: TaskTracker,
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
    let graceful_shutdown = CancellationToken::new();
    let force_shutdown = CancellationToken::new();
    let connection_tasks = TaskTracker::new();
    let graceful_shutdown_for_root = graceful_shutdown.clone();
    let force_shutdown_for_root = force_shutdown.clone();
    let connection_tasks_for_root = connection_tasks.clone();
    let root_handle = tokio::spawn(async move {
        // If the root owner itself is aborted, force every independently
        // spawned connection to drop its Hyper driver instead of detaching.
        let _force_connections_on_drop = force_shutdown_for_root.clone().drop_guard();
        let mut shutdown_rx = shutdown_rx;
        let listener = bound.listener;
        let router = bound.router;

        let forced = loop {
            tokio::select! {
                biased;
                () = force_shutdown_for_root.cancelled() => break true,
                _ = &mut shutdown_rx => break false,
                accepted = listener.accept() => {
                    let (stream, remote_addr) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            debug!(%error, "RMCP streamable HTTP accept failed");
                            continue;
                        }
                    };
                    let tower_service = router.clone().map_request(
                        |request: hyper::Request<Incoming>| request.map(Body::new),
                    );
                    let graceful = graceful_shutdown_for_root.clone();
                    let force = force_shutdown_for_root.clone();
                    connection_tasks_for_root.spawn(async move {
                        let io = TokioIo::new(stream);
                        let hyper_service = TowerToHyperService::new(tower_service);
                        let mut builder = Builder::new(TokioExecutor::new());
                        builder.http2().enable_connect_protocol();
                        let connection = builder
                            .serve_connection_with_upgrades(io, hyper_service);
                        tokio::pin!(connection);

                        let result = tokio::select! {
                            biased;
                            () = force.cancelled() => None,
                            result = &mut connection => Some(result),
                            () = graceful.cancelled() => {
                                connection.as_mut().graceful_shutdown();
                                tokio::select! {
                                    biased;
                                    () = force.cancelled() => None,
                                    result = &mut connection => Some(result),
                                }
                            }
                        };
                        if let Some(Err(error)) = result {
                            debug!(%error, ?remote_addr, "RMCP HTTP connection exited");
                        }
                    });
                }
            }
        };

        if forced {
            force_shutdown_for_root.cancel();
        } else {
            graceful_shutdown_for_root.cancel();
        }
        drop(listener);
        drop(router);
        connection_tasks_for_root.close();
        connection_tasks_for_root.wait().await;

        // The callback owns server-specific children (for example the root
        // signal watcher) and must acknowledge their cancellation before the
        // root task reports completion, including on force.
        on_server_stopped.await;
        let _ = done_tx.send(());
    });

    StreamableHttpServerTask {
        url: bound.url,
        shutdown_tx,
        root_handle,
        done_rx,
        force_shutdown,
        connection_tasks,
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
